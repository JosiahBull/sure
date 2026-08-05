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
use sure_core::{AppError, StockPrice};
use sure_scheduler::{ScheduledTask, TaskRun};
use tokio_util::sync::CancellationToken;

use crate::ports::{AccountRepo, Clock, StockPriceCacheRepo, StockPriceProvider};

/// How far back to look when backfilling around a target date — comfortably spans
/// weekends and most public-holiday clusters (e.g. Christmas/New Year) so "nearest
/// preceding trading day" still finds something.
const BACKFILL_LOOKBACK_DAYS: i64 = 10;

/// Free upstream sources are daily-resolution at best, so there's no value in polling
/// more often than this (same reasoning as the exchange-rate poll).
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether a failed cache write is the *quote's* fault rather than the database's.
///
/// A quote's `currency_code` is whatever the upstream feed said, and the price cache refuses
/// one that is not a `currencies` row — a pence pseudo-currency like `GBX`, a crypto ticker,
/// an unseeded code — as [`AppError::Validation`]; `currency_code` is the price table's only
/// foreign key, so that is the only thing it can mean (`sure_dal::stock_prices::upsert`).
/// Retrying cannot turn such a quote into a writable one, so the caller drops it and carries
/// on; anything else may be transient (a locked or unavailable database) and must still
/// surface, so the scheduler leaves the run unrecorded and tries again.
///
/// The failure this prevents: one unseeded currency used to propagate out of the sweep, so
/// every ticker after it went unpriced *and* — a failed run never being recorded — the poll
/// re-ran on every check tick instead of once a day, re-failing on the same quote each time.
///
/// A `matches!` rather than an exhaustive `match`: `AppError::Database` exists only when
/// `sure-core`'s `sqlx` feature is on, which for this crate depends on what else is in the
/// build, so naming every variant here would compile in the workspace and not on its own.
fn is_unusable_quote(err: &AppError) -> bool {
    matches!(err, AppError::Validation(_))
}

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
    // The one call here that leaves the process, and the only place its failure can still be
    // told apart from anything else that went wrong. A bare `?` converts `anyhow::Error`
    // straight into `AppError::Internal`, which meant a Yahoo outage answered `500 internal`
    // from all five routes that reach this function — this one and the four brokerage endpoints
    // through `BrokerageService::resolve_price`. See [`AppError::Upstream`] for why that is the
    // wrong thing to tell a client and the wrong thing to leave in a log.
    //
    // Note what is deliberately *not* an upstream error: a 404 for a delisted symbol, which
    // `YahooFinanceProvider` already returns as an empty vec, and a quote in a currency the
    // price table will not take, which the loop below drops. Both mean "no price", and the
    // caller handles that as `None`.
    let quotes = provider
        .fetch_daily_prices(ticker, exchange_hint, from, as_of)
        .await
        .map_err(|err| AppError::upstream(&err))?;
    for quote in &quotes {
        // A quote in an unknown currency is dropped, not fatal: the caller is a panel asking
        // for one price, and failing the whole backfill over one unusable day turned a
        // missing price (which every caller already handles as `None`) into a 500.
        if let Err(err) = prices
            .upsert(
                ticker,
                exchange,
                &quote.as_of.to_string(),
                &quote.close.to_string(),
                &quote.currency_code,
            )
            .await
        {
            if !is_unusable_quote(&err) {
                return Err(err);
            }
            tracing::warn!(
                ticker = %ticker,
                exchange = %exchange,
                currency = %quote.currency_code,
                as_of = %quote.as_of,
                error = %err,
                "skipped a stock quote in an unknown currency"
            );
        }
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

    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
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
        let mut skipped = 0;
        for t in &tickers {
            // Between whole tickers, never mid-ticker: a symbol's quotes are all written or
            // none are, and an interrupted run isn't recorded, so the next start sweeps again.
            if cancel.is_cancelled() {
                tracing::debug!(
                    refreshed,
                    skipped,
                    "stock price refresh stopped early for shutdown"
                );
                return Ok(TaskRun::Interrupted);
            }
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
                if let Err(err) = self
                    .prices
                    .upsert(
                        &t.ticker,
                        &t.exchange,
                        &quote.as_of.to_string(),
                        &quote.close.to_string(),
                        &quote.currency_code,
                    )
                    .await
                {
                    // One quote the cache cannot store must not end the sweep. It is skipped
                    // with the ticker and the offending code in the log — the two things an
                    // operator needs to decide whether to add the currency — and `run`
                    // still returns `Ok`, so the scheduler records the run and the poll waits
                    // out its interval instead of retrying the same bad quote every tick.
                    // Deliberately *not* auto-creating the currency: `currencies` carries
                    // `decimal_places`, `symbol` and a name, and a guessed row would silently
                    // mis-render every amount in that currency (a `GBX` price rendered as
                    // pounds at 2dp) while looking exactly like a curated one.
                    if !is_unusable_quote(&err) {
                        return Err(err.into());
                    }
                    tracing::warn!(
                        ticker = %t.ticker,
                        exchange = %t.exchange,
                        currency = %quote.currency_code,
                        as_of = %quote.as_of,
                        error = %err,
                        "skipped a stock quote in an unknown currency"
                    );
                    skipped += 1;
                }
            }
            refreshed += 1;
        }
        tracing::info!(
            tickers = tickers.len(),
            refreshed,
            skipped,
            "refreshed stock prices"
        );
        Ok(TaskRun::Completed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::ports::StockPriceQuote;
    use sure_core::{Account, Ownership};

    use super::*;
    use crate::ports::SharesTicker;
    use crate::test_clock::FixedClock;

    /// An in-memory stand-in for the stock-price cache table — no database needed. It models
    /// the one constraint that matters here: `currency_code` is a foreign key into
    /// `currencies`, and the real writer refuses an unknown code as `AppError::Validation`
    /// (`sure_dal::stock_prices::upsert`) rather than letting SQLite raise an opaque FK error.
    struct FakePriceCache {
        rows: Mutex<HashMap<(String, String, String), StockPrice>>,
        /// The seeded `currencies` rows. Matches migration `0001_core.sql`'s seed set, pared
        /// down — anything outside it is an unknown currency.
        known_currencies: Vec<&'static str>,
        /// When set, every write fails with a non-validation error: a stand-in for a database
        /// that is unhappy for reasons a retry might fix.
        db_broken: bool,
    }

    impl Default for FakePriceCache {
        fn default() -> Self {
            Self {
                rows: Mutex::new(HashMap::new()),
                known_currencies: vec!["NZD", "USD"],
                db_broken: false,
            }
        }
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
            if self.db_broken {
                return Err(AppError::Internal(anyhow::anyhow!("database is locked")));
            }
            if !self.known_currencies.contains(&ccy) {
                return Err(AppError::validation(format!("unknown currency '{ccy}'")));
            }
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
    /// symbol). `per_ticker` overrides `quotes` for named symbols, which is how a single
    /// sweep can mix a normal quote with one in an unusable currency.
    #[derive(Default)]
    struct FakeProvider {
        quotes: Vec<StockPriceQuote>,
        per_ticker: HashMap<&'static str, Vec<StockPriceQuote>>,
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
            Ok(self
                .per_ticker
                .get(ticker)
                .cloned()
                .unwrap_or_else(|| self.quotes.clone()))
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
            ..Default::default()
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
        let provider = FakeProvider::default();
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();

        assert!(price_at(&cache, &provider, "ZZZZ", "", as_of)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn price_at_skips_an_unknown_currency_quote_instead_of_failing_the_backfill() {
        let cache = FakePriceCache::default();
        let friday = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let thursday = NaiveDate::from_ymd_opt(2026, 7, 9).unwrap();
        // Thursday is quoted normally; Friday comes back in pence, which is not a
        // `currencies` row. The unusable day is dropped and Thursday's close still answers.
        let provider = FakeProvider {
            quotes: vec![
                StockPriceQuote {
                    as_of: thursday,
                    close: "5.50".parse().unwrap(),
                    currency_code: "NZD".to_string(),
                },
                StockPriceQuote {
                    as_of: friday,
                    close: "72.30".parse().unwrap(),
                    currency_code: "GBX".to_string(),
                },
            ],
            ..Default::default()
        };

        let price = price_at(&cache, &provider, "MEL", "NZX", friday)
            .await
            .expect("an unusable quote must not fail the whole backfill");
        assert_eq!(price.unwrap().close, "5.50");
    }

    #[tokio::test]
    async fn price_at_is_none_rather_than_an_error_when_every_quote_is_unusable() {
        let cache = FakePriceCache::default();
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let provider = FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of,
                close: "72.30".parse().unwrap(),
                currency_code: "GBX".to_string(),
            }],
            ..Default::default()
        };

        // A missing price is what every caller already handles; a 500 from a panel is not.
        assert!(price_at(&cache, &provider, "VOD", "LSE", as_of)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn price_at_still_surfaces_a_database_error() {
        let cache = FakePriceCache {
            db_broken: true,
            ..Default::default()
        };
        let as_of = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let provider = FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of,
                close: "5.60".parse().unwrap(),
                currency_code: "NZD".to_string(),
            }],
            ..Default::default()
        };

        assert!(price_at(&cache, &provider, "MEL", "NZX", as_of)
            .await
            .is_err());
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
        async fn set_ownership(&self, _id: i64, _ownership: Ownership) -> AppResult<Account> {
            unreachable!("StockPriceTask never attributes accounts")
        }
        async fn set_ownership_bulk(&self, _ids: &[i64], _ownership: Ownership) -> AppResult<u64> {
            unreachable!("StockPriceTask never attributes accounts")
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
        async fn set_account_number_if_unset(
            &self,
            _account_id: i64,
            _account_number: &str,
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
            ..Default::default()
        });
        let task = StockPriceTask::new(accounts, prices.clone(), clock, provider);

        // The failing ticker doesn't surface as an error out of run() — it's logged and
        // skipped so the rest of the batch still completes.
        task.run(&CancellationToken::new()).await.unwrap();

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

    /// Collects rendered events so a test can assert on what an operator would actually see.
    /// Hand-rolled rather than `tracing-subscriber` so `sure-app` gains no dependency for it.
    #[derive(Default)]
    struct CapturedLogs {
        events: Mutex<Vec<String>>,
    }

    impl CapturedLogs {
        fn contains_all(&self, needles: &[&str]) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| needles.iter().all(|n| e.contains(n)))
        }
    }

    struct CapturingSubscriber(Arc<CapturedLogs>);

    impl tracing::Subscriber for CapturingSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Render<'a>(&'a mut String);
            impl tracing::field::Visit for Render<'_> {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    use std::fmt::Write as _;
                    let _ = write!(self.0, " {}={value:?}", field.name());
                }
            }
            let mut rendered = event.metadata().level().to_string();
            event.record(&mut Render(&mut rendered));
            self.0.events.lock().unwrap().push(rendered);
        }
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn task_run_skips_an_unknown_currency_quote_and_still_prices_the_other_tickers() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        // `VOD` sorts before `MEL` in the listing order, so the bad quote is hit *first* —
        // the regression this guards is precisely that everything after it went unpriced.
        let accounts = Arc::new(FakeAccounts {
            shares: vec![
                SharesTicker {
                    ticker: "VOD".to_string(),
                    exchange: "LSE".to_string(),
                },
                SharesTicker {
                    ticker: "MEL".to_string(),
                    exchange: "NZX".to_string(),
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
            per_ticker: HashMap::from([(
                "VOD",
                vec![StockPriceQuote {
                    as_of: today,
                    close: "72.30".parse().unwrap(),
                    currency_code: "GBX".to_string(),
                }],
            )]),
            fail_ticker: None,
        });
        let task = StockPriceTask::new(accounts, prices.clone(), clock, provider);

        let logs = Arc::new(CapturedLogs::default());
        let guard = tracing::subscriber::set_default(CapturingSubscriber(logs.clone()));
        // `Ok` is the load-bearing part: `sure_scheduler`'s `run_if_due` records a run only
        // when `run` returns `Ok`, and an unrecorded run is retried on *every* check tick —
        // so returning `Err` over one unusable quote turned a daily poll into a tight loop
        // that failed on the same quote every time.
        let outcome = task
            .run(&CancellationToken::new())
            .await
            .expect("one unusable quote must not fail the sweep");
        assert_eq!(outcome, TaskRun::Completed);
        drop(guard);

        assert!(prices
            .get_at("MEL", "NZX", &today.to_string())
            .await
            .unwrap()
            .is_some());
        assert!(prices
            .get_at("VOD", "LSE", &today.to_string())
            .await
            .unwrap()
            .is_none());
        // The WARN has to name both the symbol and the offending code, or nobody can tell
        // which currency to add.
        assert!(
            logs.contains_all(&["WARN", "VOD", "GBX"]),
            "expected a WARN naming the ticker and the currency, got {:?}",
            logs.events.lock().unwrap()
        );
    }

    #[tokio::test]
    async fn task_run_still_fails_on_a_database_error_so_the_run_is_retried() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let accounts = Arc::new(FakeAccounts {
            shares: vec![SharesTicker {
                ticker: "MEL".to_string(),
                exchange: "NZX".to_string(),
            }],
            brokerage: vec![],
        });
        let prices = Arc::new(FakePriceCache {
            db_broken: true,
            ..Default::default()
        });
        let provider = Arc::new(FakeProvider {
            quotes: vec![StockPriceQuote {
                as_of: today,
                close: "5.60".parse().unwrap(),
                currency_code: "NZD".to_string(),
            }],
            ..Default::default()
        });
        let task = StockPriceTask::new(accounts, prices, Arc::new(FixedClock(today)), provider);

        // Unlike an unusable quote, this may well be transient — so it must still surface and
        // leave the run unrecorded, which is what makes the scheduler try again.
        assert!(task.run(&CancellationToken::new()).await.is_err());
    }
}
