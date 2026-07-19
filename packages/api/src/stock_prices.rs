//! Stock prices: an on-demand "fetch a price at an arbitrary point in time" helper
//! that backfills the historical cache from the configured [`StockPriceProvider`] on a
//! miss, plus a [`ScheduledTask`] that keeps every ticker currently held by a shares
//! account warm on a daily cadence. Persistence is handled here, not in
//! `sure-providers` — matching the split used for exchange rates (`exchange_rates.rs`)
//! and transaction providers: the provider only fetches and normalizes.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{NaiveDate, Utc};
use sure_dal::stock_prices::StockPrice;
use sure_dal::Db;
use sure_providers::StockPriceProvider;
use sure_scheduler::ScheduledTask;
pub use sure_core::AppResult;

/// How far back to look when backfilling around a target date — comfortably spans
/// weekends and most public-holiday clusters (e.g. Christmas/New Year) so "nearest
/// preceding trading day" still finds something.
const BACKFILL_LOOKBACK_DAYS: i64 = 10;

/// Free upstream sources are daily-resolution at best, so there's no value in polling
/// more often than this (same reasoning as the exchange-rate poll).
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Resolve `ticker`'s closing price as of `as_of`, backfilling the cache from
/// `provider` on a miss. `exchange` is `SharesMeta.exchange`'s free-text value (may be
/// empty). Returns `None` if the provider has nothing for the requested range either
/// (e.g. an unrecognised ticker).
pub async fn price_at(
    db: &Db,
    provider: &dyn StockPriceProvider,
    ticker: &str,
    exchange: &str,
    as_of: NaiveDate,
) -> AppResult<Option<StockPrice>> {
    if let Some(cached) = sure_dal::stock_prices::get_at(db, ticker, exchange, &as_of.to_string()).await? {
        return Ok(Some(cached));
    }

    let from = as_of - chrono::Duration::days(BACKFILL_LOOKBACK_DAYS);
    let exchange_hint = Some(exchange).filter(|e| !e.is_empty());
    let quotes = provider
        .fetch_daily_prices(ticker, exchange_hint, from, as_of)
        .await?;
    for quote in &quotes {
        sure_dal::stock_prices::upsert(
            db,
            ticker,
            exchange,
            &quote.as_of.to_string(),
            &quote.close.to_string(),
            &quote.currency_code,
        )
        .await?;
    }

    sure_dal::stock_prices::get_at(db, ticker, exchange, &as_of.to_string()).await
}

pub struct StockPriceTask {
    db: Db,
    provider: Arc<dyn StockPriceProvider>,
}

impl StockPriceTask {
    pub fn new(db: Db, provider: Arc<dyn StockPriceProvider>) -> Self {
        Self { db, provider }
    }
}

#[async_trait]
impl ScheduledTask for StockPriceTask {
    fn name(&self) -> &'static str {
        "stock_price_poll"
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self) -> anyhow::Result<()> {
        // Single-ticker Shares accounts plus every ticker held across any Brokerage
        // account — deduped, so a symbol held both ways is only fetched once.
        let mut seen = std::collections::HashSet::new();
        let tickers: Vec<_> = sure_dal::accounts::list_shares_tickers(&self.db)
            .await?
            .into_iter()
            .chain(sure_dal::accounts::list_brokerage_tickers(&self.db).await?)
            .filter(|t| seen.insert((t.ticker.clone(), t.exchange.clone())))
            .collect();
        let today = Utc::now().date_naive();
        let from = today - chrono::Duration::days(BACKFILL_LOOKBACK_DAYS);

        let mut refreshed = 0;
        for t in &tickers {
            let exchange_hint = Some(t.exchange.as_str()).filter(|e| !e.is_empty());
            let quotes = match self
                .provider
                .fetch_daily_prices(&t.ticker, exchange_hint, from, today)
                .await
            {
                Ok(quotes) => quotes,
                Err(err) => {
                    // One bad/delisted ticker shouldn't block the rest — unlike the
                    // exchange-rate poll (one call covers every currency at once), this
                    // task loops per-symbol, so it needs its own resilience here.
                    tracing::warn!(ticker = %t.ticker, exchange = %t.exchange, error = %err, "failed to fetch stock price");
                    continue;
                }
            };
            for quote in &quotes {
                sure_dal::stock_prices::upsert(
                    &self.db,
                    &t.ticker,
                    &t.exchange,
                    &quote.as_of.to_string(),
                    &quote.close.to_string(),
                    &quote.currency_code,
                )
                .await?;
            }
            refreshed += 1;
        }
        tracing::info!(tickers = tickers.len(), refreshed, "refreshed stock prices");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::{AccountKind, AccountMetadata, SaveAccount, SharesMeta};
    use sure_providers::StockPriceQuote;

    // `sure-api` deliberately has no direct `sqlx` dependency (see the crate's
    // architecture doc), so — unlike `sure-dal`'s own tests — a shared-connection
    // `sqlite::memory:` pool isn't an option here (a >1-connection pool would give each
    // connection its own empty in-memory database). A uniquely-named temp file, opened
    // through the same `sure_dal::connect`/`migrate` every production caller uses,
    // sidesteps that without adding a new dependency just for tests; `TempDb`'s `Drop`
    // removes it (plus its WAL/SHM sidecars) so repeated runs don't pile up files.
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
        let path = std::env::temp_dir().join(format!("sure-api-stock-prices-test-{}-{n}.db", std::process::id()));
        let db = sure_dal::connect(&format!("sqlite:{}", path.display())).await.unwrap();
        sure_dal::migrate(&db).await.unwrap();
        (db, TempDb { path })
    }

    /// A network-free stand-in for a real provider: returns a fixed set of quotes for
    /// any ticker except `fail_ticker`, which always errors (simulating a bad/delisted
    /// symbol).
    struct FakeProvider {
        quotes: Vec<StockPriceQuote>,
        fail_ticker: Option<&'static str>,
    }

    #[async_trait]
    impl StockPriceProvider for FakeProvider {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn description(&self) -> &'static str {
            "fake provider for tests"
        }
        async fn fetch_daily_prices(
            &self,
            ticker: &str,
            _exchange: Option<&str>,
            _from: NaiveDate,
            _to: NaiveDate,
        ) -> anyhow::Result<Vec<StockPriceQuote>> {
            if self.fail_ticker == Some(ticker) {
                return Err(anyhow::anyhow!("simulated upstream failure"));
            }
            Ok(self.quotes.clone())
        }
    }

    fn shares_account(name: &str, kind: AccountKind, currency: &str, ticker: &str, exchange: &str) -> SaveAccount {
        SaveAccount {
            name: name.to_string(),
            kind,
            currency_code: currency.to_string(),
            institution: None,
            metadata: Some(AccountMetadata::Shares(SharesMeta {
                broker: None,
                ticker: Some(ticker.to_string()),
                exchange: Some(exchange.to_string()),
                url: None,
                notes: None,
            })),
            archived: false,
            sort_order: 0,
        }
    }

    #[tokio::test]
    async fn price_at_backfills_on_a_miss_and_then_reads_from_cache() {
        let (db, _tmp) = test_db().await;
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let provider = FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of,
                close: "5.60".parse().unwrap(),
                currency_code: "NZD".to_string(),
            }],
            fail_ticker: None,
        };

        let price = price_at(&db, &provider, "MEL", "NZX", as_of).await.unwrap();
        assert_eq!(price.unwrap().close, "5.60");

        // The backfill wrote through to the cache, not just returned an ephemeral value.
        let cached = sure_dal::stock_prices::get_at(&db, "MEL", "NZX", "2026-07-10").await.unwrap();
        assert_eq!(cached.unwrap().close, "5.60");
    }

    #[tokio::test]
    async fn price_at_is_none_when_the_provider_has_nothing_for_the_range() {
        let (db, _tmp) = test_db().await;
        let provider = FakeProvider { quotes: vec![], fail_ticker: None };
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();

        assert!(price_at(&db, &provider, "ZZZZ", "", as_of).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn task_run_refreshes_every_ticker_and_a_failing_one_does_not_block_the_rest() {
        let (db, _tmp) = test_db().await;
        sure_dal::accounts::create(
            &db,
            shares_account("Meridian", AccountKind::SharesNz, "NZD", "MEL", "NZX"),
        )
        .await
        .unwrap();
        sure_dal::accounts::create(
            &db,
            shares_account("Delisted", AccountKind::SharesUs, "USD", "BAD", ""),
        )
        .await
        .unwrap();

        let today = Utc::now().date_naive();
        let provider = Arc::new(FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of: today,
                close: "5.60".parse().unwrap(),
                currency_code: "NZD".to_string(),
            }],
            fail_ticker: Some("BAD"),
        });
        let task = StockPriceTask::new(db.clone(), provider);

        // The failing ticker doesn't surface as an error out of run() — it's logged and
        // skipped so the rest of the batch still completes.
        task.run().await.unwrap();

        assert!(sure_dal::stock_prices::get_at(&db, "MEL", "NZX", &today.to_string())
            .await
            .unwrap()
            .is_some());
        assert!(sure_dal::stock_prices::list_history(&db, "BAD", "").await.unwrap().is_empty());
    }
}
