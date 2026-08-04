//! Currency conversion using the latest known rates, shared by the report aggregation
//! (`crate::reports`) and the brokerage snapshot (`crate::brokerage`). For a
//! single-family tracker, applying current rates across history is a fine approximation
//! and keeps figures readable rather than jumping around with historical fx noise.
//!
//! **Conversion is fallible, and this module refuses to hide it.** Every conversion returns
//! an `Option`, and there is deliberately no infallible `factor`. The old one ended with
//! `1.0 // unknown pair: assume parity` — defensible as a fallback between two real
//! currencies, but catastrophic when the rate table is *empty*, because then every pair is
//! unknown and every foreign amount is counted 1:1. That is exactly what happened while the
//! poller wrote a table nothing read (see `sure_dal::exchange_rates`): years of foreign
//! holdings silently reported at parity, and 2,325 converted brokerage valuations persisted
//! from it. A missing rate must therefore reach the caller as `None` so the caller can
//! *exclude* the amount and say so — an unconverted foreign amount is a wrong number, not a
//! missing one.
//!
//! Each `Fx` also remembers the currencies it was asked for and could not convert
//! ([`Fx::unconverted`]) and the newest date across the rates it loaded
//! ([`Fx::rates_as_of`]), so a response can carry both to the UI instead of a confident
//! total that quietly left money out.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard, PoisonError};

use sure_core::{AppError, AppResult};

use crate::ports::FxRatesRepo;

pub struct Fx {
    base: String,
    /// (base_code, quote_code) => 1 base = rate quote.
    rates: HashMap<(String, String), f64>,
    decimals: HashMap<String, i32>,
    /// Newest `as_of` across every loaded rate; `None` when no rate is on record at all.
    rates_as_of: Option<String>,
    /// Currency codes some conversion asked for and could not do. Recorded behind a lock
    /// rather than returned through every call site: a miss is bookkeeping about the *ask*,
    /// not a mutation of the rate table, and threading `&mut` through the report builders
    /// (closures over `&Fx`, recursive flow-graph emitters) would obscure the arithmetic
    /// this module exists to make readable. A `Mutex` over a `RefCell` keeps `Fx` `Sync`, so
    /// an `&Fx` can still be held across an `await` in a `Send` future.
    unconverted: Mutex<BTreeSet<String>>,
}

impl Fx {
    pub async fn load(repo: &dyn FxRatesRepo, base: String) -> AppResult<Self> {
        let decimals = repo
            .currency_decimals()
            .await?
            .into_iter()
            .map(|c| (c.code, c.decimal_places))
            .collect();

        // Already one row per pair, reduced to its latest `as_of` in SQL — no reliance on
        // iteration order here, and no whole dated series pulled across to be discarded.
        let rows = repo.exchange_rates().await?;
        let rates_as_of = rows.iter().map(|r| r.as_of.clone()).max();
        let rates = rows
            .into_iter()
            .filter_map(|r| {
                let v = r.rate.parse::<f64>().ok()?;
                Some(((r.base_code, r.quote_code), v))
            })
            .collect();
        Ok(Self {
            base,
            rates,
            decimals,
            rates_as_of,
            unconverted: Mutex::new(BTreeSet::new()),
        })
    }

    /// The newest date across every rate loaded (ISO-8601), or `None` when the table is
    /// empty. Belongs on any response carrying a converted total: the poller only writes on
    /// success, so a dead feed leaves last year's rates in place looking exactly like this
    /// morning's, and only the date can tell the two apart.
    pub fn rates_as_of(&self) -> Option<&str> {
        self.rates_as_of.as_deref()
    }

    /// The currency codes this `Fx` was asked to convert and could not, sorted. Whatever a
    /// caller left out of its total, in the words the user needs to hear about it.
    pub fn unconverted(&self) -> Vec<String> {
        self.misses().iter().cloned().collect()
    }

    /// This currency's minor-unit scale, or `None` when it isn't in the `currencies` table
    /// at all — in which case nothing here knows how to read its amounts.
    pub fn try_dp(&self, ccy: &str) -> Option<i32> {
        self.decimals.get(ccy).copied()
    }

    /// [`Self::try_dp`] with the near-universal 2 as a fallback.
    ///
    /// Legitimate only for scaling an amount that *stays* in `ccy` (a position's market
    /// value in its own trading currency, a base-currency figure being rendered). Never for
    /// a conversion: the conversion path goes through [`Self::try_base_scale`], which
    /// refuses an unknown currency instead of guessing its scale.
    pub fn dp(&self, ccy: &str) -> i32 {
        self.try_dp(ccy).unwrap_or(2)
    }

    /// Multiplier converting 1 unit of `ccy` into the base currency, or `None` when no rate
    /// links the two.
    ///
    /// Callers must **not** substitute `1.0`. An amount left in a foreign currency and
    /// counted into a base-currency total is a wrong number that looks like a right one;
    /// dropping it from the total and reporting the currency in [`Self::unconverted`] is a
    /// number the user can see is incomplete. `ccy == base` is `Some(1.0)` because that is a
    /// real rate, not a fallback.
    pub fn try_factor(&self, ccy: &str) -> Option<f64> {
        if ccy == self.base {
            return Some(1.0);
        }
        if let Some(r) = self.rates.get(&(self.base.clone(), ccy.to_string())) {
            // A stored zero would divide to infinity; treat it as no rate at all.
            if *r != 0.0 {
                return Some(1.0 / r);
            }
        }
        if let Some(r) = self.rates.get(&(ccy.to_string(), self.base.clone())) {
            return Some(*r);
        }
        self.record_miss(ccy);
        None
    }

    /// The single multiplier taking `ccy`'s **minor** units to base-currency **major**
    /// units, or `None` if either half is unknown. Resolve it once per currency and reuse it
    /// in a hot loop (see `crate::forecast`'s per-path simulation) rather than re-deriving
    /// the rate per amount.
    pub fn try_base_scale(&self, ccy: &str) -> Option<f64> {
        let Some(dp) = self.try_dp(ccy) else {
            // Not in `currencies`, so it has no scale — and, being the FK target a rate row
            // points at, it can have no rate either. Same outcome as a missing rate, named
            // the same way rather than guessed at two decimal places.
            self.record_miss(ccy);
            return None;
        };
        Some(self.try_factor(ccy)? / 10f64.powi(dp))
    }

    /// Convert minor units of `ccy` into base-currency major units, or `None` when `ccy`
    /// cannot be converted (see [`Self::try_factor`] — do not fall back to the raw amount).
    pub fn try_to_base_major(&self, amount_minor: i64, ccy: &str) -> Option<f64> {
        // Zero is zero at every rate, so it needs none — and must not flag a currency as
        // unconverted, or an emptied foreign account nags forever about excluding nothing.
        if amount_minor == 0 {
            return Some(0.0);
        }
        Some(amount_minor as f64 * self.try_base_scale(ccy)?)
    }

    pub fn base_minor(&self, base_major: f64) -> i64 {
        (base_major * 10f64.powi(self.dp(&self.base))).round() as i64
    }

    /// The error for a figure that has to be one converted number or nothing — an equity
    /// position, a valuation about to be persisted — naming every currency missed so far.
    ///
    /// Use it instead of dropping a term from such a total: an equity position missing one
    /// of its secured debts, or a stored valuation missing a holding, is indistinguishable
    /// from a correct one once written down. A refusal is recoverable (add the rate, re-run);
    /// a plausible wrong number is what left 2,325 valuations to be untangled.
    pub fn missing_rate_error(&self) -> AppError {
        AppError::validation(format!(
            "no exchange rate between {} and {}: refusing to report a total that would leave \
             it out or count it at parity — poll or import a rate for it first",
            self.unconverted().join(", "),
            self.base,
        ))
    }

    /// Name a currency we could not convert, warning the first time so a missing pair
    /// between two real currencies stops being invisible. Once per currency, not per row:
    /// a report converts every transaction, and one absent pair would otherwise print
    /// thousands of identical lines.
    fn record_miss(&self, ccy: &str) {
        if self.misses().insert(ccy.to_string()) {
            tracing::warn!(
                base = %self.base,
                quote = %ccy,
                "no exchange rate for this pair: amounts in it are excluded from converted \
                 totals rather than counted at parity"
            );
        }
    }

    fn misses(&self) -> MutexGuard<'_, BTreeSet<String>> {
        // Nothing in here can panic while the guard is held, so a poisoned lock can only
        // mean an unrelated panic elsewhere in the process; the set is still intact.
        self.unconverted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
impl Fx {
    /// A trivial same-currency `Fx` for tests elsewhere in this crate that don't need
    /// real conversion rates — conversion is identity for `base` itself, and `None` for
    /// anything else.
    pub(crate) fn parity(base: &str) -> Self {
        Fx {
            base: base.to_string(),
            rates: HashMap::new(),
            // The base currency's own scale still has to be known, or its own amounts
            // aren't convertible either. Two places, like every real currency here.
            decimals: HashMap::from([(base.to_string(), 2)]),
            rates_as_of: None,
            unconverted: Mutex::new(BTreeSet::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{CurrencyDecimals, ExchangeRateRow};
    use async_trait::async_trait;

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

    fn dp(code: &str) -> CurrencyDecimals {
        CurrencyDecimals {
            code: code.to_string(),
            decimal_places: 2,
        }
    }

    fn rate(base: &str, quote: &str, rate: &str, as_of: &str) -> ExchangeRateRow {
        ExchangeRateRow {
            base_code: base.to_string(),
            quote_code: quote.to_string(),
            rate: rate.to_string(),
            as_of: as_of.to_string(),
        }
    }

    async fn fx_with(rates: Vec<ExchangeRateRow>) -> Fx {
        Fx::load(
            &FakeFx {
                decimals: vec![dp("NZD"), dp("USD"), dp("AUD")],
                rates,
            },
            "NZD".to_string(),
        )
        .await
        .unwrap()
    }

    /// The recurrence guard itself: an absent pair is `None` and names itself, where it used
    /// to be a confident 1.0. $600 USD must not read as $600 NZD.
    #[tokio::test]
    async fn a_missing_rate_is_none_and_names_the_currency() {
        let fx = fx_with(Vec::new()).await;
        assert_eq!(fx.try_factor("USD"), None);
        assert_eq!(fx.try_to_base_major(60_000, "USD"), None);
        assert_eq!(fx.unconverted(), vec!["USD".to_string()]);
    }

    #[tokio::test]
    async fn the_base_currency_needs_no_rate() {
        let fx = fx_with(Vec::new()).await;
        assert_eq!(fx.try_factor("NZD"), Some(1.0));
        assert_eq!(fx.try_to_base_major(60_000, "NZD"), Some(600.0));
        assert!(fx.unconverted().is_empty());
    }

    /// 1 NZD = 0.6 USD, so $600 USD is $1000 NZD — and the pair goes both ways.
    #[tokio::test]
    async fn converts_in_either_direction_once_a_rate_exists() {
        let fx = fx_with(vec![rate("NZD", "USD", "0.6", "2026-01-02")]).await;
        assert_eq!(fx.try_to_base_major(60_000, "USD"), Some(1000.0));
        assert!(fx.unconverted().is_empty());

        let reverse = fx_with(vec![rate("USD", "NZD", "1.6", "2026-01-02")]).await;
        assert_eq!(reverse.try_to_base_major(1_000_00, "USD"), Some(1600.0));
    }

    /// A zero balance is exact in every currency: it must not be excluded, and must not put
    /// its currency on the "no rate" list.
    #[tokio::test]
    async fn zero_converts_without_a_rate() {
        let fx = fx_with(Vec::new()).await;
        assert_eq!(fx.try_to_base_major(0, "USD"), Some(0.0));
        assert!(fx.unconverted().is_empty());
    }

    /// A currency with no `currencies` row has no scale, so its amounts are unreadable —
    /// the same answer as a missing rate, reported the same way.
    #[tokio::test]
    async fn an_unknown_currency_is_unconverted_not_assumed_two_decimals() {
        let fx = fx_with(vec![rate("NZD", "JPY", "90", "2026-01-02")]).await;
        assert_eq!(fx.try_to_base_major(1_000, "JPY"), None);
        assert_eq!(fx.unconverted(), vec!["JPY".to_string()]);
    }

    #[tokio::test]
    async fn rates_as_of_is_the_newest_date_on_record() {
        let fx = fx_with(vec![
            rate("NZD", "USD", "0.6", "2026-01-02"),
            rate("NZD", "AUD", "0.92", "2026-03-09"),
        ])
        .await;
        assert_eq!(fx.rates_as_of(), Some("2026-03-09"));
        assert_eq!(fx_with(Vec::new()).await.rates_as_of(), None);
    }

    /// A rate stored as `0` (or as an unparseable string — a transposed `as_of`/`rate` bind
    /// writes a date into `rate`) is no rate: it must not divide to infinity or slip through
    /// as parity.
    #[tokio::test]
    async fn a_zero_or_unparseable_rate_is_treated_as_missing() {
        let fx = fx_with(vec![
            rate("NZD", "USD", "0", "2026-01-02"),
            rate("NZD", "AUD", "2026-01-02", "2026-01-02"),
        ])
        .await;
        assert_eq!(fx.try_factor("USD"), None);
        assert_eq!(fx.try_factor("AUD"), None);
        assert_eq!(fx.unconverted(), vec!["AUD".to_string(), "USD".to_string()]);
    }
}
