//! Brokerage compute: turn the holdings ledger + wallet cash into a valued snapshot
//! (pricing each position via the historical stock-price cache and converting into the
//! account's currency), snapshot it into a valuation, and backfill a daily valuation
//! series across the account's whole history. Persistence lives in `sure_dal::brokerage`
//! / `sure_dal::valuations`; this module is the price-lookup + FX orchestration, matching
//! the `price_at`/`StockPriceTask` split in `crate::stock_prices`.

use chrono::{NaiveDate, Utc};

use sure_core::{AppError, AppResult, BrokerageSnapshot, Position, WalletBalance};
use sure_dal::stock_prices::StockPrice;
use sure_dal::Db;
use sure_providers::StockPriceProvider;

use crate::fx::Fx;

/// Resolve a position's price as of `as_of`. With a provider, backfill the cache from it
/// on a miss (the live endpoints want fresh data); without one, read cache-only (the
/// day-by-day backfill loop, after a single bulk fetch has already warmed the cache — so
/// it never fires one upstream request per day).
async fn resolve_price(
    db: &Db,
    provider: Option<&dyn StockPriceProvider>,
    ticker: &str,
    exchange: &str,
    as_of: NaiveDate,
) -> AppResult<Option<StockPrice>> {
    match provider {
        Some(p) => crate::stock_prices::price_at(db, p, ticker, exchange, as_of).await,
        None => sure_dal::stock_prices::get_at(db, ticker, exchange, &as_of.to_string()).await,
    }
}

/// Value in minor units of holding `quantity` units at `close` (decimal text), in the
/// price's own currency (`dp` decimal places). `None` if `close` doesn't parse.
fn market_value_minor(quantity: f64, close: &str, dp: i32) -> Option<i64> {
    let close = close.parse::<f64>().ok()?;
    Some((quantity * close * 10f64.powi(dp)).round() as i64)
}

/// Compute the account's full snapshot as of `as_of`: every open position priced and
/// valued, every wallet cash balance, and a grand total converted into the account's
/// currency. See [`resolve_price`] for the `provider` semantics.
pub async fn snapshot(
    db: &Db,
    provider: Option<&dyn StockPriceProvider>,
    account_id: i64,
    as_of: NaiveDate,
) -> AppResult<BrokerageSnapshot> {
    let account = sure_dal::accounts::get(db, account_id).await?;
    let account_ccy = account.currency_code;
    let as_of_str = as_of.to_string();
    let fx = Fx::load(db, account_ccy.clone()).await?;

    let mut total_major = 0.0f64;

    let mut positions = Vec::new();
    for p in sure_dal::brokerage::positions_at(db, account_id, &as_of_str).await? {
        let price = resolve_price(db, provider, &p.ticker, &p.exchange, as_of).await?;
        let (price_text, price_as_of, value_minor) = match &price {
            Some(sp) => {
                let value = market_value_minor(p.quantity, &sp.close, fx.dp(&sp.currency_code));
                (Some(sp.close.clone()), Some(sp.as_of.clone()), value)
            }
            None => (None, None, None),
        };
        if let Some(v) = value_minor {
            // A position's price may be quoted in a different currency than the ticker's
            // listing (rare), so convert from the price's currency where we have it.
            let value_ccy = price.as_ref().map(|sp| sp.currency_code.as_str()).unwrap_or(&p.currency_code);
            total_major += fx.to_base_major(v, value_ccy);
        }
        positions.push(Position {
            ticker: p.ticker,
            exchange: p.exchange,
            name: p.name,
            currency_code: p.currency_code,
            quantity: p.quantity,
            price: price_text,
            price_as_of,
            market_value_minor: value_minor,
        });
    }

    let mut wallets = Vec::new();
    for w in sure_dal::brokerage::wallet_balances_at(db, account_id, &as_of_str).await? {
        total_major += fx.to_base_major(w.amount_minor, &w.currency_code);
        wallets.push(WalletBalance {
            currency_code: w.currency_code,
            amount_minor: w.amount_minor,
        });
    }

    Ok(BrokerageSnapshot {
        account_id,
        as_of: as_of_str,
        currency_code: account_ccy,
        positions,
        wallets,
        total_value_minor: fx.base_minor(total_major),
    })
}

/// Snapshot the account's value as of `as_of` and persist it as a `source='brokerage'`
/// valuation (upserting the day in place), so it flows into net worth. Mirrors
/// `equity::revalue`.
pub async fn revalue(
    db: &Db,
    provider: Option<&dyn StockPriceProvider>,
    account_id: i64,
    as_of: NaiveDate,
) -> AppResult<BrokerageSnapshot> {
    let snap = snapshot(db, provider, account_id, as_of).await?;
    sure_dal::valuations::upsert_from_brokerage(
        db,
        account_id,
        &snap.as_of,
        snap.total_value_minor,
        &snap.currency_code,
    )
    .await?;
    Ok(snap)
}

/// Reconstruct the account's whole net-worth history: bulk-fetch each held ticker's full
/// daily price series in one upstream call, then walk every calendar day from the
/// account's first activity to today, upserting a `source='brokerage'` valuation per day
/// from the (now warm) cache. Idempotent — safe to re-run as a retry. Returns the number
/// of days valued.
pub async fn backfill_history(
    db: &Db,
    provider: &dyn StockPriceProvider,
    account_id: i64,
) -> AppResult<usize> {
    let Some(earliest) = sure_dal::brokerage::earliest_activity_date(db, account_id).await? else {
        return Ok(0); // nothing imported yet
    };
    let Some(from) = NaiveDate::parse_from_str(&earliest, "%Y-%m-%d").ok() else {
        return Err(AppError::validation("could not parse earliest activity date"));
    };
    let today = Utc::now().date_naive();

    // One upstream call per ticker covering the full window, written through to the cache.
    for (ticker, exchange) in sure_dal::brokerage::account_tickers(db, account_id).await? {
        let exchange_hint = Some(exchange.as_str()).filter(|e| !e.is_empty());
        match provider.fetch_daily_prices(&ticker, exchange_hint, from, today).await {
            Ok(quotes) => {
                for q in &quotes {
                    // Exact decimal text, matching `stock_prices::price_at`'s own write.
                    sure_dal::stock_prices::upsert(
                        db,
                        &ticker,
                        &exchange,
                        &q.as_of.to_string(),
                        &q.close.to_string(),
                        &q.currency_code,
                    )
                    .await?;
                }
            }
            Err(err) => {
                // One bad/delisted ticker shouldn't sink the whole backfill — its position
                // simply goes unpriced for the affected days.
                tracing::warn!(ticker = %ticker, exchange = %exchange, error = %err, "brokerage backfill: price fetch failed");
            }
        }
    }

    // Day-by-day valuation from the warm cache (provider=None → no further network calls).
    let mut day = from;
    let mut valued = 0usize;
    while day <= today {
        revalue(db, None, account_id, day).await?;
        day += chrono::Duration::days(1);
        valued += 1;
    }
    tracing::info!(account_id, days = valued, "brokerage history backfilled");
    Ok(valued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use sure_core::{AccountKind, AccountMetadata, BrokerageMeta, SaveAccount};
    use sure_providers::StockPriceQuote;

    struct TempDb {
        path: std::path::PathBuf,
    }
    impl Drop for TempDb {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }
    async fn test_db() -> (Db, TempDb) {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sure-api-brokerage-test-{}-{n}.db", std::process::id()));
        let db = sure_dal::connect(&format!("sqlite:{}", path.display())).await.unwrap();
        sure_dal::migrate(&db).await.unwrap();
        (db, TempDb { path })
    }

    struct FakeProvider {
        close: Decimal,
        currency: String,
    }
    #[async_trait]
    impl StockPriceProvider for FakeProvider {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn description(&self) -> &'static str {
            "fake"
        }
        async fn fetch_daily_prices(
            &self,
            _ticker: &str,
            _exchange: Option<&str>,
            from: NaiveDate,
            _to: NaiveDate,
        ) -> anyhow::Result<Vec<StockPriceQuote>> {
            Ok(vec![StockPriceQuote {
                as_of: from,
                close: self.close,
                currency_code: self.currency.clone(),
            }])
        }
    }

    async fn brokerage_account(db: &Db, ccy: &str) -> i64 {
        sure_dal::accounts::create(
            db,
            SaveAccount {
                name: "Sharesies".to_string(),
                kind: AccountKind::Brokerage,
                currency_code: ccy.to_string(),
                institution: Some("Sharesies".to_string()),
                metadata: Some(AccountMetadata::Brokerage(BrokerageMeta::default())),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn holding(ext: &str, ticker: &str, date: &str, qty: f64) -> sure_dal::brokerage::HoldingImport {
        sure_dal::brokerage::HoldingImport {
            ticker: ticker.to_string(),
            exchange: "NZX".to_string(),
            name: Some(format!("{ticker} Ltd")),
            currency_code: "NZD".to_string(),
            trade_date: date.to_string(),
            quantity: qty,
            unit_price: Some(1.0),
            fee_minor: 0,
            kind: "buy".to_string(),
            external_id: ext.to_string(),
        }
    }
    fn wallet(ext: &str, date: &str, minor: i64, ccy: &str) -> sure_dal::providers::ImportRow {
        sure_dal::providers::ImportRow {
            external_id: ext.to_string(),
            posted_at: date.to_string(),
            amount_minor: minor,
            currency_code: Some(ccy.to_string()),
            description: "w".to_string(),
            merchant: None,
            category_name: None,
            category_group: None,
            category_kind: None,
        }
    }

    #[tokio::test]
    async fn snapshot_values_positions_and_wallet_cash() {
        let (db, _tmp) = test_db().await;
        let account_id = brokerage_account(&db, "NZD").await;
        sure_dal::brokerage::import_export(
            &db,
            account_id,
            "NZD",
            "sharesies#1",
            &[wallet("w1", "2026-01-01", 25_00, "NZD")],
            &[holding("h1", "MEL", "2026-01-02", 100.0)],
            &[],
        )
        .await
        .unwrap();

        let provider = FakeProvider { close: "5.60".parse().unwrap(), currency: "NZD".to_string() };
        let as_of = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let snap = snapshot(&db, Some(&provider), account_id, as_of).await.unwrap();

        assert_eq!(snap.positions.len(), 1);
        assert_eq!(snap.positions[0].ticker, "MEL");
        // 100 shares × $5.60 = $560.00
        assert_eq!(snap.positions[0].market_value_minor, Some(56_000));
        assert_eq!(snap.wallets.len(), 1);
        assert_eq!(snap.wallets[0].amount_minor, 25_00);
        // $560 holdings + $25 wallet cash
        assert_eq!(snap.total_value_minor, 58_500);
    }

    #[tokio::test]
    async fn backfill_writes_a_valuation_per_day_and_is_idempotent() {
        let (db, _tmp) = test_db().await;
        let account_id = brokerage_account(&db, "NZD").await;
        sure_dal::brokerage::import_export(
            &db,
            account_id,
            "NZD",
            "sharesies#1",
            &[],
            &[holding("h1", "MEL", &Utc::now().date_naive().to_string(), 10.0)],
            &[],
        )
        .await
        .unwrap();

        let provider = FakeProvider { close: "2.00".parse().unwrap(), currency: "NZD".to_string() };
        let days = backfill_history(&db, &provider, account_id).await.unwrap();
        assert_eq!(days, 1); // earliest == today
        let vals = sure_dal::valuations::list_for_account(&db, account_id).await.unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].source, "brokerage");
        assert_eq!(vals[0].value_minor, 20_00); // 10 × $2.00

        // Re-running upserts the same day rather than accumulating rows.
        backfill_history(&db, &provider, account_id).await.unwrap();
        let vals = sure_dal::valuations::list_for_account(&db, account_id).await.unwrap();
        assert_eq!(vals.len(), 1);
    }
}
