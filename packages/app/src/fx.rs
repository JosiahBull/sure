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
//! **Rates chain.** The stored table is a star, not a matrix — the poller knows one base and
//! writes `base → quote` for each quote — so a pair like `AUD→USD` is on record in neither
//! direction even when both are one hop from the centre. [`build_factors`] therefore resolves
//! every currency's factor once, at load, by walking the rate graph out from the base rather
//! than looking for a single stored pair. This widens what converts; it never invents a rate,
//! and a currency the graph cannot reach is refused exactly as it was before.
//!
//! Each `Fx` also remembers the currencies it was asked for and could not convert
//! ([`Fx::unconverted`]) and the newest date across the rates it loaded
//! ([`Fx::rates_as_of`]), so a response can carry both to the UI instead of a confident
//! total that quietly left money out.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use sure_core::{AppError, AppResult};

use crate::ports::FxRatesRepo;

/// Every currency's multiplier into `base`, walked out of the stored pairs.
///
/// **The rate table is a star, not a matrix.** `crate::tasks::exchange_rates` polls exactly one
/// base — `settings.base_currency_code` — and stores `base → quote` for each quote it gets
/// back, so every edge on record has that one currency on one side. Looking only for a direct
/// pair therefore works perfectly while a report is denominated in that same currency, and
/// falls apart the moment it isn't: with NZD→{AUD,USD,GBP,EUR} on record and `?currency=AUD`
/// (a documented param on every report), `AUD→USD` exists in neither direction, so every US
/// holding drops out of an Australian-dollar report — even though NZD→AUD and NZD→USD between
/// them say exactly what it is worth. Two currencies one hop from the centre are two hops from
/// each other, and that is all this is.
///
/// So walk it. Each stored `1 b = r q` is usable in both directions, and a breadth-first search
/// from `base` gives every reachable currency a factor in the fewest hops on record. A star
/// resolves in at most two: quote → centre → other quote. Nothing here invents a rate — an
/// unreachable currency stays unreachable and is refused exactly as before.
///
/// Deterministic by construction: adjacency is a `BTreeMap` and the frontier drains in order,
/// because a factor that depended on hash iteration order would move a balance between two runs
/// over identical data. Explicit directions are inserted before derived inverses, so a table
/// holding both `NZD→USD` and `USD→NZD` uses the row actually written for each direction rather
/// than one of them reciprocated.
///
/// A rate of zero or one that isn't finite is **not an edge**: it cannot be inverted, and
/// "1 NZD = 0 USD" is a broken row, not a currency that has become worthless. Same for a factor
/// that comes out non-finite partway along a chain. This is the one place that judgement is
/// made, which is what lets every conversion downstream divide by a factor without checking it.
fn build_factors(base: &str, edges: &BTreeMap<(String, String), f64>) -> HashMap<String, f64> {
    let usable = |r: &f64| r.is_finite() && *r != 0.0;

    let mut adj: BTreeMap<&str, BTreeMap<&str, f64>> = BTreeMap::new();
    for ((b, q), r) in edges.iter().filter(|(_, r)| usable(r)) {
        adj.entry(b).or_default().insert(q, *r);
    }
    for ((b, q), r) in edges.iter().filter(|(_, r)| usable(r)) {
        adj.entry(q).or_default().entry(b).or_insert(1.0 / r);
    }

    let mut factors = HashMap::from([(base.to_string(), 1.0)]);
    let mut frontier = VecDeque::from([base.to_string()]);
    while let Some(from) = frontier.pop_front() {
        let from_factor = factors[&from];
        let Some(neighbours) = adj.get(from.as_str()) else {
            continue;
        };
        for (to, rate) in neighbours {
            if factors.contains_key(*to) {
                continue;
            }
            // `1 from = rate to`, so one `to` is `1/rate` of a `from`, which is
            // `from_factor / rate` of the base currency.
            let factor = from_factor / rate;
            if !usable(&factor) {
                continue;
            }
            factors.insert((*to).to_string(), factor);
            frontier.push_back((*to).to_string());
        }
    }
    factors
}

pub struct Fx {
    base: String,
    /// Multiplier taking 1 unit of a currency into the base currency, for every currency the
    /// rate table connects to it — directly or through a chain. Resolved once, at load, by
    /// [`build_factors`]; a conversion is then a lookup rather than a search.
    factors: HashMap<String, f64>,
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
        let edges: BTreeMap<(String, String), f64> = rows
            .into_iter()
            .filter_map(|r| {
                let v = r.rate.parse::<f64>().ok()?;
                Some(((r.base_code, r.quote_code), v))
            })
            .collect();
        Ok(Self {
            factors: build_factors(&base, &edges),
            base,
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

    /// Multiplier converting 1 unit of `ccy` into the base currency, or `None` when no chain
    /// of stored rates links the two.
    ///
    /// Callers must **not** substitute `1.0`. An amount left in a foreign currency and
    /// counted into a base-currency total is a wrong number that looks like a right one;
    /// dropping it from the total and reporting the currency in [`Self::unconverted`] is a
    /// number the user can see is incomplete. `ccy == base` is `Some(1.0)` because that is a
    /// real rate, not a fallback.
    pub fn try_factor(&self, ccy: &str) -> Option<f64> {
        match self.factors.get(ccy) {
            Some(f) => Some(*f),
            None => {
                self.record_miss(ccy);
                None
            }
        }
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

    /// Convert `amount_minor` in `from` into **minor units of `to`**, or `None` when either
    /// leg has no rate — in which case the unconvertible side is named in
    /// [`Self::unconverted`], exactly as [`Self::try_to_base_major`] would.
    ///
    /// For a total that has to reach a currency which isn't the base one: an account holding
    /// several currencies being expressed in its own (see
    /// `crate::reports::account_value_at`). Both legs go through the base currency, so it
    /// carries one rounding — resolve it over a currency's *subtotal*, never per row, or a
    /// few hundred transactions round into a visibly wrong balance. `from == to` short
    /// circuits and is exact.
    pub fn try_convert_minor(&self, amount_minor: i64, from: &str, to: &str) -> Option<i64> {
        if from == to {
            return Some(amount_minor);
        }
        // Zero is zero at every rate, so it needs none — and must not flag either side as
        // unconverted, on the same argument as `try_to_base_major`.
        if amount_minor == 0 {
            return Some(0);
        }
        let base_major = self.try_to_base_major(amount_minor, from)?;
        // Safe to divide by unchecked: `build_factors` refuses to record a zero or non-finite
        // factor at all, so every factor that exists is one this can divide by.
        Some((base_major / self.try_base_scale(to)?).round() as i64)
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
    /// An `Fx` over an explicit rate table, for tests elsewhere in this crate that convert.
    ///
    /// Each entry is `(base_code, quote_code, rate)` meaning 1 `base_code` = `rate`
    /// `quote_code`, exactly as a row of `exchange_rates` reads — the triple rather than a
    /// bare quote so a test can build a table that is *not* centred on `base` and exercise
    /// the pivot. Every code named on either side gets the usual two decimal places; anything
    /// not named has no scale and no rate, which is how a test asks for an unconvertible
    /// currency.
    pub(crate) fn with_rates(base: &str, rates: &[(&str, &str, f64)]) -> Self {
        let mut decimals = HashMap::from([(base.to_string(), 2)]);
        let mut edges = BTreeMap::new();
        for (from, to, rate) in rates {
            decimals.insert((*from).to_string(), 2);
            decimals.insert((*to).to_string(), 2);
            edges.insert(((*from).to_string(), (*to).to_string()), *rate);
        }
        Fx {
            factors: build_factors(base, &edges),
            base: base.to_string(),
            decimals,
            rates_as_of: None,
            unconverted: Mutex::new(BTreeSet::new()),
        }
    }

    /// A trivial same-currency `Fx` for tests elsewhere in this crate that don't need
    /// real conversion rates — conversion is identity for `base` itself, and `None` for
    /// anything else.
    pub(crate) fn parity(base: &str) -> Self {
        Fx {
            base: base.to_string(),
            factors: HashMap::from([(base.to_string(), 1.0)]),
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

    /// Converting between two currencies that are both *not* the base one, which is what an
    /// account holding several currencies needs: 1 NZD = 0.6 USD and 1 NZD = 0.9 AUD, so
    /// US$60 is A$90.
    #[tokio::test]
    async fn converts_between_two_non_base_currencies() {
        let fx = fx_with(vec![
            rate("NZD", "USD", "0.6", "2026-01-02"),
            rate("NZD", "AUD", "0.9", "2026-01-02"),
        ])
        .await;
        assert_eq!(fx.try_convert_minor(60_00, "USD", "AUD"), Some(90_00));
        assert_eq!(fx.try_convert_minor(90_00, "AUD", "USD"), Some(60_00));
        assert!(fx.unconverted().is_empty());
    }

    /// The identity and zero short circuits: neither needs a rate, and neither may report a
    /// currency as unconverted — an all-NZD account must never reach the rate table at all.
    #[tokio::test]
    async fn converting_a_currency_to_itself_or_converting_zero_needs_no_rate() {
        let fx = fx_with(Vec::new()).await;
        assert_eq!(fx.try_convert_minor(1_234_56, "JPY", "JPY"), Some(1_234_56));
        assert_eq!(fx.try_convert_minor(0, "USD", "AUD"), Some(0));
        assert!(fx.unconverted().is_empty());
    }

    /// Either leg missing is a refusal, and the leg that is actually missing is the one named.
    #[tokio::test]
    async fn a_missing_leg_in_either_direction_refuses_and_names_itself() {
        let fx = fx_with(vec![rate("NZD", "USD", "0.6", "2026-01-02")]).await;
        assert_eq!(fx.try_convert_minor(60_00, "USD", "JPY"), None);
        assert_eq!(fx.try_convert_minor(60_00, "JPY", "USD"), None);
        assert_eq!(fx.unconverted(), vec!["JPY".to_string()]);
    }

    /// A `0` anywhere is not a rate: it cannot be inverted, and it must not divide to infinity
    /// and saturate into a nonsense balance. Rejected when the graph is built, so no currency
    /// it touches becomes reachable through it.
    #[tokio::test]
    async fn a_zero_rate_is_not_an_edge_in_either_direction() {
        let fx = fx_with(vec![
            rate("NZD", "USD", "0.6", "2026-01-02"),
            rate("AUD", "NZD", "0", "2026-01-02"),
        ])
        .await;
        assert_eq!(fx.try_convert_minor(60_00, "USD", "AUD"), None);
        assert_eq!(fx.unconverted(), vec!["AUD".to_string()]);
    }

    /// **The real table, and a report that isn't in the currency it was polled for.**
    ///
    /// `tasks::exchange_rates` only ever stores `settings.base_currency_code → quote`, so the
    /// rows on record are a star around NZD. Ask for a report in AUD — `?currency=AUD` is a
    /// documented param — and `AUD→USD` is on record in neither direction. Every US, British
    /// and European holding used to drop straight out of that report, while NZD converted fine
    /// off the reversed `NZD→AUD` edge, so the total looked plausible and was short by
    /// whatever sat in the other three currencies.
    ///
    /// Two hops through the centre is all it takes: 1 NZD = ½ AUD and 1 NZD = ¼ USD, so
    /// 1 USD = 4 NZD = 2 AUD. (Rates are powers of two so every figure below is exact in
    /// binary floating point and the assertions can be equalities.)
    #[tokio::test]
    async fn a_report_currency_that_is_not_the_polled_base_still_converts_every_quote() {
        let star = || {
            vec![
                rate("NZD", "AUD", "0.5", "2026-08-05"),
                rate("NZD", "USD", "0.25", "2026-08-05"),
                rate("NZD", "GBP", "0.125", "2026-08-05"),
            ]
        };
        let fx = Fx::load(
            &FakeFx {
                decimals: vec![dp("NZD"), dp("USD"), dp("AUD"), dp("GBP")],
                rates: star(),
            },
            "AUD".to_string(),
        )
        .await
        .unwrap();

        // One hop, reversed — the only pair that worked before.
        assert_eq!(fx.try_factor("NZD"), Some(0.5));
        // Two hops, via the centre. These were `None`, and every holding in them was dropped.
        assert_eq!(fx.try_factor("USD"), Some(2.0));
        assert_eq!(fx.try_factor("GBP"), Some(4.0));
        assert_eq!(fx.try_convert_minor(100_00, "USD", "AUD"), Some(200_00));
        assert_eq!(fx.try_convert_minor(100_00, "GBP", "USD"), Some(200_00));
        assert!(fx.unconverted().is_empty());

        // And the polled base itself is unchanged: everything is still one direct hop.
        let home = fx_with(star()).await;
        assert_eq!(home.try_factor("AUD"), Some(2.0));
        assert_eq!(home.try_factor("USD"), Some(4.0));
    }

    /// A currency the chain genuinely cannot reach is still refused and still named. Walking
    /// the graph widens what converts; it does not invent a rate for a currency nothing links.
    #[tokio::test]
    async fn an_unreachable_currency_is_still_refused_after_the_walk() {
        let fx = fx_with(vec![
            rate("NZD", "USD", "0.6", "2026-01-02"),
            // A pair between two currencies with no link back to the rest of the table.
            rate("SEK", "NOK", "1.02", "2026-01-02"),
        ])
        .await;
        assert_eq!(fx.try_factor("USD"), Some(1.0 / 0.6));
        assert_eq!(fx.try_factor("SEK"), None);
        assert_eq!(fx.try_factor("NOK"), None);
        assert_eq!(fx.unconverted(), vec!["NOK".to_string(), "SEK".to_string()]);
    }

    /// An explicitly stored direction is used as written, not reciprocated from the opposite
    /// row. Both directions on record is pathological — the poller writes one — but which of
    /// the two a factor comes from must not depend on iteration order.
    ///
    /// Read from USD, so the walk needs the `USD→NZD` edge: it must come from the row that
    /// says so rather than from `1/0.5`.
    #[tokio::test]
    async fn an_explicit_direction_beats_a_derived_inverse() {
        let fx = Fx::load(
            &FakeFx {
                decimals: vec![dp("NZD"), dp("USD")],
                rates: vec![
                    rate("NZD", "USD", "0.5", "2026-01-02"),
                    // Deliberately not 1/0.5, so the two are distinguishable.
                    rate("USD", "NZD", "1.9", "2026-01-02"),
                ],
            },
            "USD".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(fx.try_factor("NZD"), Some(1.0 / 1.9));
        assert_ne!(
            fx.try_factor("NZD"),
            Some(0.5),
            "0.5 would mean the NZD→USD row was reciprocated instead"
        );
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
