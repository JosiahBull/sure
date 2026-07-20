//! Brokerage compute: turn the holdings ledger + wallet cash into a valued snapshot
//! (pricing each position via the historical stock-price cache and converting into the
//! account's currency), snapshot it into a valuation, and backfill a daily valuation
//! series across the account's whole history. Persistence goes through the
//! [`AccountRepo`]/[`BrokerageRepo`]/[`StockPriceCacheRepo`]/[`ValuationRepo`]/[`FxRatesRepo`]
//! ports (implemented by `sure-dal`'s `SqliteStore`); this module is the price-lookup +
//! FX orchestration, matching the `price_at`/`StockPriceTask` split in `crate::stock_prices`.

use std::sync::Arc;

use chrono::NaiveDate;

use sure_core::StockPrice;
use sure_core::{AppError, AppResult, BrokerageSnapshot, Position, WalletBalance};
use sure_providers::StockPriceProvider;

use crate::fx::Fx;
use crate::ports::{
    AccountRepo, BrokerageRepo, Clock, FxRatesRepo, StockPriceCacheRepo, ValuationRepo,
};

pub struct BrokerageService {
    accounts: Arc<dyn AccountRepo>,
    brokerage: Arc<dyn BrokerageRepo>,
    prices: Arc<dyn StockPriceCacheRepo>,
    valuations: Arc<dyn ValuationRepo>,
    fx: Arc<dyn FxRatesRepo>,
    clock: Arc<dyn Clock>,
}

impl BrokerageService {
    pub fn new(
        accounts: Arc<dyn AccountRepo>,
        brokerage: Arc<dyn BrokerageRepo>,
        prices: Arc<dyn StockPriceCacheRepo>,
        valuations: Arc<dyn ValuationRepo>,
        fx: Arc<dyn FxRatesRepo>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            accounts,
            brokerage,
            prices,
            valuations,
            fx,
            clock,
        }
    }

    /// Resolve a position's price as of `as_of`. With a provider, backfill the cache from
    /// it on a miss (the live endpoints want fresh data); without one, read cache-only
    /// (the day-by-day backfill loop, after a single bulk fetch has already warmed the
    /// cache — so it never fires one upstream request per day).
    async fn resolve_price(
        &self,
        provider: Option<&dyn StockPriceProvider>,
        ticker: &str,
        exchange: &str,
        as_of: NaiveDate,
    ) -> AppResult<Option<StockPrice>> {
        match provider {
            Some(p) => {
                crate::stock_prices::price_at(self.prices.as_ref(), p, ticker, exchange, as_of)
                    .await
            }
            None => {
                self.prices
                    .get_at(ticker, exchange, &as_of.to_string())
                    .await
            }
        }
    }

    /// Compute the account's full snapshot as of `as_of`: every open position priced and
    /// valued, every wallet cash balance, and a grand total converted into the account's
    /// currency. See [`Self::resolve_price`] for the `provider` semantics.
    pub async fn snapshot(
        &self,
        provider: Option<&dyn StockPriceProvider>,
        account_id: i64,
        as_of: NaiveDate,
    ) -> AppResult<BrokerageSnapshot> {
        let account = self.accounts.get(account_id).await?;
        let account_ccy = account.currency_code;
        let as_of_str = as_of.to_string();
        let fx = Fx::load(self.fx.as_ref(), account_ccy.clone()).await?;

        let mut total_major = 0.0f64;

        let mut positions = Vec::new();
        for p in self.brokerage.positions_at(account_id, &as_of_str).await? {
            let price = self
                .resolve_price(provider, &p.ticker, &p.exchange, as_of)
                .await?;
            let (price_text, price_as_of, value_minor) = match &price {
                Some(sp) => {
                    let value = market_value_minor(p.quantity, &sp.close, fx.dp(&sp.currency_code));
                    (Some(sp.close.clone()), Some(sp.as_of.clone()), value)
                }
                None => (None, None, None),
            };
            if let Some(v) = value_minor {
                // A position's price may be quoted in a different currency than the
                // ticker's listing (rare), so convert from the price's currency where we
                // have it.
                let value_ccy = price
                    .as_ref()
                    .map(|sp| sp.currency_code.as_str())
                    .unwrap_or(&p.currency_code);
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
        for w in self
            .brokerage
            .wallet_balances_at(account_id, &as_of_str)
            .await?
        {
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
        &self,
        provider: Option<&dyn StockPriceProvider>,
        account_id: i64,
        as_of: NaiveDate,
    ) -> AppResult<BrokerageSnapshot> {
        let snap = self.snapshot(provider, account_id, as_of).await?;
        self.valuations
            .upsert_from_brokerage(
                account_id,
                &snap.as_of,
                snap.total_value_minor,
                &snap.currency_code,
            )
            .await?;
        Ok(snap)
    }

    /// Reconstruct the account's whole net-worth history: bulk-fetch each held ticker's
    /// full daily price series in one upstream call, then walk every calendar day from
    /// the account's first activity to today, upserting a `source='brokerage'` valuation
    /// per day from the (now warm) cache. Idempotent — safe to re-run as a retry. Returns
    /// the number of days valued.
    pub async fn backfill_history(
        &self,
        provider: &dyn StockPriceProvider,
        account_id: i64,
    ) -> AppResult<usize> {
        let Some(earliest) = self.brokerage.earliest_activity_date(account_id).await? else {
            return Ok(0); // nothing imported yet
        };
        let Some(from) = NaiveDate::parse_from_str(&earliest, "%Y-%m-%d").ok() else {
            return Err(AppError::validation(
                "could not parse earliest activity date",
            ));
        };
        let today = self.clock.today();

        // One upstream call per ticker covering the full window, written through to the cache.
        for (ticker, exchange) in self.brokerage.account_tickers(account_id).await? {
            let exchange_hint = Some(exchange.as_str()).filter(|e| !e.is_empty());
            match provider
                .fetch_daily_prices(&ticker, exchange_hint, from, today)
                .await
            {
                Ok(quotes) => {
                    for q in &quotes {
                        // Exact decimal text, matching `stock_prices::price_at`'s own write.
                        self.prices
                            .upsert(
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
                    // One bad/delisted ticker shouldn't sink the whole backfill — its
                    // position simply goes unpriced for the affected days.
                    tracing::warn!(ticker = %ticker, exchange = %exchange, error = %err, "brokerage backfill: price fetch failed");
                }
            }
        }

        // Day-by-day valuation from the warm cache (provider=None → no further network calls).
        let mut day = from;
        let mut valued = 0usize;
        while day <= today {
            self.revalue(None, account_id, day).await?;
            day += chrono::Duration::days(1);
            valued += 1;
        }
        tracing::info!(account_id, days = valued, "brokerage history backfilled");
        Ok(valued)
    }

    /// The raw holdings ledger (every buy/sell/corporate lot) for an audit view.
    pub async fn list_holdings(&self, account_id: i64) -> AppResult<Vec<sure_core::HoldingLot>> {
        self.brokerage.list_holdings(account_id).await
    }

    /// Manually record a lot (most arrive via import).
    pub async fn create_holding(
        &self,
        account_id: i64,
        input: sure_core::SaveHoldingLot,
    ) -> AppResult<sure_core::HoldingLot> {
        self.brokerage.create_holding(account_id, input).await
    }

    pub async fn delete_holding(&self, id: i64) -> AppResult<()> {
        self.brokerage.delete_holding(id).await
    }

    /// Dividend/distribution history with per-jurisdiction withholding detail.
    pub async fn list_dividends(
        &self,
        account_id: i64,
    ) -> AppResult<Vec<sure_core::DividendDetail>> {
        self.brokerage.list_dividends(account_id).await
    }

    /// Persist a parsed bulk export (wallet transactions, holding lots, dividends).
    #[allow(clippy::too_many_arguments)]
    pub async fn import_export(
        &self,
        account_id: i64,
        account_currency: &str,
        provider_tag: &str,
        wallet_rows: &[crate::ports::ImportRow],
        holdings: &[crate::ports::HoldingImport],
        dividends: &[crate::ports::DividendImport],
    ) -> AppResult<crate::ports::ImportCounts> {
        self.brokerage
            .import_export(
                account_id,
                account_currency,
                provider_tag,
                wallet_rows,
                holdings,
                dividends,
            )
            .await
    }
}

/// Value in minor units of holding `quantity` units at `close` (decimal text), in the
/// price's own currency (`dp` decimal places). `None` if `close` doesn't parse.
fn market_value_minor(quantity: f64, close: &str, dp: i32) -> Option<i64> {
    let close = close.parse::<f64>().ok()?;
    Some((quantity * close * 10f64.powi(dp)).round() as i64)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use sure_core::{
        Account, AccountClass, AccountKind, AccountMetadata, BrokerageMeta, NewValuation,
        SaveAccount, Valuation,
    };
    use sure_providers::StockPriceQuote;

    use super::*;
    use crate::ports::{CurrencyDecimals, ExchangeRateRow, HoldingRow, SharesTicker, WalletRow};
    use crate::test_clock::FixedClock;

    fn brokerage_account(id: i64, ccy: &str) -> Account {
        Account {
            id,
            name: "Sharesies".to_string(),
            kind: AccountKind::Brokerage,
            class: AccountClass::Investment,
            currency_code: ccy.to_string(),
            institution: Some("Sharesies".to_string()),
            metadata: AccountMetadata::Brokerage(BrokerageMeta::default()),
            archived: false,
            sort_order: 0,
            secured_by_account_id: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        }
    }

    struct FakeAccounts {
        account: Account,
    }
    #[async_trait]
    impl AccountRepo for FakeAccounts {
        async fn list(&self, _include_archived: bool) -> AppResult<Vec<Account>> {
            unreachable!("BrokerageService never lists accounts")
        }
        async fn get(&self, _id: i64) -> AppResult<Account> {
            Ok(self.account.clone())
        }
        async fn create(&self, _input: SaveAccount) -> AppResult<Account> {
            unreachable!("BrokerageService never creates an account")
        }
        async fn update(&self, _id: i64, _input: SaveAccount) -> AppResult<Account> {
            unreachable!("BrokerageService never updates an account")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("BrokerageService never deletes an account")
        }
        async fn set_secured_by(&self, _id: i64, _target: Option<i64>) -> AppResult<Account> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            unreachable!("BrokerageService never lists global tickers")
        }
        async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            unreachable!("BrokerageService never lists global tickers")
        }
        async fn set_credit_limit(
            &self,
            _account_id: i64,
            _credit_limit_minor: i64,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn set_original_amount(
            &self,
            _account_id: i64,
            _original_amount_minor: i64,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
        async fn set_institution_if_unset(
            &self,
            _account_id: i64,
            _institution: &str,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never mutates account metadata")
        }
    }

    #[derive(Default, Clone)]
    struct FakeBrokerage {
        positions: Vec<HoldingRow>,
        wallets: Vec<WalletRow>,
        tickers: Vec<(String, String)>,
        earliest: Option<String>,
    }
    #[async_trait]
    impl BrokerageRepo for FakeBrokerage {
        async fn positions_at(&self, _account_id: i64, _as_of: &str) -> AppResult<Vec<HoldingRow>> {
            Ok(self.positions.clone())
        }
        async fn wallet_balances_at(
            &self,
            _account_id: i64,
            _as_of: &str,
        ) -> AppResult<Vec<WalletRow>> {
            Ok(self.wallets.clone())
        }
        async fn account_tickers(&self, _account_id: i64) -> AppResult<Vec<(String, String)>> {
            Ok(self.tickers.clone())
        }
        async fn earliest_activity_date(&self, _account_id: i64) -> AppResult<Option<String>> {
            Ok(self.earliest.clone())
        }
        async fn list_holdings(&self, _account_id: i64) -> AppResult<Vec<sure_core::HoldingLot>> {
            unreachable!("BrokerageService never lists the raw holdings ledger")
        }
        async fn create_holding(
            &self,
            _account_id: i64,
            _input: sure_core::SaveHoldingLot,
        ) -> AppResult<sure_core::HoldingLot> {
            unreachable!("BrokerageService never creates a holding lot")
        }
        async fn delete_holding(&self, _id: i64) -> AppResult<()> {
            unreachable!("BrokerageService never deletes a holding lot")
        }
        async fn list_dividends(
            &self,
            _account_id: i64,
        ) -> AppResult<Vec<sure_core::DividendDetail>> {
            unreachable!("BrokerageService never lists dividends")
        }
        async fn import_export(
            &self,
            _account_id: i64,
            _account_currency: &str,
            _provider_tag: &str,
            _wallet_rows: &[crate::ports::ImportRow],
            _holdings: &[crate::ports::HoldingImport],
            _dividends: &[crate::ports::DividendImport],
        ) -> AppResult<crate::ports::ImportCounts> {
            unreachable!("BrokerageService never imports a bulk export")
        }
    }

    #[derive(Default)]
    struct FakePrices {
        rows: Mutex<HashMap<(String, String, String), StockPrice>>,
    }
    #[async_trait]
    impl StockPriceCacheRepo for FakePrices {
        async fn get_at(
            &self,
            ticker: &str,
            exchange: &str,
            as_of: &str,
        ) -> AppResult<Option<StockPrice>> {
            // Mirrors the real query: the closest cached close on or before `as_of`, not
            // an exact-date match — a weekend/holiday date still resolves to something.
            Ok(self
                .rows
                .lock()
                .unwrap()
                .values()
                .filter(|sp| {
                    sp.ticker == ticker && sp.exchange == exchange && sp.as_of.as_str() <= as_of
                })
                .max_by(|a, b| a.as_of.cmp(&b.as_of))
                .cloned())
        }
        async fn upsert(
            &self,
            ticker: &str,
            exchange: &str,
            as_of: &str,
            close: &str,
            ccy: &str,
        ) -> AppResult<()> {
            self.rows.lock().unwrap().insert(
                (ticker.to_string(), exchange.to_string(), as_of.to_string()),
                StockPrice {
                    ticker: ticker.to_string(),
                    exchange: exchange.to_string(),
                    as_of: as_of.to_string(),
                    close: close.to_string(),
                    currency_code: ccy.to_string(),
                    fetched_at: "2026-01-01T00:00:00.000Z".to_string(),
                },
            );
            Ok(())
        }
    }

    /// Mirrors the real table's `ON CONFLICT(account_id, as_of) ... DO UPDATE` — a second
    /// upsert for the same day replaces rather than accumulates, so tests can assert
    /// idempotency the same way the real backend does.
    #[derive(Default)]
    struct FakeValuations {
        rows: Mutex<HashMap<(i64, String), (i64, String)>>,
    }
    #[async_trait]
    impl ValuationRepo for FakeValuations {
        async fn list_for_account(&self, _account_id: i64) -> AppResult<Vec<Valuation>> {
            unreachable!("BrokerageService never lists valuations")
        }
        async fn create(&self, _account_id: i64, _input: NewValuation) -> AppResult<Valuation> {
            unreachable!("BrokerageService never creates a manual valuation")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("BrokerageService never deletes a valuation")
        }
        async fn upsert_from_brokerage(
            &self,
            account_id: i64,
            as_of: &str,
            value_minor: i64,
            ccy: &str,
        ) -> AppResult<()> {
            self.rows.lock().unwrap().insert(
                (account_id, as_of.to_string()),
                (value_minor, ccy.to_string()),
            );
            Ok(())
        }
        async fn upsert_from_provider(
            &self,
            _account_id: i64,
            _as_of: &str,
            _value_minor: i64,
            _ccy: &str,
        ) -> AppResult<()> {
            unreachable!("BrokerageService never records a provider valuation")
        }
    }

    struct FakeFx {
        decimals: Vec<CurrencyDecimals>,
        rates: Vec<ExchangeRateRow>,
    }
    #[async_trait]
    impl FxRatesRepo for FakeFx {
        async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>> {
            Ok(self.decimals.clone())
        }
        async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>> {
            Ok(self.rates.clone())
        }
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

    fn service(
        account: Account,
        brokerage: FakeBrokerage,
        clock: NaiveDate,
    ) -> (BrokerageService, Arc<FakePrices>, Arc<FakeValuations>) {
        let prices = Arc::new(FakePrices::default());
        let valuations = Arc::new(FakeValuations::default());
        let svc = BrokerageService::new(
            Arc::new(FakeAccounts { account }),
            Arc::new(brokerage),
            prices.clone(),
            valuations.clone(),
            Arc::new(FakeFx {
                decimals: vec![CurrencyDecimals {
                    code: "NZD".to_string(),
                    decimal_places: 2,
                }],
                rates: vec![],
            }),
            Arc::new(FixedClock(clock)),
        );
        (svc, prices, valuations)
    }

    #[tokio::test]
    async fn snapshot_values_positions_and_wallet_cash() {
        let as_of = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
        let (svc, _prices, _valuations) = service(
            brokerage_account(1, "NZD"),
            FakeBrokerage {
                positions: vec![HoldingRow {
                    ticker: "MEL".to_string(),
                    exchange: "NZX".to_string(),
                    currency_code: "NZD".to_string(),
                    name: Some("MEL Ltd".to_string()),
                    quantity: 100.0,
                }],
                wallets: vec![WalletRow {
                    currency_code: "NZD".to_string(),
                    amount_minor: 25_00,
                }],
                ..Default::default()
            },
            as_of,
        );
        let provider = FakeProvider {
            close: "5.60".parse().unwrap(),
            currency: "NZD".to_string(),
        };

        let snap = svc.snapshot(Some(&provider), 1, as_of).await.unwrap();

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
        let today = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let (svc, _prices, valuations) = service(
            brokerage_account(1, "NZD"),
            FakeBrokerage {
                positions: vec![HoldingRow {
                    ticker: "MEL".to_string(),
                    exchange: "NZX".to_string(),
                    currency_code: "NZD".to_string(),
                    name: Some("MEL Ltd".to_string()),
                    quantity: 10.0,
                }],
                wallets: vec![],
                tickers: vec![("MEL".to_string(), "NZX".to_string())],
                earliest: Some(today.to_string()),
            },
            today,
        );
        let provider = FakeProvider {
            close: "2.00".parse().unwrap(),
            currency: "NZD".to_string(),
        };

        let days = svc.backfill_history(&provider, 1).await.unwrap();
        assert_eq!(days, 1); // earliest == today
        assert_eq!(valuations.rows.lock().unwrap().len(), 1);
        assert_eq!(
            valuations.rows.lock().unwrap().get(&(1, today.to_string())),
            Some(&(20_00, "NZD".to_string())) // 10 × $2.00
        );

        // Re-running upserts the same day rather than accumulating rows.
        svc.backfill_history(&provider, 1).await.unwrap();
        assert_eq!(valuations.rows.lock().unwrap().len(), 1);
    }
}
