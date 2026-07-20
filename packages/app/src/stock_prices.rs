//! Stock prices: an on-demand "fetch a price at an arbitrary point in time" helper
//! that backfills the historical cache from the configured [`StockPriceProvider`] on a
//! miss, plus a [`ScheduledTask`] that keeps every ticker currently held by a shares
//! account warm on a daily cadence. Persistence is handled through the [`StockPriceCacheRepo`]
//! / [`AccountRepo`] ports, not `sure-providers` — matching the split used for exchange
//! rates (`tasks::exchange_rates`) and transaction providers: the provider only fetches
//! and normalizes.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
pub use sure_core::AppResult;
use sure_core::StockPrice;
use sure_providers::StockPriceProvider;
use sure_scheduler::ScheduledTask;

use crate::ports::{AccountRepo, Clock, StockPriceCacheRepo};

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
    prices: &dyn StockPriceCacheRepo,
    provider: &dyn StockPriceProvider,
    ticker: &str,
    exchange: &str,
    as_of: NaiveDate,
) -> AppResult<Option<StockPrice>> {
    if let Some(cached) = prices.get_at(ticker, exchange, &as_of.to_string()).await? {
        return Ok(Some(cached));
    }

    let from = as_of - chrono::Duration::days(BACKFILL_LOOKBACK_DAYS);
    let exchange_hint = Some(exchange).filter(|e| !e.is_empty());
    let quotes = provider
        .fetch_daily_prices(ticker, exchange_hint, from, as_of)
        .await?;
    for quote in &quotes {
        prices
            .upsert(
                ticker,
                exchange,
                &quote.as_of.to_string(),
                &quote.close.to_string(),
                &quote.currency_code,
            )
            .await?;
    }

    prices.get_at(ticker, exchange, &as_of.to_string()).await
}

pub struct StockPriceTask {
    accounts: Arc<dyn AccountRepo>,
    prices: Arc<dyn StockPriceCacheRepo>,
    clock: Arc<dyn Clock>,
    provider: Arc<dyn StockPriceProvider>,
}

impl StockPriceTask {
    pub fn new(
        accounts: Arc<dyn AccountRepo>,
        prices: Arc<dyn StockPriceCacheRepo>,
        clock: Arc<dyn Clock>,
        provider: Arc<dyn StockPriceProvider>,
    ) -> Self {
        Self {
            accounts,
            prices,
            clock,
            provider,
        }
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
        let tickers: Vec<_> = self
            .accounts
            .list_shares_tickers()
            .await?
            .into_iter()
            .chain(self.accounts.list_brokerage_tickers().await?)
            .filter(|t| seen.insert((t.ticker.clone(), t.exchange.clone())))
            .collect();
        let today = self.clock.today();
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
                self.prices
                    .upsert(
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    use sure_core::Account;
    use sure_providers::StockPriceQuote;

    use super::*;
    use crate::ports::SharesTicker;
    use crate::test_clock::FixedClock;

    /// An in-memory stand-in for the stock-price cache table — no database needed.
    #[derive(Default)]
    struct FakePriceCache {
        rows: Mutex<HashMap<(String, String, String), StockPrice>>,
    }

    #[async_trait]
    impl StockPriceCacheRepo for FakePriceCache {
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

    #[tokio::test]
    async fn price_at_backfills_on_a_miss_and_then_reads_from_cache() {
        let cache = FakePriceCache::default();
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let provider = FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of,
                close: "5.60".parse().unwrap(),
                currency_code: "NZD".to_string(),
            }],
            fail_ticker: None,
        };

        let price = price_at(&cache, &provider, "MEL", "NZX", as_of)
            .await
            .unwrap();
        assert_eq!(price.unwrap().close, "5.60");

        // The backfill wrote through to the cache, not just returned an ephemeral value.
        let cached = cache.get_at("MEL", "NZX", "2026-07-10").await.unwrap();
        assert_eq!(cached.unwrap().close, "5.60");
    }

    #[tokio::test]
    async fn price_at_is_none_when_the_provider_has_nothing_for_the_range() {
        let cache = FakePriceCache::default();
        let provider = FakeProvider {
            quotes: vec![],
            fail_ticker: None,
        };
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();

        assert!(price_at(&cache, &provider, "ZZZZ", "", as_of)
            .await
            .unwrap()
            .is_none());
    }

    /// An in-memory stand-in for the accounts table's ticker listings — the task never
    /// looks up an individual account, so `get`/the setters are unreachable here.
    struct FakeAccounts {
        shares: Vec<SharesTicker>,
        brokerage: Vec<SharesTicker>,
    }

    #[async_trait]
    impl AccountRepo for FakeAccounts {
        async fn list(&self, _include_archived: bool) -> AppResult<Vec<Account>> {
            unreachable!("StockPriceTask never lists accounts")
        }
        async fn get(&self, _id: i64) -> AppResult<Account> {
            unreachable!("StockPriceTask never looks up a single account")
        }
        async fn create(&self, _input: sure_core::SaveAccount) -> AppResult<Account> {
            unreachable!("StockPriceTask never creates an account")
        }
        async fn update(&self, _id: i64, _input: sure_core::SaveAccount) -> AppResult<Account> {
            unreachable!("StockPriceTask never updates an account")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("StockPriceTask never deletes an account")
        }
        async fn set_secured_by(&self, _id: i64, _target: Option<i64>) -> AppResult<Account> {
            unreachable!("StockPriceTask never mutates account metadata")
        }
        async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            Ok(self.shares.clone())
        }
        async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            Ok(self.brokerage.clone())
        }
        async fn set_credit_limit(
            &self,
            _account_id: i64,
            _credit_limit_minor: i64,
        ) -> AppResult<()> {
            unreachable!("StockPriceTask never mutates an account")
        }
        async fn set_original_amount(
            &self,
            _account_id: i64,
            _original_amount_minor: i64,
        ) -> AppResult<()> {
            unreachable!("StockPriceTask never mutates an account")
        }
        async fn set_institution_if_unset(
            &self,
            _account_id: i64,
            _institution: &str,
        ) -> AppResult<()> {
            unreachable!("StockPriceTask never mutates an account")
        }
    }

    #[tokio::test]
    async fn task_run_refreshes_every_ticker_and_a_failing_one_does_not_block_the_rest() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let accounts = Arc::new(FakeAccounts {
            shares: vec![
                SharesTicker {
                    ticker: "MEL".to_string(),
                    exchange: "NZX".to_string(),
                },
                SharesTicker {
                    ticker: "BAD".to_string(),
                    exchange: String::new(),
                },
            ],
            brokerage: vec![],
        });
        let prices = Arc::new(FakePriceCache::default());
        let clock = Arc::new(FixedClock(today));
        let provider = Arc::new(FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of: today,
                close: "5.60".parse().unwrap(),
                currency_code: "NZD".to_string(),
            }],
            fail_ticker: Some("BAD"),
        });
        let task = StockPriceTask::new(accounts, prices.clone(), clock, provider);

        // The failing ticker doesn't surface as an error out of run() — it's logged and
        // skipped so the rest of the batch still completes.
        task.run().await.unwrap();

        assert!(prices
            .get_at("MEL", "NZX", &today.to_string())
            .await
            .unwrap()
            .is_some());
        assert!(prices
            .get_at("BAD", "", &today.to_string())
            .await
            .unwrap()
            .is_none());
    }
}
