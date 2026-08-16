//! Forecast: assumption resolution plus the Monte Carlo projection built on top of it.
//!
//! For every asset/investment/liability account and every top-level income/expense
//! category, [`ForecastService::resolved_assumptions`] resolves the growth/volatility/
//! dividend-yield knob the simulation consumes, in precedence order: an explicit
//! override, an existing enabled cron's rate, or a value derived from history. A
//! mortgage/loan with a complete amortisation schedule is deterministic instead — no
//! rate to resolve at all.
//!
//! [`ForecastService::simulate`] then walks `horizon_months` forward, month by month,
//! `simulations` independent times: a stochastic account's value compounds by a randomly
//! drawn monthly return (lognormal-style — `value *= exp(r)`), a deterministic
//! mortgage/loan projects its exact remaining balance from its own terms, and an income/
//! expense category's baseline grows with its own draw of noise, all cash/bank/savings
//! accounts pooled into one bucket driven by the net of those category flows. Every
//! `forecast_events` step-change/one-off applies identically across every path — it's a
//! certainty the user is asserting, not a statistical estimate. The result is percentile
//! bands (P10/P25/median/mean/P75/P90) per month, not a single guess.
//!
//! Nothing here writes to the real ledger — the whole simulation runs in memory over a
//! snapshot of current state; see `docs/architecture-refactor.md` for why this can't just
//! extend `crons`, which persists real rows when it runs.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Datelike, NaiveDate};
use rand::rngs::StdRng;
// `RngExt`, not `Rng`: rand 0.10 renamed the old `RngCore` to `Rng`, so the trait carrying the
// sampling methods (`random`, `random_range`, …) is now `RngExt`. Importing `Rng` still
// compiles — it is a real trait — but leaves `random::<f64>()` unresolved.
use rand::{RngExt, SeedableRng};
use rand_distr::{Distribution, Normal};

use sure_core::{
    AccountClass, AccountKind, AccountMetadata, AppResult, CategoryKind, CronKind, EffectTarget,
    ForecastAssumption, ForecastEvent, ForecastTargetType, Interval, LifeEffectSpec, LifeEventKind,
    RateType, RelationKind, RepaymentFrequency, StepAmount,
};

use crate::fx::Fx;
use crate::ports::{AccountRepo, Clock, CronRepo, ForecastRepo, FxRatesRepo, ReportRepo};
use crate::reports;

/// Below this many days of valuation/transaction history, a derived default would be
/// noise rather than signal — flagged `InsufficientHistory` instead of guessing.
const MIN_HISTORY_DAYS: i64 = 60;
/// How many trailing complete months of category totals feed the trend regression.
const CATEGORY_TREND_MONTHS: i64 = 24;
/// How far back an account's value series is sampled when deriving its growth rate. Long
/// enough to be stable across a lumpy year, short enough that a structural break — leaving
/// study, finishing a renovation, changing how an account is used — drops out of the fit
/// instead of being averaged against the present.
const ACCOUNT_TREND_MONTHS: i64 = 36;
/// Months a category's history must span before a *direction* is read into it. Below this
/// its run-rate is held flat: the baseline is still projected, but the slope of three or
/// four lumpy months is noise, and compounding it over a multi-year horizon is how a
/// category that saw one large month comes to dominate the whole forecast.
const MIN_CATEGORY_TREND_MONTHS: i64 = 6;
/// …and how many of those months must contain actual spend. A series that is mostly zeros
/// with a couple of spikes has a steep OLS slope that describes the spikes' placement, not
/// a trend in the household's behaviour.
const MIN_CATEGORY_ACTIVE_MONTHS: i64 = 4;
/// How far a fitted slope must stand out from the scatter around it before it is treated
/// as a direction rather than an accident of which months happened to be expensive.
/// Roughly the 95% two-tailed threshold for the 6-24 month windows this fits over.
const MIN_TREND_T_STATISTIC: f64 = 2.0;
/// Ceiling on a *derived* category growth rate, ±25%/yr. Household spending on a category
/// does not compound at triple digits; a fit that says so is over-fitting a short series.
/// An explicit override is deliberately not clamped — that's the user asserting something.
const MAX_DERIVED_CATEGORY_GROWTH_BPS: i64 = 2_500;
/// Ceiling on a *derived* category volatility, 300%/yr.
///
/// This is a numerical guard, not an opinion about how lumpy spending can be: it bounds the
/// exponent of the lognormal monthly draw so one tail sample can't produce an absurd month.
/// Real measured values do run this high — a category paid three months in seven has a
/// month-to-month coefficient of variation in the hundreds of percent, and that is a true
/// description of it. It is only usable at all because the noise no longer accumulates into
/// the run-rate (see the category step in `simulate`); while it did, anything above ~75%
/// compounded into a meaningless spread and the ceiling had to do the model's job for it.
const MAX_DERIVED_CATEGORY_VOL_BPS: i64 = 30_000;
/// Ceiling on *any* volatility reaching the simulation — derived, cron-sourced, or an
/// explicit override — in basis points.
///
/// The same bound as [`MAX_DERIVED_CATEGORY_VOL_BPS`], for the same reason it exists: it
/// bounds the exponent of a lognormal draw, so it is a numerical guard rather than an opinion
/// about how volatile a real holding can be. Growth has had [`MIN_ANNUAL_RATE`]/
/// [`MAX_ANNUAL_RATE`] for exactly this since the beginning; volatility is the one knob that
/// escaped it, and an override arrived from the HTTP edge completely untouched.
///
/// Unbounded, it is a permanent 500 on `GET /api/forecast`: `exp()` saturates past ±745, so a
/// σ in the millions of bps makes a draw of −1e14 (underflowing the month's factor to `0.0`)
/// and one of +1e14 (overflowing it to `inf`) both routine across a few thousand samples.
/// `0.0 * inf` is `NaN`; `NaN >= 0.0` is false, so it files itself under liabilities and
/// lands in the percentile bands. `sure_dal::forecast::upsert_assumption` now refuses such a
/// value at the write edge — the point of clamping *here* as well is that a row written before
/// that check existed, or by anything that isn't that function, still cannot take the endpoint
/// down.
const MAX_VOLATILITY_BPS: i64 = MAX_DERIVED_CATEGORY_VOL_BPS;
const MIN_HORIZON_MONTHS: i64 = 1;
/// Thirty years. Life events — a child, a career break, a promotion chain, a mortgage running
/// to term — are decade-scale, and at the old five-year ceiling most of them fell outside the
/// window entirely.
///
/// Raising it is only honest alongside [`TREND_FULL_STRENGTH_MONTHS`]: a rate fitted over two
/// or three years of history is not evidence about year twenty-nine, and compounding it as if
/// it were turns the ±25%/yr derived clamp into a 807x multiplier at the tail (see
/// [`decayed_monthly_log_return`]).
const MAX_HORIZON_MONTHS: i64 = 360;
const MIN_SIMULATIONS: i64 = 100;
const MAX_SIMULATIONS: i64 = 5000;
const DEFAULT_HORIZON_MONTHS: i64 = 12;
const DEFAULT_SIMULATIONS: i64 = 2000;
/// The most `paths × months` one simulation may run.
///
/// The old worst case was `MAX_SIMULATIONS × 60` = 300k path-months. Six times that keeps a
/// 360-month horizon at 2 000 paths — comfortably enough for a stable P10/P90 — while stopping a
/// 5 000 × 360 request from costing thirty times the old ceiling inside a compute slot the rest
/// of the process is waiting on (see [`ForecastService::simulate_from`] on why that matters).
///
/// A 60-month horizon allows 12 000 paths under this budget, which is well above
/// [`MAX_SIMULATIONS`] — so no request that was legal before the ceiling was raised is affected
/// by it. The reduction is reported rather than silent: [`ForecastResult`] echoes the
/// `simulations` and `horizon_months` actually run, so a caller that asked for more can tell.
const MAX_PATH_MONTHS: i64 = 720_000;
/// Months over which a *derived* trend is used at full strength — exactly the old
/// [`MAX_HORIZON_MONTHS`], which is what makes raising that ceiling leave every previously-legal
/// projection byte-identical.
const TREND_FULL_STRENGTH_MONTHS: i64 = 60;
/// Half-life, in months, of a derived rate's *excess* over its long-run anchor beyond the window
/// above.
///
/// 24 = [`CATEGORY_TREND_MONTHS`], and the symmetry is the argument: a fit is evidence about the
/// window it was taken over, so it is worth about half its weight one window past the point where
/// it stops being evidence at all.
///
/// The category window rather than [`ACCOUNT_TREND_MONTHS`] (36), deliberately, because one
/// constant serves both and the shorter window is the conservative choice — the entire purpose of
/// this decay is conservatism about extrapolation, so where the two disagree it should side with
/// the fit that saw less. The difference is not academic: at the derived growth ceiling a
/// 36-month half-life leaves a ×8.37 multiplier over 30 years against ×5.97 for 24, and the
/// tests below pin both figures so this comment cannot quietly stop being true.
const TREND_HALF_LIFE_MONTHS: f64 = 24.0;
/// A relative annual rate is clamped to this range before it's turned into a monthly
/// log-return, so a noisy/pathological historical fit can't make `ln(1 + rate)`
/// undefined (`rate <= -100%`) or blow the simulation up over a multi-year horizon.
const MIN_ANNUAL_RATE: f64 = -0.95;
const MAX_ANNUAL_RATE: f64 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssumptionSource {
    /// An explicit `forecast_assumptions` override is set for this target.
    Override,
    /// No override, but an enabled appreciation/depreciation/interest cron already
    /// configures this account's rate — reused rather than re-derived.
    Cron,
    /// No override or cron; computed from this target's own transaction/valuation
    /// history.
    Derived,
    /// A mortgage/loan with a complete amortisation schedule — projected exactly, no
    /// rate to resolve.
    Deterministic,
    /// Fewer than 2 data points (or under `MIN_HISTORY_DAYS`/3 months of history) — mean
    /// and volatility default to 0 rather than inventing a plausible-looking number.
    InsufficientHistory,
    /// This account receives payroll contributions — KiwiSaver, or student-loan repayments — so its
    /// fitted rate has been **discarded**.
    ///
    /// Not a preference. A balance series that rose while contributions were flowing into it cannot
    /// be decomposed into market growth and contributions after the fact, so a rate fitted from it
    /// already contains the money this projection is about to add. Using both counts it twice, and
    /// the error compounds for the whole horizon. Growth therefore comes from an explicit override,
    /// else the long-run anchor, else flat — and a warning says so, because flat is a placeholder
    /// rather than an answer. The measured *volatility* is kept: month-to-month scatter is real
    /// either way.
    ContributionDriven,
    /// This category's cash flow comes from per-person income streams, not from its own fitted
    /// trend. The baseline shown is the *residual* — the part of the category the streams do not
    /// explain — so a non-zero one means some income here is still un-modelled.
    ModelledFromIncome,
}

#[derive(Debug, Clone)]
pub struct ResolvedAssumption {
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    pub label: String,
    pub annual_growth_bps: i64,
    pub annual_volatility_bps: i64,
    /// Annual fund fee in basis points, and a flat annual fee in the account's own minor units.
    /// `None` for both means fees are not modelled here.
    pub annual_fee_bps: Option<i64>,
    pub annual_fixed_fee_minor: Option<i64>,
    /// The annual rate `annual_growth_bps` decays toward past
    /// [`TREND_FULL_STRENGTH_MONTHS`], in basis points. Only consulted when `source` is
    /// [`AssumptionSource::Derived`] — see [`long_run_anchor`] for why every other source is
    /// left alone. 0 (the default) means the trend flattens in nominal terms.
    pub long_run_growth_bps: i64,
    /// Only set for Investment-class accounts (brokerage/shares).
    pub dividend_yield_bps: Option<i64>,
    /// Only set for categories: the current fitted monthly run-rate (base currency,
    /// minor units) the simulation grows forward from. `None` if there's no derived
    /// trend to anchor to (an override alone doesn't imply a baseline).
    pub baseline_minor: Option<i64>,
    /// Only set for a mortgage/loan projected from an amortisation schedule: what that
    /// schedule actually is, so the forecast can show its working rather than an
    /// unexplained "deterministic".
    pub schedule: Option<LoanScheduleSummary>,
    /// The account's own currency, so [`LoanScheduleSummary`]'s minor-unit amounts can be
    /// formatted. `None` for a category, whose `baseline_minor` is in the base currency.
    pub currency_code: Option<String>,
    pub source: AssumptionSource,
}

/// The repayment schedule a deterministic mortgage/loan is projected from.
///
/// Computed at the *stated* refix rate rather than the mean of the simulated draws: the
/// payment is convex in the rate, so those differ slightly. Present it as "at the assumed
/// refix rate", never as an average.
#[derive(Debug, Clone, Copy)]
pub struct LoanScheduleSummary {
    /// Minor units of the account's own currency.
    pub monthly_payment_minor: i64,
    pub current_rate_bps: i64,
    pub remaining_term_months: i64,
    /// Months from today until the fixed rate rolls off; `None` if none is modelled.
    pub refix_in_months: Option<i64>,
    pub refix_rate_bps: Option<i64>,
    pub refix_rate_uncertainty_bps: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct SimulationParams {
    pub horizon_months: i64,
    pub simulations: i64,
    pub currency: Option<String>,
    /// Fixed seed for reproducible output (tests); `None` seeds from OS entropy.
    pub seed: Option<u64>,
}

impl Default for SimulationParams {
    fn default() -> Self {
        Self {
            horizon_months: DEFAULT_HORIZON_MONTHS,
            simulations: DEFAULT_SIMULATIONS,
            currency: None,
            seed: None,
        }
    }
}

/// Everything a Monte Carlo run needs, loaded and owned — the boundary between the awaiting
/// half of a simulation and the purely computational half.
///
/// Produced by [`ForecastService::simulate_inputs`], consumed by
/// [`ForecastService::simulate_from`]. Owned and `Send + 'static` on purpose: that is what
/// lets a request handler move it onto the blocking pool (`Shutdown::spawn_blocking`) and get
/// the several-hundred-millisecond RNG loop off the async workers, without `sure-app` having
/// to know that a runtime exists. Every field is a value, never a borrow of the service or of
/// the params — a `&SimulationParams` here would tie the compute to the request's lifetime and
/// defeat the whole arrangement, which is why the one field it needs (`seed`) is copied in.
///
/// The fields are deliberately not `pub`: nothing outside this module has any business
/// assembling a half-built simulation, and the two functions above are the only legal way to
/// obtain or spend one.
pub struct SimulationInputs {
    accounts: Vec<sure_core::Account>,
    today: NaiveDate,
    tx_by_acct: HashMap<i64, Vec<(NaiveDate, i64, String)>>,
    val_by_acct: HashMap<i64, Vec<(NaiveDate, i64, String)>>,
    fx: Fx,
    /// The projection currency's code.
    base: String,
    horizon: i64,
    n_paths: usize,
    /// Already resolved from `SimulationParams::seed` (or drawn) on the async side, so the
    /// compute half is a pure function of this struct.
    seed: u64,
    /// Reported back out in [`ForecastResult::assumptions`]; the projection itself has
    /// already been distilled into `account_sims`/`category_sims`.
    assumptions: Vec<ResolvedAssumption>,
    account_sims: Vec<AccountSim>,
    category_sims: Vec<CategorySim>,
    stream_sims: Vec<StreamSim>,
    event_sims: Vec<EventSim>,
    warnings: Vec<String>,
    reconciliations: Vec<StreamReconciliation>,
    unmodelled_streams: Vec<String>,
}

/// A percentile band across every simulated path, in the report currency's minor units.
#[derive(Debug, Clone, Default)]
pub struct Band {
    pub p10_minor: i64,
    pub p25_minor: i64,
    pub median_minor: i64,
    pub mean_minor: i64,
    pub p75_minor: i64,
    pub p90_minor: i64,
}

#[derive(Debug, Clone)]
pub struct ForecastMonth {
    pub as_of: String,
    pub net_worth: Band,
    pub assets: Band,
    pub liabilities: Band,
}

#[derive(Debug, Clone)]
pub struct ForecastResult {
    pub currency: String,
    pub months: Vec<ForecastMonth>,
    pub assumptions: Vec<ResolvedAssumption>,
    /// Currency codes whose accounts and transactions are **not** in this projection,
    /// because no rate links them to `currency`. Projecting them at parity would compound a
    /// wrong starting balance for the whole horizon.
    pub unconverted: Vec<String>,
    /// Newest date across the rates used (ISO-8601), or `None` if none are on record.
    pub rates_as_of: Option<String>,
    /// The horizon actually projected, after clamping to
    /// [`MIN_HORIZON_MONTHS`]/[`MAX_HORIZON_MONTHS`]. Equal to `months.len()`, and reported
    /// alongside `simulations` so a caller can tell what it got rather than what it asked for.
    pub horizon_months: i64,
    /// The number of paths actually run, after [`MAX_PATH_MONTHS`]. A caller asking for 5 000
    /// paths over 360 months gets 2 000, and without this had no way to know.
    pub simulations: i64,
    /// Household net income landing in each projected month, in the report currency.
    ///
    /// A band rather than a single figure because it will not stay deterministic: once events can
    /// pause or step a stream per path, this is where that spread shows up. Same length as
    /// `months`.
    pub income_net: Vec<Band>,
    /// Per linked income category, what the streams claim against what history actually saw.
    ///
    /// Reported rather than folded into the projection: a diagnostic that silently changes the
    /// thing it is diagnosing stops being one. This is the check that catches a salary entered
    /// before tax and modelled as take-home.
    /// How each event actually landed across the paths. The chart draws this, not the configured
    /// window — relations move timing, so the two genuinely differ.
    pub events: Vec<EventOutcome>,
    pub reconciliations: Vec<StreamReconciliation>,
    /// Things that changed meaning, or figures the projection is standing in for. Prose, because
    /// each one needs to say what to do about it and there is nothing for a caller to branch on.
    pub warnings: Vec<String>,
    /// Streams left out of the projection, and why — `Fx::unconverted`'s contract. A figure the
    /// user can see is incomplete beats one they cannot.
    pub unmodelled_streams: Vec<String>,
    /// Per month, the fraction of paths whose pooled cash balance was negative, in basis points.
    ///
    /// The projection has always filed a negative cash pool under liabilities by sign, and still
    /// does — changing that would move every existing figure. But a band around net worth cannot
    /// answer "could we actually afford this", because a path that ends rich having gone $40k
    /// overdrawn in year three looks identical to one that never did. This is that question,
    /// counted directly. Same length as `months`.
    pub negative_cash_rate_bps: Vec<i64>,
}

/// What the income streams linked to one category claim, beside what that category's own history
/// actually recorded.
///
/// The pair is the point. A modelled figure 18-45% above the observed one is the exact signature of
/// a gross salary being modelled as take-home at NZ marginal rates — a mistake that is invisible in
/// a net-worth band and obvious here.
#[derive(Debug, Clone)]
pub struct StreamReconciliation {
    pub person_id: i64,
    pub category_id: i64,
    pub category_label: String,
    /// Monthly net the streams model as of today, report currency minor units.
    pub modelled_net_minor: i64,
    /// The category's own fitted monthly baseline, report currency minor units — what history saw.
    pub observed_net_minor: i64,
    /// `modelled / observed`, in basis points. Above 10 000 means the streams claim more than the
    /// category ever recorded, which is a wrong link or a wrong figure rather than good news.
    pub coverage_bps: i64,
    /// What is left for the fitted trend to project once the streams are netted out.
    pub residual_minor: i64,
}

pub struct ForecastService {
    forecast: Arc<dyn ForecastRepo>,
    reports: Arc<dyn ReportRepo>,
    fx: Arc<dyn FxRatesRepo>,
    accounts: Arc<dyn AccountRepo>,
    crons: Arc<dyn CronRepo>,
    clock: Arc<dyn Clock>,
}

impl ForecastService {
    pub fn new(
        forecast: Arc<dyn ForecastRepo>,
        reports: Arc<dyn ReportRepo>,
        fx: Arc<dyn FxRatesRepo>,
        accounts: Arc<dyn AccountRepo>,
        crons: Arc<dyn CronRepo>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            forecast,
            reports,
            fx,
            accounts,
            crons,
            clock,
        }
    }

    async fn base_currency(&self, override_: Option<&str>) -> AppResult<String> {
        if let Some(c) = override_.filter(|s| !s.is_empty()) {
            return Ok(c.to_uppercase());
        }
        self.reports.base_currency().await
    }

    /// The projection currency plus the rate table loaded for it, with an unknown
    /// `?currency=` refused. Same contract and same reasoning as
    /// [`crate::reports::ReportService::currency_and_fx`] — a forecast in a currency that
    /// does not exist is the same nonsense as a report in one, and both parse the value at
    /// the one edge it arrives as text.
    async fn currency_and_fx(&self, override_: Option<&str>) -> AppResult<(String, Fx)> {
        let base = self.base_currency(override_).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;
        if override_.is_some_and(|s| !s.is_empty()) && fx.try_dp(&base).is_none() {
            return Err(reports::unknown_currency(&base));
        }
        Ok((base, fx))
    }

    /// Every account/category's resolved forecast assumption.
    pub async fn resolved_assumptions(&self) -> AppResult<Vec<ResolvedAssumption>> {
        let (_, fx) = self.currency_and_fx(None).await?;
        self.resolved_assumptions_with(&fx).await
    }

    /// As [`Self::resolved_assumptions`], against a caller-supplied `Fx`.
    ///
    /// `simulate` passes its own, for two reasons: the currencies a category baseline could
    /// not convert land on the same `Fx` whose [`Fx::unconverted`] the forecast reports, and
    /// the baselines are fitted in the same currency the projection runs in — this used to
    /// load a second `Fx` on the *default* base while `simulate` ran on the requested one,
    /// so `?currency=` produced baselines in one currency and totals in another.
    async fn resolved_assumptions_with(&self, fx: &Fx) -> AppResult<Vec<ResolvedAssumption>> {
        let overrides = self.forecast.list_assumptions().await?;
        let mut by_target: HashMap<(ForecastTargetType, i64), ForecastAssumption> = HashMap::new();
        for o in overrides {
            by_target.insert((o.target_type, o.target_id), o);
        }

        let today = self.clock.today();
        let mut out = self
            .resolve_account_assumptions(today, &by_target, fx)
            .await?;
        out.extend(
            self.resolve_category_assumptions(today, &by_target, fx)
                .await?,
        );
        Ok(out)
    }

    async fn resolve_account_assumptions(
        &self,
        today: NaiveDate,
        overrides: &HashMap<(ForecastTargetType, i64), ForecastAssumption>,
        fx: &Fx,
    ) -> AppResult<Vec<ResolvedAssumption>> {
        let mut accounts = self.accounts.list(false).await?;
        // Nothing projects an account the household has taken out of its net worth, so the
        // assumptions tab must not offer a growth-rate control for one.
        accounts.retain(|a| !a.excluded_from_net_worth);
        let crons = self.crons.list().await?;
        let (tx_by_acct, val_by_acct) = reports::load_ledger(self.reports.as_ref()).await?;

        let mut out = Vec::new();
        for a in &accounts {
            let class = a.kind.class();
            // Cash is pooled and driven by category cash flow, not an account-level
            // growth rate; a plain credit_card/revolving_credit is an everyday
            // transaction account like cash, not a valued instrument.
            if class == AccountClass::Cash
                || (class == AccountClass::Liability
                    && !matches!(
                        a.kind,
                        AccountKind::Mortgage | AccountKind::Loan | AccountKind::StudentLoan
                    ))
            {
                continue;
            }

            if let Some(terms) = loan_terms(&a.metadata, today) {
                // No rate reaching a currency this account holds: it has no single current
                // balance to amortise, so it is left out here exactly as it is left out of the
                // simulation in `account_sims`, and its currency is named in `unconverted`.
                let Some((current_minor, _)) = reports::account_value_at(
                    a.id,
                    &a.currency_code,
                    today,
                    fx,
                    &tx_by_acct,
                    &val_by_acct,
                ) else {
                    continue;
                };
                // The same constructor the simulation uses, at the stated refix rate — so
                // the figure shown and the figure projected cannot drift apart.
                let schedule = AmortSchedule::expected(&terms, current_minor as f64, today);
                out.push(ResolvedAssumption {
                    target_type: ForecastTargetType::Account,
                    target_id: a.id,
                    label: a.name.clone(),
                    annual_growth_bps: 0,
                    annual_volatility_bps: 0,
                    long_run_growth_bps: 0,
                    annual_fee_bps: None,
                    annual_fixed_fee_minor: None,
                    dividend_yield_bps: None,
                    baseline_minor: None,
                    schedule: Some(LoanScheduleSummary {
                        monthly_payment_minor: schedule.payment.round() as i64,
                        current_rate_bps: terms.rate_bps,
                        remaining_term_months: schedule.remaining_term,
                        refix_in_months: terms.refix.map(|r| r.month),
                        refix_rate_bps: terms.refix.map(|r| r.rate_bps),
                        refix_rate_uncertainty_bps: terms.refix.map(|r| r.uncertainty_bps),
                    }),
                    currency_code: Some(a.currency_code.clone()),
                    source: AssumptionSource::Deterministic,
                });
                continue;
            }

            let ov = overrides.get(&(ForecastTargetType::Account, a.id));
            let cron_growth_bps = crons.iter().find_map(|c| {
                (c.account_id == a.id
                    && c.enabled
                    && matches!(
                        c.kind,
                        CronKind::Appreciation | CronKind::Depreciation | CronKind::Interest
                    ))
                .then(|| {
                    let rate = c.rate_bps.unwrap_or(0);
                    if c.kind == CronKind::Depreciation {
                        -rate
                    } else {
                        rate
                    }
                })
            });

            let series =
                monthly_value_series(a.id, &a.currency_code, today, fx, &tx_by_acct, &val_by_acct);
            let derived = derive_account_rate(class, &series);

            let (growth, vol, source) = resolve_growth(
                ov.and_then(|o| o.annual_growth_bps),
                ov.and_then(|o| o.annual_volatility_bps),
                cron_growth_bps,
                derived,
            );

            let dividend_yield_bps = if class == AccountClass::Investment {
                let derived_yield = self.derive_dividend_yield(a.id, today, &series).await?;
                Some(
                    ov.and_then(|o| o.dividend_yield_bps)
                        .unwrap_or(derived_yield),
                )
            } else {
                None
            };

            out.push(ResolvedAssumption {
                target_type: ForecastTargetType::Account,
                target_id: a.id,
                label: a.name.clone(),
                annual_growth_bps: growth,
                annual_volatility_bps: vol,
                long_run_growth_bps: ov.and_then(|o| o.long_run_growth_bps).unwrap_or(0),
                annual_fee_bps: ov.and_then(|o| o.annual_fee_bps),
                annual_fixed_fee_minor: ov.and_then(|o| o.annual_fixed_fee_minor),
                dividend_yield_bps,
                baseline_minor: None,
                schedule: None,
                currency_code: Some(a.currency_code.clone()),
                source,
            });
        }
        Ok(out)
    }

    async fn resolve_category_assumptions(
        &self,
        today: NaiveDate,
        overrides: &HashMap<(ForecastTargetType, i64), ForecastAssumption>,
        fx: &Fx,
    ) -> AppResult<Vec<ResolvedAssumption>> {
        let cats = reports::Categories::load(self.reports.as_ref()).await?;
        let from = today - chrono::Duration::days(31 * (CATEGORY_TREND_MONTHS + 1));
        // The whole household: a forecast projects the household's finances, and splitting
        // it per person would need per-person income/expense assumptions that don't exist.
        let spend =
            reports::load_spend(self.reports.as_ref(), &cats, from, today, false, None).await?;

        let mut out = Vec::new();
        for (id, kind) in cats.top_level_kinds() {
            match kind {
                // Transfer categories have no cash-flow assumption to make.
                CategoryKind::Transfer => continue,
                CategoryKind::Income | CategoryKind::Expense => {}
            }

            let totals = category_monthly_totals(&spend, &cats, id, today, fx);
            let fit = category_fit(&totals);
            // The baseline survives even when no trend could be fitted. Previously it came
            // only from a successful regression, so a category with a few months of history
            // dropped out of the simulation altogether — silently projecting *no* spending
            // on it, which is a worse error than projecting it flat.
            //
            // The volatility survives too. `category_fit` reports a growth of 0 when the
            // slope isn't distinguishable from flat, but the month-to-month scatter around
            // that flat line is measured and real — gating it on a *trend* having been
            // fitted would claim a category's spend is known to the cent.
            let baseline_minor = fit.map(|f| fx.base_minor(f.baseline));
            let derived = fit.map(|f| (f.growth_bps, f.vol_bps));

            let ov = overrides.get(&(ForecastTargetType::Category, id));
            let (growth, vol, source) = resolve_growth(
                ov.and_then(|o| o.annual_growth_bps),
                ov.and_then(|o| o.annual_volatility_bps),
                None,
                derived,
            );

            out.push(ResolvedAssumption {
                target_type: ForecastTargetType::Category,
                target_id: id,
                label: cats.name_of(id),
                annual_growth_bps: growth,
                annual_volatility_bps: vol,
                long_run_growth_bps: ov.and_then(|o| o.long_run_growth_bps).unwrap_or(0),
                // A category has no balance to charge a fee against.
                annual_fee_bps: None,
                annual_fixed_fee_minor: None,
                dividend_yield_bps: None,
                baseline_minor,
                schedule: None,
                currency_code: None,
                source,
            });
        }
        Ok(out)
    }

    /// Trailing-12-month dividend cash ÷ average account value over that window,
    /// annualised (the window already is 12 months, so no further scaling needed).
    async fn derive_dividend_yield(
        &self,
        account_id: i64,
        today: NaiveDate,
        series: &[(NaiveDate, f64)],
    ) -> AppResult<i64> {
        let since = today - chrono::Duration::days(365);
        let trailing = self
            .forecast
            .trailing_dividends_minor(account_id, &since.to_string())
            .await?;
        if trailing <= 0 {
            return Ok(0);
        }
        let recent: Vec<f64> = series
            .iter()
            .filter(|(d, _)| *d >= since)
            .map(|(_, v)| *v)
            .collect();
        let avg_value = if recent.is_empty() {
            series.last().map(|(_, v)| *v).unwrap_or(0.0)
        } else {
            recent.iter().sum::<f64>() / recent.len() as f64
        };
        if avg_value <= 0.0 {
            return Ok(0);
        }
        Ok(((trailing as f64 / avg_value) * 10_000.0).round() as i64)
    }

    // ---- thin CRUD passthrough — no orchestration, so it lives directly on the repo
    // port rather than duplicating logic here; kept on ForecastService so
    // routes/forecast.rs has one handle for everything forecast-related. ----

    pub async fn upsert_assumption(
        &self,
        input: sure_core::SaveForecastAssumption,
    ) -> AppResult<ForecastAssumption> {
        self.forecast.upsert_assumption(input).await
    }

    pub async fn clear_assumption(
        &self,
        target_type: ForecastTargetType,
        target_id: i64,
    ) -> AppResult<()> {
        self.forecast.clear_assumption(target_type, target_id).await
    }

    pub async fn list_events(&self) -> AppResult<Vec<ForecastEvent>> {
        self.forecast.list_events().await
    }

    pub async fn get_event(&self, id: i64) -> AppResult<ForecastEvent> {
        self.forecast.get_event(id).await
    }

    pub async fn create_event(
        &self,
        input: sure_core::SaveForecastEvent,
    ) -> AppResult<ForecastEvent> {
        self.forecast.create_event(input).await
    }

    pub async fn update_event(
        &self,
        id: i64,
        input: sure_core::SaveForecastEvent,
    ) -> AppResult<ForecastEvent> {
        self.forecast.update_event(id, input).await
    }

    pub async fn delete_event(&self, id: i64) -> AppResult<()> {
        self.forecast.delete_event(id).await
    }

    pub async fn list_income_streams(&self) -> AppResult<Vec<sure_core::IncomeStream>> {
        self.forecast.list_income_streams().await
    }

    pub async fn get_income_stream(&self, id: i64) -> AppResult<sure_core::IncomeStream> {
        self.forecast.get_income_stream(id).await
    }

    pub async fn create_income_stream(
        &self,
        person_id: i64,
        input: sure_core::SaveIncomeStream,
    ) -> AppResult<sure_core::IncomeStream> {
        self.forecast.create_income_stream(person_id, input).await
    }

    pub async fn update_income_stream(
        &self,
        id: i64,
        input: sure_core::SaveIncomeStream,
    ) -> AppResult<sure_core::IncomeStream> {
        self.forecast.update_income_stream(id, input).await
    }

    pub async fn delete_income_stream(&self, id: i64) -> AppResult<()> {
        self.forecast.delete_income_stream(id).await
    }

    pub async fn list_tax_scales(&self) -> AppResult<Vec<sure_core::StoredTaxScale>> {
        self.forecast.list_tax_scales().await
    }

    pub async fn create_tax_scale(
        &self,
        input: sure_core::SaveTaxScale,
    ) -> AppResult<sure_core::StoredTaxScale> {
        self.forecast
            .create_tax_scale(sure_core::TaxScaleId::NzPaye, input)
            .await
    }

    pub async fn update_tax_scale(
        &self,
        id: i64,
        input: sure_core::SaveTaxScale,
    ) -> AppResult<sure_core::StoredTaxScale> {
        self.forecast.update_tax_scale(id, input).await
    }

    pub async fn delete_tax_scale(&self, id: i64) -> AppResult<()> {
        self.forecast.delete_tax_scale(id).await
    }

    pub async fn restore_tax_scales(&self) -> AppResult<Vec<sure_core::StoredTaxScale>> {
        self.forecast.restore_tax_scales().await
    }

    /// Salaries the ledger appears to contain, for someone about to record one by hand.
    ///
    /// Reads a two-year window, which is enough to see an annual payment twice and far more than
    /// enough for anything more frequent.
    pub async fn detect_income(
        &self,
        account_id: Option<i64>,
    ) -> AppResult<Vec<crate::detect::DetectedStream>> {
        let today = self.clock.today();
        let from = (today - chrono::Duration::days(730)).to_string();
        let txns = self.forecast.income_transactions(&from, account_id).await?;
        Ok(crate::detect::detect(&txns, today))
    }

    /// Monte Carlo projection: `params.simulations` independent monthly paths out to
    /// `params.horizon_months`, aggregated into percentile bands per month.
    ///
    /// The whole operation, loads and arithmetic together, for every caller that is not a
    /// request handler — tests, the scheduler, anything holding the service directly. It is
    /// exactly [`Self::simulate_inputs`] followed by [`Self::simulate_from`]; `GET
    /// /api/forecast` calls those two halves itself so the second one can run on the
    /// blocking pool instead of on an async worker (see [`SimulationInputs`]). Both routes
    /// through the code do identical arithmetic in identical order, which
    /// `simulate_matches_the_two_step_split` pins for a fixed seed.
    pub async fn simulate(&self, params: &SimulationParams) -> AppResult<ForecastResult> {
        let inputs = self.simulate_inputs(params).await?;
        Self::simulate_from(inputs)
    }

    /// The awaiting half of [`Self::simulate`]: everything that reads the database, plus the
    /// per-account/per-category setup that needs an `await` to build.
    ///
    /// Returns an owned, `Send + 'static` bundle so the caller can hand the arithmetic to a
    /// thread that is allowed to block. Nothing here is expensive in CPU; the loads dominate,
    /// and they are all `await`s, so a runtime worker can park on them like any other query.
    pub async fn simulate_inputs(&self, params: &SimulationParams) -> AppResult<SimulationInputs> {
        let _phase = sure_telemetry::instruments::ReportPhase::load("forecast");
        let today = self.clock.today();
        let horizon = params
            .horizon_months
            .clamp(MIN_HORIZON_MONTHS, MAX_HORIZON_MONTHS);
        // Two independent limits. The first is what a caller may ask for; the second is what the
        // horizon can afford (see `MAX_PATH_MONTHS`). At any horizon up to the old 60-month
        // ceiling the budget allows 12 000 paths, so it cannot bind and nothing that was legal
        // before is affected.
        let requested = params.simulations.clamp(MIN_SIMULATIONS, MAX_SIMULATIONS);
        let budgeted = (MAX_PATH_MONTHS / horizon).max(MIN_SIMULATIONS);
        let n_paths = requested.min(budgeted) as usize;

        let (base, fx) = self.currency_and_fx(params.currency.as_deref()).await?;

        let mut assumptions = self.resolved_assumptions_with(&fx).await?;
        // Loaded before `by_target`, because which accounts receive payroll contributions decides
        // whether their fitted rate may be used at all — and that has to be settled before the
        // account projections are built from it.
        let streams = self.forecast.list_income_streams().await?;
        let mut warnings: Vec<String> = Vec::new();
        let mut contribution_targets: HashMap<i64, &'static str> = HashMap::new();
        for st in streams.iter().filter(|s| s.enabled) {
            if let Some(id) = st.kiwisaver_account_id {
                contribution_targets.insert(id, "KiwiSaver contributions");
            }
            if let Some(id) = st.student_loan_account_id {
                contribution_targets.insert(id, "student loan repayments");
            }
        }
        for a in &mut assumptions {
            if a.target_type != ForecastTargetType::Account {
                continue;
            }
            let Some(&what) = contribution_targets.get(&a.target_id) else {
                continue;
            };
            // A deterministic mortgage/loan has no fitted rate to discard, so there is nothing to
            // guard against — and overriding its schedule would be strictly worse.
            if a.source == AssumptionSource::Deterministic {
                continue;
            }
            let asserted = a.source == AssumptionSource::Override;
            if !asserted {
                // The fitted rate contains the contributions. Drop it rather than double count.
                a.annual_growth_bps = a.long_run_growth_bps;
                a.source = AssumptionSource::ContributionDriven;
                if a.long_run_growth_bps == 0 {
                    warnings.push(format!(
                        "{} now receives {what}, so its own measured growth rate was discarded — a \
                         balance that rose while money was flowing in cannot tell the two apart. It \
                         is projected flat until you set an expected return on it.",
                        a.label
                    ));
                }
            }
        }

        let by_target: HashMap<(ForecastTargetType, i64), &ResolvedAssumption> = assumptions
            .iter()
            .map(|a| ((a.target_type, a.target_id), a))
            .collect();
        let events = self.forecast.list_events().await?;

        let mut accounts = self.accounts.list(false).await?;
        // Filtered at the load, not inside the loop below, because this one vector feeds two
        // consumers: `account_sims` here and `cash_start` further down. Cash never reaches
        // `account_sims` at all (the `continue` below skips it), so filtering in the loop
        // would leave an excluded *bank* account fully inside the projection.
        //
        // Out of net worth has to mean out of the *projection* of net worth: the Forecast page
        // draws its history from `/api/reports/net-worth` and its projection from here, on one
        // axis meeting at today. Filter only one and the seam becomes a step exactly the size
        // of the excluded account — the mirror image of the defect recorded further down, where
        // leaving an account out of both made the projection start above today's reported
        // net worth.
        accounts.retain(|a| !a.excluded_from_net_worth);
        let (tx_by_acct, val_by_acct) = reports::load_ledger(self.reports.as_ref()).await?;

        let mut account_sims = Vec::new();
        for a in &accounts {
            let class = a.kind.class();
            if class == AccountClass::Cash
                || (class == AccountClass::Liability
                    && !matches!(
                        a.kind,
                        AccountKind::Mortgage | AccountKind::Loan | AccountKind::StudentLoan
                    ))
            {
                continue;
            }

            // `None` for the same reason `try_base_scale` returns it below — a currency with no
            // rate, here one the account *holds* rather than the one it is quoted in. Same
            // outcome: out of the simulation, named in `unconverted`.
            let Some((current_minor, _)) = reports::account_value_at(
                a.id,
                &a.currency_code,
                today,
                &fx,
                &tx_by_acct,
                &val_by_acct,
            ) else {
                continue;
            };
            let current = current_minor as f64;

            // Resolved once per account, not once per (path × month × account): the whole
            // projection is carried in native units and only converted for the monthly
            // totals. `None` means no rate reaches the projection currency, so the account is
            // out of the simulation entirely and its currency is reported in `unconverted` —
            // a starting balance taken at parity would be wrong in every month of every path.
            let Some(base_scale) = fx.try_base_scale(&a.currency_code) else {
                continue;
            };

            let (projection, monthly_drift) = if let Some(terms) = loan_terms(&a.metadata, today) {
                (AccountProjection::Deterministic(terms), Vec::new())
            } else {
                let resolved = by_target.get(&(ForecastTargetType::Account, a.id));
                let annual_growth = resolved.map(|r| r.annual_growth_bps).unwrap_or(0);
                let annual_vol = resolved.map(|r| r.annual_volatility_bps).unwrap_or(0);
                // The fee comes off the growth rate, which is what a percentage fee is. Subtracted
                // as a *log* return rather than from the annual bps, so 6% gross less a 1.05% fee
                // compounds to exactly what 4.95% net would — the two differ by a few basis points
                // a year otherwise, and over thirty years that is visible.
                let fee_bps = resolved.and_then(|r| r.annual_fee_bps).unwrap_or(0);
                let fee_log = annual_rate_to_monthly_log_return(fee_bps);
                let drift: Vec<f64> = drift_series(
                    annual_growth,
                    resolved.and_then(|r| long_run_anchor(r)),
                    horizon,
                )
                .into_iter()
                .map(|r| r - fee_log)
                .collect();
                let monthly_vol = annual_vol_to_monthly_sd(annual_vol);
                if class == AccountClass::Liability {
                    // Project a debt the same way its rate was measured. `derive_account_rate`
                    // fits a liability with a *linear* trend (a log-return is undefined once a
                    // balance crosses zero), so applying that fit as a compounding rate is a
                    // different model from the one the data supported: an exponential decay
                    // approaches zero without ever arriving, leaving a loan that is genuinely
                    // three years from being cleared still showing a balance a decade out.
                    // Converting the rate back into the dollars-per-month it was fitted from
                    // keeps the two consistent, and lets the debt actually finish.
                    //
                    // The conversion is per month because the rate now is: `current` is the
                    // balance the fit was taken against and stays fixed, so a decaying rate
                    // becomes a decaying dollars-per-month, which is the same fit expressed in
                    // the same unit it was measured in.
                    (
                        AccountProjection::LinearPaydown {
                            monthly_vol_abs: current.abs() * monthly_vol,
                        },
                        drift
                            .iter()
                            .map(|r| current * (r.exp() - 1.0))
                            .collect::<Vec<f64>>(),
                    )
                } else {
                    (AccountProjection::Stochastic { monthly_vol }, drift)
                }
            };

            // Event effects reach an account through the per-path overlay, not through the sim —
            // they are per-path now. A deterministic mortgage/loan still takes none: it projects
            // from its own terms alone.
            let takes_events = !matches!(projection, AccountProjection::Deterministic(_));

            account_sims.push(AccountSim {
                base_scale,
                current,
                projection,
                monthly_drift,
                // Exactly the kinds whose own ledger rows are kept out of the income/
                // expense report: that exclusion is what guarantees the repayment isn't
                // already inside a category baseline. `StudentLoan` is excluded from
                // spend too but must not debit — see the field's doc comment. Since it
                // acquired its own schedule-less profile that exclusion is belt-and-braces
                // (this flag is only read on the deterministic branch, which a student loan
                // can no longer reach), and it is kept because it costs nothing and states
                // the intent where the reader is looking.
                repayment_debits_cash: reports::is_excluded_from_spend(a.kind)
                    && a.kind != AccountKind::StudentLoan,
                monthly_fixed_fee: by_target
                    .get(&(ForecastTargetType::Account, a.id))
                    .and_then(|r| r.annual_fixed_fee_minor)
                    .unwrap_or(0) as f64
                    / 12.0,
                account_id: a.id,
                takes_events,
            });
        }

        // ---- income streams ------------------------------------------------------------
        //
        // Built before `category_sims`, because what a stream models has to be netted out of the
        // category it lands in before that category's baseline is fixed. Without that, a salary
        // recorded here *and* fitted from the bank statement is counted twice.
        // The stored scales, resolved once for the whole run rather than per stream per month.
        let tax_scales = crate::income::TaxScales::new(&self.forecast.list_tax_scales().await?);
        let mut stream_sims: Vec<StreamSim> = Vec::new();
        let mut unmodelled_streams: Vec<String> = Vec::new();
        // Modelled monthly net per linked category, base-currency major units.
        let mut modelled_by_category: HashMap<i64, (i64, f64)> = HashMap::new();

        // A person's brackets are progressive over their *total* gross, so the level every gross
        // stream is taxed against is the sum of them — pricing each alone would tax each as if the
        // other did not exist and under-tax both.
        let mut person_gross: HashMap<i64, i64> = HashMap::new();
        for st in streams.iter().filter(|s| s.enabled && s.basis.is_gross()) {
            let (level, _, _, _) = crate::income::level_schedule(st, today, horizon);
            *person_gross.entry(st.person_id).or_default() += level as i64;
        }

        for st in &streams {
            if !st.enabled {
                continue;
            }
            // Same argument as an account: a stream in a currency with no rate to the projection
            // currency is left out and named, never counted at parity.
            let Some(base_scale) = fx.try_base_scale(&st.currency_code) else {
                unmodelled_streams.push(format!(
                    "{} — no exchange rate from {} to {base}",
                    st.label, st.currency_code
                ));
                continue;
            };
            let Some(anchor) = reports::parse_date(&st.first_payment_on) else {
                unmodelled_streams.push(format!("{} — unreadable first payment date", st.label));
                continue;
            };
            // Outside the projection window entirely: left out rather than included paying zero,
            // so it cannot be mistaken for a stream that earns nothing.
            let Some((active_from, active_to)) = crate::income::active_window(st, today, horizon)
            else {
                continue;
            };
            let (start_level, steps, residual_from_month, monthly_increase) =
                crate::income::level_schedule(st, today, horizon);
            let gross_total = person_gross.get(&st.person_id).copied().unwrap_or(0);
            let take_home = crate::income::take_home(st, gross_total, today, &tax_scales);
            let (kiwisaver_fraction, student_loan_fraction) =
                crate::income::contribution_rates(st, gross_total, today, &tax_scales);

            // The contribution is in the stream's currency and the balance is in the account's, so
            // the factor is one over the other — both are already native-minor-to-base-major.
            let mut target = |account_id: Option<i64>, label: &str| -> Option<(usize, f64)> {
                let id = account_id?;
                match account_sims.iter().position(|s| s.account_id == id) {
                    Some(i) => Some((i, base_scale / account_sims[i].base_scale)),
                    None => {
                        // Cash accounts are pooled rather than simulated, and an unconvertible one is
                        // left out entirely — so a link can point at something that has no slot. The
                        // money is left where it was rather than credited to the wrong place.
                        warnings.push(format!(
                            "{}'s {label} are not being tracked: the account it points at is not in \
                             the projection (a pooled cash account, or one with no exchange rate).",
                            st.label
                        ));
                        None
                    }
                }
            };
            let kiwisaver_target = target(st.kiwisaver_account_id, "KiwiSaver contributions");
            let student_loan_target = target(st.student_loan_account_id, "student loan repayments");

            // The monthly net this stream claims, for netting and for the reconciliation. Annual
            // over twelve, not this month's paydays: a category baseline is a monthly *average*, so
            // that is the like-for-like comparison.
            let annual_net = take_home.net_annual(start_level, start_level);
            let monthly_net_base = annual_net / 12.0 * base_scale;
            if let Some(cat) = st.linked_category_id {
                let entry = modelled_by_category
                    .entry(cat)
                    .or_insert((st.person_id, 0.0));
                entry.1 += monthly_net_base;
            }

            stream_sims.push(StreamSim {
                person_id: st.person_id,
                stream_id: st.id,
                base_scale,
                payments: crate::income::payment_counts(st.pay_frequency, anchor, today, horizon),
                periods_per_year: st.pay_frequency.periods_per_year(),
                start_level,
                calibrated_level: start_level,
                steps,
                monthly_increase,
                residual_from_month,
                active_from,
                active_to,
                take_home,
                kiwisaver_target,
                student_loan_target,
                kiwisaver_fraction,
                student_loan_fraction,
            });
        }

        let mut category_sims = Vec::new();
        let mut reconciliations: Vec<StreamReconciliation> = Vec::new();
        for a in &mut assumptions {
            if a.target_type != ForecastTargetType::Category {
                continue;
            }
            // Net the streams out of the category they land in, and report the pair. The residual
            // — not zero — is what the fitted trend still projects: excluding the category outright
            // would silently drop the income the streams do not explain (interest, a gift, a second
            // job nobody modelled).
            if let Some(&(person_id, modelled)) = modelled_by_category.get(&a.target_id) {
                let observed = a
                    .baseline_minor
                    .and_then(|m| fx.try_to_base_major(m, &base))
                    .unwrap_or(0.0);
                let residual = (observed - modelled).max(0.0);
                reconciliations.push(StreamReconciliation {
                    person_id,
                    category_id: a.target_id,
                    category_label: a.label.clone(),
                    modelled_net_minor: fx.base_minor(modelled),
                    observed_net_minor: fx.base_minor(observed),
                    coverage_bps: if observed > 0.0 {
                        (modelled / observed * 10_000.0).round() as i64
                    } else {
                        0
                    },
                    residual_minor: fx.base_minor(residual),
                });
                a.baseline_minor = Some(fx.base_minor(residual));
                a.source = AssumptionSource::ModelledFromIncome;
            }
            let Some(baseline_minor) = a.baseline_minor else {
                continue; // no derived trend to anchor to — contributes nothing
            };
            // Already a base-currency figure (see `category_monthly_totals`), so this only
            // rescales minor→major; it can still fail if the base currency itself has no
            // `currencies` row, in which case there is no projection to run.
            let Some(baseline) = fx.try_to_base_major(baseline_minor, &base) else {
                continue;
            };
            category_sims.push(CategorySim {
                is_income: source_kind_is_income(self, a.target_id).await?,
                baseline,
                monthly_log_return: drift_series(a.annual_growth_bps, long_run_anchor(a), horizon),
                monthly_vol_fraction: annual_vol_to_monthly_sd(a.annual_volatility_bps),
                category_id: a.target_id,
            });
        }

        // ---- events, in topological order -----------------------------------------------
        //
        // Sorted so a per-path `after` clamp can read its parent's already-sampled month by index.
        // A cycle surviving to here means the write-time check was bypassed (a hand-edited
        // database), so the members are logged and processed unconstrained rather than failing the
        // whole forecast — `metadata_from_stored` sets the precedent: coerce, do not panic.
        let event_sims = resolve_events(&events, today, horizon);

        // Drawn here, on the async side, and carried into the compute half by value. It must
        // *not* move inside `simulate_from`: a caller that passes an explicit seed is asking
        // for a reproducible run, and re-drawing per invocation would make the same
        // `SimulationInputs` produce different numbers each time it were used.
        let seed = params.seed.unwrap_or_else(rand::random);

        Ok(SimulationInputs {
            accounts,
            today,
            tx_by_acct,
            val_by_acct,
            fx,
            base,
            horizon,
            n_paths,
            seed,
            assumptions,
            account_sims,
            category_sims,
            stream_sims,
            event_sims,
            warnings,
            reconciliations,
            unmodelled_streams,
        })
    }

    /// The synchronous half of [`Self::simulate`]: the Monte Carlo loop and the percentile
    /// aggregation, over inputs already loaded by [`Self::simulate_inputs`].
    ///
    /// Free of `self`, free of `.await`, and therefore safe to run on the blocking pool —
    /// which is the point. `simulations × horizon_months × accounts` random draws is tens of
    /// milliseconds to seconds of *uninterrupted* CPU, and on an async worker that is a
    /// thread the runtime cannot use for anything else in the meantime: on a four-worker box
    /// four concurrent `GET /api/forecast`s stop the whole process — no connections accepted,
    /// `/api/health` silent, no scheduler tick, no shutdown watcher — and no external failure
    /// is needed to get there, since one dashboard load fans out several report calls. It also
    /// makes the request deadline real: `tokio::time::timeout` can only fire at an `.await`
    /// inside the future it wraps, so while this ran inline the timeout was not observed until
    /// the work had already finished and the completed response was thrown away.
    ///
    /// The RNG is owned (`StdRng::seed_from_u64`), never thread-local, so which thread runs
    /// this cannot change a single figure.
    pub fn simulate_from(inputs: SimulationInputs) -> AppResult<ForecastResult> {
        // Its own histogram rather than `report_duration`'s `compute` phase: this is the
        // heaviest computation in the application and its distribution has nothing in common
        // with a report's, so sharing buckets would flatten both.
        let _timer = sure_telemetry::instruments::Timer::new(
            &sure_telemetry::instruments().forecast_duration,
            Vec::new(),
        );
        let SimulationInputs {
            accounts,
            today,
            tx_by_acct,
            val_by_acct,
            fx,
            base,
            horizon,
            n_paths,
            seed,
            assumptions,
            account_sims,
            category_sims,
            stream_sims,
            event_sims,
            warnings,
            reconciliations,
            unmodelled_streams,
        } = inputs;

        let cash_start: f64 = accounts
            .iter()
            .filter(|a| {
                a.kind.class() == AccountClass::Cash
                    // A card or a revolving facility is an everyday transaction account,
                    // not a valued instrument — the same argument that keeps it out of
                    // `account_sims` (it has no growth rate) puts it *in* the pool the
                    // category cash flow drives. Leaving it out of both, as before, made
                    // its balance invisible to the forecast entirely, so the projection
                    // started above the net worth the reports show for today.
                    || matches!(
                        a.kind,
                        AccountKind::CreditCard | AccountKind::RevolvingCredit
                    )
            })
            // An unconvertible cash account is left out of the pool (and named in
            // `unconverted`) on the same argument as `account_sims` above.
            .filter_map(|a| {
                let (v, ccy) = reports::account_value_at(
                    a.id,
                    &a.currency_code,
                    today,
                    &fx,
                    &tx_by_acct,
                    &val_by_acct,
                )?;
                fx.try_to_base_major(v, &ccy)
            })
            .sum();

        let mut rng = StdRng::seed_from_u64(seed);

        let mut month_samples: Vec<MonthSamples> = (0..horizon)
            .map(|_| MonthSamples {
                assets: Vec::with_capacity(n_paths),
                liabilities: Vec::with_capacity(n_paths),
                net_worth: Vec::with_capacity(n_paths),
            })
            .collect();
        // Household net income per month, per path — a band, because events will make it one.
        let mut income_samples: Vec<Vec<f64>> =
            (0..horizon).map(|_| Vec::with_capacity(n_paths)).collect();
        // Paths whose cash pool was negative, per month. A count rather than samples: the answer
        // is a single fraction, so there is nothing to take percentiles of.
        let mut negative_cash: Vec<u32> = vec![0; horizon as usize];

        // Sampled before the path loop, from RNGs seeded independently of `rng`. That independence
        // is the acceptance criterion for this whole feature: with no events configured, not one
        // value is taken from the shared stream, so every figure is byte-identical to a run from
        // before events existed.
        let event_outcomes = sample_event_outcomes(seed, n_paths, horizon, &event_sims);
        let mut overlay = Overlay::new(account_sims.len(), category_sims.len());
        // Scratch, reused across paths: the promotions this path drew, and the published scale
        // merged with them in month order.
        let mut path_steps: Vec<Vec<(i64, f64)>> = vec![Vec::new(); stream_sims.len()];
        let mut path_levels: Vec<Vec<(i64, f64)>> = vec![Vec::new(); stream_sims.len()];

        for outcomes in &event_outcomes {
            let mut acc_values: Vec<f64> = account_sims.iter().map(|s| s.current).collect();
            let mut cat_baselines: Vec<f64> = category_sims.iter().map(|s| s.baseline).collect();
            let mut cash = cash_start;

            apply_due(&mut acc_values, &overlay, 0);
            for (i, base) in cat_baselines.iter_mut().enumerate() {
                if let Some(&(_, val)) = overlay.cat_step[i].iter().find(|&&(idx, _)| idx == 0) {
                    *base = val;
                }
            }

            // Per-path income state. Deterministic today — every path runs the same schedule — but
            // per-path from the start, because a promotion or a career break moves the level on some
            // paths and not others, and retrofitting that into shared state is how a path leaks into
            // its neighbour.
            let mut stream_levels: Vec<f64> = stream_sims.iter().map(|s| s.start_level).collect();
            let mut stream_next_step: Vec<usize> = vec![0; stream_sims.len()];
            let mut stream_from: Vec<i64> = stream_sims.iter().map(|s| s.active_from).collect();
            let mut stream_to: Vec<i64> = stream_sims.iter().map(|s| s.active_to).collect();
            let mut stream_pauses: Vec<Vec<(i64, i64, i64)>> = vec![Vec::new(); stream_sims.len()];

            // ---- flatten this path's events -----------------------------------------------
            //
            // The order is load-bearing. Windows move first, so everything below is decided against
            // the window this path actually has; then levels, so a promotion on a not-yet-started
            // job still raises the salary it will pay; then pauses; then costs. Doing levels before
            // windows would make "promotion, then a new job" stop composing.
            overlay.clear();
            for v in path_steps.iter_mut() {
                v.clear();
            }
            for (ei, ev) in event_sims.iter().enumerate() {
                let Some(month) = outcomes[ei].month else {
                    continue;
                };
                if month > horizon {
                    // Occurred, but after the projection ends. Reported as such; applied to nothing.
                    continue;
                }
                for effect in &ev.effects {
                    match *effect {
                        LifeEffectSpec::IncomeStart { income_stream_id } => {
                            if let Some(i) = stream_index(&stream_sims, income_stream_id) {
                                stream_from[i] = month;
                                stream_to[i] = stream_to[i].max(horizon);
                            }
                        }
                        LifeEffectSpec::IncomeEnd { income_stream_id } => {
                            if let Some(i) = stream_index(&stream_sims, income_stream_id) {
                                stream_to[i] = stream_to[i].min(month - 1);
                            }
                        }
                        LifeEffectSpec::IncomeStep {
                            income_stream_id,
                            amount,
                        } => {
                            if let Some(i) = stream_index(&stream_sims, income_stream_id) {
                                // Merged into the dated scale rather than applied here, so the two
                                // resolve in month order together and a published step and a
                                // promotion in the same month cannot both win.
                                let at = |base: f64| match amount {
                                    StepAmount::Absolute {
                                        annual_amount_minor,
                                    } => annual_amount_minor as f64,
                                    StepAmount::Percent { rate_bps } => {
                                        base * (1.0 + rate_bps as f64 / 10_000.0)
                                    }
                                };
                                let base = stream_sims[i]
                                    .steps
                                    .iter()
                                    .rev()
                                    .find(|&&(sm, _)| sm <= month)
                                    .map(|&(_, v)| v)
                                    .unwrap_or(stream_sims[i].start_level);
                                path_steps[i].push((month, at(base)));
                            }
                        }
                        LifeEffectSpec::IncomePause {
                            person_id,
                            months,
                            replacement_rate_bps,
                        } => {
                            // Every stream this person has — nobody takes parental leave from one of
                            // their two jobs. Overlapping pauses take the *lower* replacement rate:
                            // adding them could pay more than 100% of a salary nobody is earning.
                            for (i, sim) in stream_sims.iter().enumerate() {
                                if sim.person_id == person_id {
                                    stream_pauses[i].push((
                                        month,
                                        month + months - 1,
                                        replacement_rate_bps,
                                    ));
                                }
                            }
                        }
                        LifeEffectSpec::RecurringDelta {
                            category_id,
                            amount_minor,
                            delay_months,
                            ramp_months,
                            duration_months,
                        } => {
                            if let Some(c) = category_index(&category_sims, category_id) {
                                let from = month + delay_months;
                                overlay.deltas.push(ActiveDelta {
                                    category: c,
                                    from,
                                    to: duration_months.map(|d| from + d - 1),
                                    amount: amount_minor as f64 / 10f64.powi(fx.dp(&base)),
                                    ramp: ramp_months,
                                });
                            }
                        }
                        LifeEffectSpec::SetBaseline {
                            target,
                            amount_minor,
                        } => match target {
                            EffectTarget::Account { account_id } => {
                                if let Some(i) = account_index(&account_sims, account_id) {
                                    overlay.acc_step[i].push((month, amount_minor as f64));
                                }
                            }
                            EffectTarget::Category { category_id } => {
                                if let Some(c) = category_index(&category_sims, category_id) {
                                    overlay.cat_step[c].push((
                                        month,
                                        amount_minor as f64 / 10f64.powi(fx.dp(&base)),
                                    ));
                                }
                            }
                        },
                        LifeEffectSpec::OneOffAmount {
                            target,
                            amount_minor,
                        } => match target {
                            EffectTarget::Account { account_id } => {
                                if let Some(i) = account_index(&account_sims, account_id) {
                                    overlay.acc_one[i].push((month, amount_minor as f64));
                                }
                            }
                            EffectTarget::Category { category_id } => {
                                if let Some(c) = category_index(&category_sims, category_id) {
                                    overlay.cat_one[c].push((
                                        month,
                                        amount_minor as f64 / 10f64.powi(fx.dp(&base)),
                                    ));
                                }
                            }
                        },
                    }
                }
            }
            // Promotions merged with the published scale, in month order.
            for (i, extra) in path_steps.iter_mut().enumerate() {
                path_levels[i].clear();
                path_levels[i].extend_from_slice(&stream_sims[i].steps);
                path_levels[i].append(extra);
                path_levels[i].sort_by_key(|&(m, _)| m);
            }

            // Per-path repayment state, parallel to `acc_values` (`None` for every
            // non-amortising account). Opening a schedule draws this path's post-refix
            // rate, so it happens here, once per path, in `account_sims` order.
            let mut schedules: Vec<Option<AmortSchedule>> = account_sims
                .iter()
                .enumerate()
                .map(|(i, sim)| match sim.projection {
                    AccountProjection::Deterministic(terms) => {
                        Some(AmortSchedule::open(&terms, acc_values[i], today, &mut rng))
                    }
                    AccountProjection::Stochastic { .. }
                    | AccountProjection::LinearPaydown { .. } => None,
                })
                .collect();

            for m in 1..=horizon {
                // Base-currency major units, like `cash`.
                let mut repayments = 0.0;
                for (i, sim) in account_sims.iter().enumerate() {
                    match sim.projection {
                        AccountProjection::Deterministic(_) => {
                            let Some(schedule) = schedules[i].as_mut() else {
                                continue;
                            };
                            let paid = schedule.advance(m);
                            acc_values[i] = schedule.signed_balance();
                            if sim.repayment_debits_cash {
                                repayments += paid.cash_out() * sim.base_scale;
                            }
                        }
                        AccountProjection::Stochastic { monthly_vol } => {
                            if let Some(&(_, val)) =
                                overlay.acc_step[i].iter().find(|&&(idx, _)| idx == m)
                            {
                                acc_values[i] = val;
                            } else {
                                let noise = if monthly_vol > 0.0 {
                                    Normal::new(0.0, monthly_vol).unwrap().sample(&mut rng)
                                } else {
                                    0.0
                                };
                                acc_values[i] *= (sim.monthly_drift[m as usize] + noise).exp();
                            }
                            acc_values[i] += overlay.acc_one[i]
                                .iter()
                                .filter(|&&(idx, _)| idx == m)
                                .map(|&(_, d)| d)
                                .sum::<f64>();
                        }
                        AccountProjection::LinearPaydown { monthly_vol_abs } => {
                            if let Some(&(_, val)) =
                                overlay.acc_step[i].iter().find(|&&(idx, _)| idx == m)
                            {
                                acc_values[i] = val;
                            } else {
                                let noise = if monthly_vol_abs > 0.0 {
                                    Normal::new(0.0, monthly_vol_abs).unwrap().sample(&mut rng)
                                } else {
                                    0.0
                                };
                                acc_values[i] += sim.monthly_drift[m as usize] + noise;
                            }
                            acc_values[i] += overlay.acc_one[i]
                                .iter()
                                .filter(|&&(idx, _)| idx == m)
                                .map(|&(_, d)| d)
                                .sum::<f64>();
                            // Cleared. A debt paid off is paid off — it must not run past
                            // zero into being an asset, since the assets/liabilities split
                            // is by sign and noise alone would otherwise push it across.
                            if acc_values[i] > 0.0 {
                                acc_values[i] = 0.0;
                            }
                        }
                    }
                }

                let mut net_flow = 0.0;
                for (i, sim) in category_sims.iter().enumerate() {
                    if let Some(&(_, new_baseline)) =
                        overlay.cat_step[i].iter().find(|&&(idx, _)| idx == m)
                    {
                        cat_baselines[i] = new_baseline;
                    } else {
                        // The run-rate drifts by its trend alone. Crucially the month's
                        // noise is *not* folded back into it: doing so made the baseline a
                        // random walk, so one expensive January permanently raised the
                        // projected food budget for every month after it, and the spread
                        // compounded without bound (a ±$600k band five years out). Household
                        // spending is lumpy but mean-reverting — you spend about the same on
                        // food each year whichever months it lands in — so the lumpiness
                        // belongs on the month, not on the estimate.
                        cat_baselines[i] *= sim.monthly_log_return[m as usize].exp();
                    }
                    // A one-off due this month, plus any recurring cost an event switched on. Both
                    // land on the *realised* month rather than on the baseline: a daycare invoice is
                    // a known amount, and folding it into the run-rate would put the category's
                    // lognormal lumpiness on top of a fee that is not lumpy, then compound it through
                    // the drift — the exact mistake the comment above exists to prevent.
                    let one_off: f64 = overlay.cat_one[i]
                        .iter()
                        .filter(|&&(idx, _)| idx == m)
                        .map(|&(_, d)| d)
                        .sum::<f64>()
                        + overlay.delta_at(i, m);
                    // What this month actually happens to cost. Lognormal, scaled so its
                    // *mean* is exactly the run-rate: spending can't go negative, and the
                    // old `max(0.0)` clip on a symmetric draw silently inflated every lumpy
                    // category — with noise this wide, clipping the bottom half of the
                    // distribution overstated the mean by roughly 50%.
                    let realised = if sim.monthly_vol_fraction > 0.0 {
                        let sigma = sim.monthly_vol_fraction;
                        let z = Normal::new(0.0, sigma).unwrap().sample(&mut rng);
                        cat_baselines[i] * (z - 0.5 * sigma * sigma).exp()
                    } else {
                        cat_baselines[i]
                    };
                    let contribution = realised + one_off;
                    net_flow += if sim.is_income {
                        contribution
                    } else {
                        -contribution
                    };
                }
                // A flat fee is money leaving the account regardless of how it performed, so it is
                // charged after growth rather than folded into the rate. Not allowed to push a
                // balance below zero: a fund closes an emptied account, it does not invoice you.
                for (i, sim) in account_sims.iter().enumerate() {
                    if sim.monthly_fixed_fee > 0.0 && acc_values[i] > 0.0 {
                        acc_values[i] = (acc_values[i] - sim.monthly_fixed_fee).max(0.0);
                    }
                }

                // ---- income streams -----------------------------------------------------
                //
                // Base-currency major units, like `cash`. Ordered deliberately: the level moves
                // first (a pay-scale step effective this month pays at the new level *this*
                // month), then the residual increase, then the window gate, then the calendar.
                let mut stream_net = 0.0;
                for (i, sim) in stream_sims.iter().enumerate() {
                    while stream_next_step[i] < path_levels[i].len()
                        && path_levels[i][stream_next_step[i]].0 <= m
                    {
                        stream_levels[i] = path_levels[i][stream_next_step[i]].1;
                        stream_next_step[i] += 1;
                    }
                    if m > sim.residual_from_month {
                        stream_levels[i] *= sim.monthly_increase;
                    }
                    if m < stream_from[i] || m > stream_to[i] {
                        continue;
                    }
                    let paydays = f64::from(sim.payments[m as usize]);
                    if paydays == 0.0 {
                        continue;
                    }
                    let level = stream_levels[i];
                    let mut gross = paydays * level / sim.periods_per_year;
                    // A pause scales the *payout*, not the level: a promotion landing during parental
                    // leave still raises the salary you go back to.
                    if let Some(bps) = stream_pauses[i]
                        .iter()
                        .filter(|&&(a, b, _)| m >= a && m <= b)
                        .map(|&(_, _, bps)| bps)
                        .min()
                    {
                        gross *= bps as f64 / 10_000.0;
                    }
                    // The take-home *ratio* comes from the annual level and the month's amount from
                    // the calendar. Annualising the month instead would push a quarterly bonus into
                    // the top bracket for that month alone, which is not how PAYE works.
                    let net_annual = sim.take_home.net_annual(level, sim.calibrated_level);
                    let ratio = if level > 0.0 { net_annual / level } else { 0.0 };
                    stream_net += gross * ratio * sim.base_scale;

                    // The deductions no longer vanish. Over thirty years these two lines are most of
                    // a retirement balance and the whole of a student loan being cleared.
                    //
                    // Applied to the *paused* gross deliberately: nobody contributes to KiwiSaver or
                    // repays a student loan out of pay they are not receiving.
                    if let Some((i, scale)) = sim.kiwisaver_target {
                        acc_values[i] += gross * sim.kiwisaver_fraction * scale;
                    }
                    if let Some((i, scale)) = sim.student_loan_target {
                        // A loan balance is negative, so a repayment moves it *up* toward zero. The
                        // `LinearPaydown` arm clamps at zero, so an overpayment cannot turn a repaid
                        // loan into an asset.
                        acc_values[i] += gross * sim.student_loan_fraction * scale;
                        if acc_values[i] > 0.0 {
                            acc_values[i] = 0.0;
                        }
                    }
                }

                // Servicing the debt is real money leaving. Net worth therefore falls by
                // exactly the interest each month: the principal moves from cash to the
                // liability and nets out, the interest simply goes.
                cash += net_flow + stream_net - repayments;
                income_samples[(m - 1) as usize].push(stream_net);

                let mut assets = 0.0;
                let mut liabilities = 0.0;
                for (i, sim) in account_sims.iter().enumerate() {
                    let base_val = acc_values[i] * sim.base_scale;
                    if base_val >= 0.0 {
                        assets += base_val;
                    } else {
                        liabilities += base_val;
                    }
                }
                if cash >= 0.0 {
                    assets += cash;
                } else {
                    liabilities += cash;
                }

                let idx = (m - 1) as usize;
                // Counted, not acted on: the sign split above is unchanged, so no existing
                // figure moves. See `ForecastResult::negative_cash_rate_bps` for why a band
                // around net worth cannot answer this on its own.
                if cash < 0.0 {
                    negative_cash[idx] += 1;
                }
                month_samples[idx].assets.push(assets);
                month_samples[idx].liabilities.push(liabilities);
                month_samples[idx].net_worth.push(assets + liabilities);
            }
        }

        let mut months = Vec::with_capacity(horizon as usize);
        for m in 1..=horizon {
            let idx = (m - 1) as usize;
            months.push(ForecastMonth {
                as_of: add_months(today, m).to_string(),
                net_worth: band_from_samples(&mut month_samples[idx].net_worth, &fx),
                assets: band_from_samples(&mut month_samples[idx].assets, &fx),
                liabilities: band_from_samples(&mut month_samples[idx].liabilities, &fx),
            });
        }

        Ok(ForecastResult {
            currency: base,
            months,
            assumptions,
            unconverted: fx.unconverted(),
            rates_as_of: fx.rates_as_of().map(str::to_string),
            horizon_months: horizon,
            simulations: n_paths as i64,
            income_net: income_samples
                .iter_mut()
                .map(|s| band_from_samples(s, &fx))
                .collect(),
            events: summarise_events(&event_sims, &event_outcomes, today, horizon),
            reconciliations,
            warnings,
            unmodelled_streams,
            negative_cash_rate_bps: negative_cash
                .iter()
                .map(|&c| (c as i64) * 10_000 / n_paths.max(1) as i64)
                .collect(),
        })
    }
}

/// A category's flow direction. Kept as a tiny associated lookup (rather than stashing
/// `kind` on `ResolvedAssumption`, which every other caller has no use for) since only
/// `simulate` needs to know whether a category adds to or subtracts from cash flow.
async fn source_kind_is_income(svc: &ForecastService, category_id: i64) -> AppResult<bool> {
    let cats = reports::Categories::load(svc.reports.as_ref()).await?;
    Ok(cats.kind_of(category_id) == Some(CategoryKind::Income))
}

// ---- simulation building blocks --------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum AccountProjection {
    /// A mortgage/loan with a complete amortisation schedule. The terms are shared across
    /// every path; the state they drive — balance, current rate, and this path's drawn
    /// refix rate — is per-path, and lives in `simulate`'s `schedules` beside
    /// `acc_values`.
    Deterministic(LoanTerms),
    /// `value *= exp(drift + noise)` each month, where `drift` is that month's entry in
    /// [`AccountSim::monthly_drift`]. Assets and investments, whose rate is fitted as a
    /// compounding return in the first place.
    Stochastic { monthly_vol: f64 },
    /// `value += drift + noise` each month, stopping at zero, where `drift` is that month's
    /// entry in [`AccountSim::monthly_drift`] — there an absolute delta rather than a rate. A
    /// liability without a repayment schedule of its own: its rate is fitted as a straight
    /// line, so it is projected as one, and a debt being paid down reaches zero and stays
    /// there rather than shrinking by a fixed percentage forever.
    LinearPaydown {
        /// Absolute standard deviation of the monthly move, native minor units.
        monthly_vol_abs: f64,
    },
}

struct AccountSim {
    /// Multiplier from this account's native minor units to base-currency major units
    /// ([`Fx::try_base_scale`]), resolved before the simulation starts — an account whose
    /// currency has no rate never becomes an `AccountSim` at all.
    base_scale: f64,
    current: f64,
    projection: AccountProjection,
    /// This account's drift at each month, indexed `0..=horizon` (see [`drift_series`]).
    ///
    /// The unit depends on the projection, because the two models were fitted in different
    /// ones: a monthly log-return for [`AccountProjection::Stochastic`], and an absolute
    /// native-minor-unit delta for [`AccountProjection::LinearPaydown`]. Empty for
    /// [`AccountProjection::Deterministic`], which projects from its own terms and has no rate.
    ///
    /// A table rather than a scalar so a *derived* rate can decay past the window it was fitted
    /// over; for every other source every entry is the same value, and for any month within
    /// [`TREND_FULL_STRENGTH_MONTHS`] it is the value the scalar had.
    monthly_drift: Vec<f64>,
    /// Whether this loan's repayment should be debited from the projected cash pool.
    ///
    /// A mortgage's legs are already outside the category cash-flow model — the account
    /// kind is excluded from spend outright, and the bank-side legs are linked transfer
    /// legs — so debiting the payment introduces no double count. A student loan is the
    /// opposite case: repaid via PAYE, it is already inside the *net* salary the income
    /// baseline is fitted to, and there is no ledger leg that could reveal the overlap.
    /// For those, the paydown is projected but the cash side is left to the categories.
    ///
    /// Read only on the [`AccountProjection::Deterministic`] branch, which is why a student
    /// loan no longer depends on this flag for that correctness: having its own profile, it
    /// has no schedule to be deterministic *with* (see [`loan_terms`]), and a trend-projected
    /// account never debits the pool at all.
    repayment_debits_cash: bool,
    /// A flat fee charged against this account every month, native minor units — an annual
    /// membership fee spread evenly. The *percentage* fee is not here: it is folded into
    /// `monthly_drift`, because a proportional fee is arithmetically a reduction in the growth rate
    /// and modelling it twice over would be two places to get one number wrong.
    monthly_fixed_fee: f64,
    /// Which account this is, so an event effect naming it can be matched to this slot.
    account_id: i64,
    /// Whether event effects apply here at all.
    takes_events: bool,
}

struct CategorySim {
    is_income: bool,
    /// Base-currency major units (dollars) — the current fitted monthly run-rate.
    baseline: f64,
    /// Monthly log-return at each month, indexed `0..=horizon` (see [`drift_series`]). A table
    /// rather than a scalar so a derived trend can decay past the 24 months it was fitted over.
    monthly_log_return: Vec<f64>,
    /// Fraction of the (then-current) baseline, not yet scaled to an absolute $ stdev —
    /// scaled per-month so noise grows with the baseline over the horizon.
    monthly_vol_fraction: f64,
    /// Which category this is, so an event effect naming it can be matched to this slot.
    category_id: i64,
}

/// One income stream, resolved. Everything here is path-invariant; the *level* is not — a promotion
/// moves it — so that lives in the path loop beside `acc_values`.
struct StreamSim {
    /// Whose income this is. A career break pauses every stream one person has, so the effect has to
    /// be able to find them.
    person_id: i64,
    /// Which stream this is, so an effect naming it can be matched to this slot.
    stream_id: i64,
    /// Native minor units -> base-currency major units, resolved once. A stream whose currency has
    /// no rate never becomes a `StreamSim` at all; it is named in `unmodelled_streams`.
    base_scale: f64,
    /// Paydays landing in each month index, `0..=horizon`. Index 0 is always 0.
    payments: Vec<u8>,
    periods_per_year: f64,
    /// Annual level at month 0, native minor units, after any pay-scale step already in force.
    start_level: f64,
    /// The level the take-home map was calibrated at — the denominator its marginal rate prices
    /// increments above.
    calibrated_level: f64,
    /// `(month_index, new_annual_level)` from the dated pay scale, ascending. Deterministic on every
    /// path: a published scale is a certainty.
    steps: Vec<(i64, f64)>,
    /// `(1 + annual_increase)^(1/12)`, applied only from `residual_from_month` on — so a published
    /// scale and a typed-in annual rise cannot both apply to the same month.
    monthly_increase: f64,
    residual_from_month: i64,
    /// Inclusive month window from `starts_on`/`ends_on`, clamped into `1..=horizon`.
    active_from: i64,
    active_to: i64,
    take_home: sure_core::TakeHome,
    /// Slot in `account_sims` that KiwiSaver contributions land in, with the multiplier from this
    /// stream's native minor units to that account's. `None` when nothing is linked, in which case
    /// the money leaves the projection exactly as it did before.
    kiwisaver_target: Option<(usize, f64)>,
    /// Likewise for the student loan the deductions pay down.
    student_loan_target: Option<(usize, f64)>,
    /// Share of each gross dollar reaching the KiwiSaver account — employee plus employer, net of
    /// ESCT (see `crate::income::contribution_rates`).
    kiwisaver_fraction: f64,
    /// Share of each gross dollar deducted for the student loan.
    student_loan_fraction: f64,
}

struct MonthSamples {
    assets: Vec<f64>,
    liabilities: Vec<f64>,
    net_worth: Vec<f64>,
}

/// Apply this path's account-level event effects due in `month`.
///
/// Takes the per-path overlay rather than reading the sims, because event effects are per-path now:
/// two paths can disagree about whether a revaluation happened at all.
fn apply_due(values: &mut [f64], overlay: &Overlay, month: i64) {
    for (i, value) in values.iter_mut().enumerate() {
        if let Some(&(_, val)) = overlay.acc_step[i].iter().find(|&&(idx, _)| idx == month) {
            *value = val;
        }
        *value += overlay.acc_one[i]
            .iter()
            .filter(|&&(idx, _)| idx == month)
            .map(|&(_, d)| d)
            .sum::<f64>();
    }
}

/// One path's event effects, flattened into the shape the month loop indexes.
///
/// Reused across paths — cleared rather than reallocated, because a fresh set of vectors per path is
/// thousands of allocations per request for nothing.
struct Overlay {
    acc_step: Vec<Vec<(i64, f64)>>,
    acc_one: Vec<Vec<(i64, f64)>>,
    cat_step: Vec<Vec<(i64, f64)>>,
    cat_one: Vec<Vec<(i64, f64)>>,
    deltas: Vec<ActiveDelta>,
}

impl Overlay {
    fn new(accounts: usize, categories: usize) -> Self {
        Overlay {
            acc_step: vec![Vec::new(); accounts],
            acc_one: vec![Vec::new(); accounts],
            cat_step: vec![Vec::new(); categories],
            cat_one: vec![Vec::new(); categories],
            deltas: Vec::new(),
        }
    }

    fn clear(&mut self) {
        for v in self
            .acc_step
            .iter_mut()
            .chain(self.acc_one.iter_mut())
            .chain(self.cat_step.iter_mut())
            .chain(self.cat_one.iter_mut())
        {
            v.clear();
        }
        self.deltas.clear();
    }

    /// The sum of every recurring cost active in `month`, base-currency major units. Signed as a
    /// cost, so the caller adds it to spending.
    fn delta_at(&self, category: usize, month: i64) -> f64 {
        self.deltas
            .iter()
            .filter(|d| d.category == category)
            .map(|d| d.at(month))
            .sum()
    }
}

/// Slot of the account/category/stream an effect names, or `None` when it is not in this
/// simulation at all.
///
/// `None` is a real answer, not a failure: a cash account is deliberately pooled rather than
/// simulated, a transfer category has no assumption, and a stream in an unconvertible currency was
/// left out and named. An effect pointing at one of those is dropped here — the alternative, a
/// panic or a fabricated slot, would take down a live GET over a target the projection was never
/// going to model.
fn account_index(sims: &[AccountSim], account_id: i64) -> Option<usize> {
    sims.iter()
        .position(|s| s.account_id == account_id && s.takes_events)
}

fn category_index(sims: &[CategorySim], category_id: i64) -> Option<usize> {
    sims.iter().position(|s| s.category_id == category_id)
}

fn stream_index(sims: &[StreamSim], stream_id: i64) -> Option<usize> {
    sims.iter().position(|s| s.stream_id == stream_id)
}

/// Salt for the events RNG, so its stream cannot collide with the projection's own.
const EVENTS_RNG_SALT: u64 = 0x4C49_4645_5645_4E54; // "LIFEVENT"

/// One forecast event, resolved. Held in **topological order**, so applying an `after` constraint
/// can read its parent's already-sampled month by index without a second pass.
struct EventSim {
    event_id: i64,
    label: String,
    kind: LifeEventKind,
    person_id: Option<i64>,
    probability: f64,
    /// Month offset of `expected_on` from today. Signed and unclamped — a date already past is a
    /// real case, handled by the window clamp rather than by dropping the event.
    expected_month: f64,
    spread: f64,
    effects: Vec<LifeEffectSpec>,
    /// `(parent index into the sims, min gap in months)`.
    after: Vec<(usize, i64)>,
    only_if: Vec<usize>,
}

/// What one path decided about one event.
#[derive(Clone, Copy, Default)]
struct PathEvent {
    occurred: bool,
    /// `None` when it did not occur. May exceed the horizon — kept rather than dropped, so the
    /// realised timing can honestly report "beyond this chart".
    month: Option<i64>,
    /// An `after` bound actually moved the sampled month. The honesty signal the UI reports.
    constrained: bool,
    /// The sample landed at or before today and was clamped into the first projected month.
    clamped_early: bool,
}

/// A 64-bit mix of three values, for deriving an independent RNG stream per (event, path).
///
/// SplitMix64's finaliser. One stream per pair means adding, deleting or reordering an event cannot
/// move any *other* event's realisation on any path, so the reported occurrence rates stay stable
/// across edits — which they would not if every event drew from one shared stream in list order.
fn mix64(a: u64, b: u64, c: u64) -> u64 {
    let mut z = a ^ b.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ c.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Inverse CDF of a uniform hard window on `[-spread, +spread]`.
///
/// One uniform in, one value out, so RNG consumption is fixed whatever the spread — the property a
/// seeded run needs, and the reason this is not a reject-and-retry. A spread of 0 short-circuits, so
/// a certain-timing event costs the same draws as an uncertain one.
fn uniform_offset(u: f64, spread: f64) -> f64 {
    if spread <= 0.0 {
        return 0.0;
    }
    (2.0 * u - 1.0) * spread
}

/// Every event's outcome on every path.
///
/// Drawn from RNGs seeded independently of the projection's, which is the acceptance criterion for
/// this whole feature: with no events configured, every figure is byte-identical to a run before
/// events existed, because not one value is taken from the shared stream.
fn sample_event_outcomes(
    seed: u64,
    n_paths: usize,
    horizon: i64,
    sims: &[EventSim],
) -> Vec<Vec<PathEvent>> {
    let mut out = Vec::with_capacity(n_paths);
    for path in 0..n_paths {
        let mut path_events: Vec<PathEvent> = vec![PathEvent::default(); sims.len()];
        // Topological order, so a parent is always decided before its child reads it.
        for (i, ev) in sims.iter().enumerate() {
            let mut rng = StdRng::seed_from_u64(mix64(
                seed ^ EVENTS_RNG_SALT,
                ev.event_id as u64,
                path as u64,
            ));
            let occurred = rng.random::<f64>() < ev.probability
                && ev.only_if.iter().all(|&p| path_events[p].occurred);
            // Drawn unconditionally, and separately from the occurrence draw, so changing an event's
            // probability cannot shift its timing distribution. One question, one draw.
            let u = rng.random::<f64>();
            let mut month = (ev.expected_month + uniform_offset(u, ev.spread)).round() as i64;

            // Relations, by clamping — never by resampling, which would make RNG consumption depend
            // on the draws (`AmortSchedule::open`'s rule).
            let mut constrained = false;
            for &(parent, gap) in &ev.after {
                // A parent that did not occur imposes nothing: `after` is ordering, not
                // conditionality. If conditionality was meant, that is `only_if`.
                if let Some(pm) = path_events[parent].month
                    && pm + gap > month
                {
                    month = pm + gap;
                    constrained = true;
                }
            }
            let clamped_early = month < 1;
            // Clamped to 1, never 0 and never dropped: month 0 is today, which is already inside the
            // history every baseline was fitted from, so firing there would double-apply. The same
            // rule `Refix.month` uses.
            let month = month.max(1);
            path_events[i] = PathEvent {
                occurred,
                month: occurred.then_some(month),
                constrained,
                clamped_early,
            };
            let _ = horizon;
        }
        out.push(path_events);
    }
    out
}

/// Events sorted into dependency order, with their relations turned into indices.
fn resolve_events(events: &[ForecastEvent], today: NaiveDate, horizon: i64) -> Vec<EventSim> {
    // Kahn's algorithm over the `depends_on` edges.
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    let mut indegree: HashMap<i64, usize> = ids.iter().map(|&i| (i, 0)).collect();
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    for e in events {
        for r in &e.relations {
            if !indegree.contains_key(&r.depends_on_event_id) {
                continue; // dangling parent: no constraint to apply
            }
            *indegree.entry(e.id).or_default() += 1;
            children
                .entry(r.depends_on_event_id)
                .or_default()
                .push(e.id);
        }
    }
    let mut ready: Vec<i64> = ids
        .iter()
        .copied()
        .filter(|i| indegree.get(i).copied().unwrap_or(0) == 0)
        .collect();
    let mut order: Vec<i64> = Vec::with_capacity(ids.len());
    while let Some(id) = ready.pop() {
        order.push(id);
        for child in children.get(&id).cloned().unwrap_or_default() {
            let d = indegree.entry(child).or_default();
            *d = d.saturating_sub(1);
            if *d == 0 {
                ready.push(child);
            }
        }
    }
    if order.len() < ids.len() {
        let stuck: Vec<i64> = ids.iter().copied().filter(|i| !order.contains(i)).collect();
        tracing::warn!(
            ?stuck,
            "forecast events form a dependency cycle; their ordering constraints are ignored for \
             this run. The write path refuses a cycle, so this means the rows were edited around it."
        );
        order.extend(stuck);
    }

    let position: HashMap<i64, usize> = order.iter().enumerate().map(|(i, &id)| (id, i)).collect();
    let by_id: HashMap<i64, &ForecastEvent> = events.iter().map(|e| (e.id, e)).collect();
    let mut sims: Vec<EventSim> = Vec::with_capacity(order.len());
    for id in &order {
        let Some(e) = by_id.get(id) else { continue };
        let expected_month = reports::parse_date(&e.expected_on)
            .map(|d| months_between(today, d) as f64)
            .unwrap_or(0.0);
        let mut after = Vec::new();
        let mut only_if = Vec::new();
        for r in &e.relations {
            let Some(&parent) = position.get(&r.depends_on_event_id) else {
                continue;
            };
            // Only a parent already placed can be read; a back-edge is part of the cycle logged
            // above and is dropped rather than read from uninitialised state.
            if parent >= sims.len() {
                continue;
            }
            match r.kind {
                RelationKind::After => after.push((parent, r.min_gap_months)),
                RelationKind::OnlyIf => only_if.push(parent),
            }
        }
        sims.push(EventSim {
            event_id: e.id,
            label: e.label.clone(),
            kind: e.kind,
            person_id: e.person_id,
            probability: e.probability_bps as f64 / 10_000.0,
            expected_month,
            spread: e.timing_spread_months as f64,
            effects: e.effects.iter().map(|x| x.spec).collect(),
            after,
            only_if,
        });
    }
    let _ = horizon;
    sims
}

/// How an event actually landed across the simulated paths — not what the user typed.
///
/// Relations move timing, so the configured window and the realised distribution genuinely differ.
/// The chart draws *this*; the editor shows the input. Drawing the input would be a lie about
/// precisely the thing the chart exists to show.
#[derive(Debug, Clone)]
pub struct EventOutcome {
    pub event_id: i64,
    pub label: String,
    pub kind: LifeEventKind,
    /// Whose event it is, so the chart can colour the band with their swatch. `None` for a
    /// household event.
    pub person_id: Option<i64>,
    pub probability_bps: i64,
    /// Paths it occurred on at all, in basis points. Differs from `probability_bps` exactly when an
    /// `only_if` bound.
    pub occurrence_rate_bps: i64,
    /// …of which also landed inside the horizon.
    pub in_window_rate_bps: i64,
    /// Realised timing across occurring paths; `None` if it never occurred.
    pub month_p10: Option<i64>,
    pub month_median: Option<i64>,
    pub month_p90: Option<i64>,
    pub date_p10: Option<String>,
    pub date_median: Option<String>,
    pub date_p90: Option<String>,
    /// Of occurring paths, how many had the date moved by an ordering constraint.
    pub constrained_rate_bps: i64,
    /// Of occurring paths, how many sampled a month at or before today.
    pub clamped_early_rate_bps: i64,
    /// Whether the p90 ran past the horizon, so the chart can draw an open end rather than assert a
    /// date the model never committed to.
    pub truncated: bool,
}

/// Reduce every path's decisions about every event into the per-event summary the UI draws.
fn summarise_events(
    sims: &[EventSim],
    outcomes: &[Vec<PathEvent>],
    today: NaiveDate,
    horizon: i64,
) -> Vec<EventOutcome> {
    let paths = outcomes.len().max(1) as i64;
    let rate = |n: usize| (n as i64) * 10_000 / paths;
    sims.iter()
        .enumerate()
        .map(|(i, ev)| {
            let mut months: Vec<f64> = Vec::new();
            let (mut occurred, mut in_window, mut constrained, mut early) = (0, 0, 0, 0);
            for path in outcomes {
                let pe = path[i];
                if !pe.occurred {
                    continue;
                }
                occurred += 1;
                if pe.constrained {
                    constrained += 1;
                }
                if pe.clamped_early {
                    early += 1;
                }
                if let Some(m) = pe.month {
                    if m <= horizon {
                        in_window += 1;
                    }
                    months.push(m as f64);
                }
            }
            months.sort_by(f64::total_cmp);
            let at = |p: f64| (!months.is_empty()).then(|| percentile(&months, p).round() as i64);
            let (p10, median, p90) = (at(0.10), at(0.50), at(0.90));
            // Percentiles over *all* occurring paths, not just the ones inside the horizon — so a
            // p90 beyond the chart says so instead of being quietly pulled back to the edge.
            let date = |m: Option<i64>| m.map(|m| add_months(today, m).to_string());
            EventOutcome {
                event_id: ev.event_id,
                label: ev.label.clone(),
                kind: ev.kind,
                person_id: ev.person_id,
                probability_bps: (ev.probability * 10_000.0).round() as i64,
                occurrence_rate_bps: rate(occurred),
                in_window_rate_bps: rate(in_window),
                month_p10: p10,
                month_median: median,
                month_p90: p90,
                date_p10: date(p10),
                date_median: date(median),
                date_p90: date(p90),
                constrained_rate_bps: if occurred > 0 {
                    (constrained as i64) * 10_000 / occurred as i64
                } else {
                    0
                },
                clamped_early_rate_bps: if occurred > 0 {
                    (early as i64) * 10_000 / occurred as i64
                } else {
                    0
                },
                truncated: p90.is_some_and(|m| m > horizon),
            }
        })
        .collect()
}

/// A recurring cost an event switched on, on one path.
struct ActiveDelta {
    category: usize,
    from: i64,
    to: Option<i64>,
    amount: f64,
    ramp: i64,
}

impl ActiveDelta {
    /// This month's share, ramped linearly in over `ramp` months.
    fn at(&self, m: i64) -> f64 {
        if m < self.from || self.to.is_some_and(|t| m > t) {
            return 0.0;
        }
        if self.ramp <= 0 {
            return self.amount;
        }
        let elapsed = (m - self.from + 1).min(self.ramp);
        self.amount * elapsed as f64 / self.ramp as f64
    }
}

/// An annual relative rate (in bps) to the monthly log-return a lognormal-style
/// multiplicative step uses, clamped so a noisy/extreme historical fit can't make the
/// compounding blow up or `ln` undefined over a multi-year horizon.
fn annual_rate_to_monthly_log_return(annual_bps: i64) -> f64 {
    let annual = (annual_bps as f64 / 10_000.0).clamp(MIN_ANNUAL_RATE, MAX_ANNUAL_RATE);
    (1.0 + annual).ln() / 12.0
}

/// An annual volatility (in bps) as the standard deviation of one month's draw, clamped to
/// `0..=`[`MAX_VOLATILITY_BPS`].
///
/// Both ends matter. The lower bound keeps a negative value out of `Normal::new`, whose
/// `unwrap` would otherwise panic on a bad variance; the upper bound — the half this used to
/// be missing — keeps `exp()` in its finite range, so no path can produce the `0.0 * inf`
/// that makes a `NaN` and takes `GET /api/forecast` down permanently. √12 because volatility
/// scales with the square root of time.
fn annual_vol_to_monthly_sd(annual_bps: i64) -> f64 {
    (annual_bps.clamp(0, MAX_VOLATILITY_BPS) as f64 / 10_000.0) / 12f64.sqrt()
}

/// `annual_bps` decayed exponentially toward `long_run_bps` at month `m`, as a monthly
/// log-return.
///
/// Inside [`TREND_FULL_STRENGTH_MONTHS`] this is the *identity* — it returns bit-for-bit what
/// [`annual_rate_to_monthly_log_return`] alone returned before decay existed. That early return
/// is load-bearing, not an optimisation: it is what guarantees every projection legal at the old
/// 60-month ceiling produces the same numbers now the ceiling is 360.
///
/// Beyond it, the fitted rate is walked toward the anchor with a [`TREND_HALF_LIFE_MONTHS`]
/// half-life. The reason is arithmetic rather than taste. A category pinned at the derived
/// ceiling ([`MAX_DERIVED_CATEGORY_GROWTH_BPS`], +25%/yr) compounds to ×3.05 over 60 months,
/// which is the bound that clamp was chosen to give. Left alone over 360 it is ×807 — at which
/// point the clamp is no longer bounding the damage from an over-fitted series, it *is* the
/// model, and the whole projection is a statement about a number nobody chose. Decayed toward a
/// 0 bps anchor the same rate gives ×5.97, or about 6.1%/yr averaged over the thirty years —
/// still the pathological end of the range, but a claim the fit can support rather than one it
/// cannot.
///
/// The ordinary case moves far less, which is the point: a derived +3%/yr goes from ×2.43
/// undecayed to ×1.26. The decay is aimed at the tail, not at every trend.
///
/// Rounding back to whole bps before converting keeps this on exactly the same code path as an
/// un-decayed rate, including [`MIN_ANNUAL_RATE`]/[`MAX_ANNUAL_RATE`].
fn decayed_monthly_log_return(annual_bps: i64, long_run_bps: i64, m: i64) -> f64 {
    let excess = m - TREND_FULL_STRENGTH_MONTHS;
    if excess <= 0 {
        return annual_rate_to_monthly_log_return(annual_bps);
    }
    let w = (-std::f64::consts::LN_2 * excess as f64 / TREND_HALF_LIFE_MONTHS).exp();
    let annual = annual_bps as f64 * w + long_run_bps as f64 * (1.0 - w);
    annual_rate_to_monthly_log_return(annual.round() as i64)
}

/// The monthly log-return a projection uses at each month, indexed `0..=horizon`.
///
/// Precomputed once per target rather than evaluated per (path × month): the table is
/// `horizon + 1` floats against `paths × months` lookups, so it turns tens of millions of
/// `exp()` calls into an index.
///
/// `long_run_bps` is `None` for every rate that must **not** decay, and each exclusion is a
/// different argument:
///
/// * an explicit override — the user asserting a rate, which is the same line
///   [`MAX_DERIVED_CATEGORY_GROWTH_BPS`] already declines to cross;
/// * a cron-configured rate — likewise configured rather than fitted;
/// * [`AssumptionSource::InsufficientHistory`] — already flat, so there is nothing to decay;
/// * [`AssumptionSource::Deterministic`] — an amortisation schedule has no rate to decay at all.
///
/// Only a rate *derived* from a finite window of history is being extrapolated past that window,
/// and only that rate is walked back toward the anchor.
fn drift_series(annual_bps: i64, long_run_bps: Option<i64>, horizon: i64) -> Vec<f64> {
    match long_run_bps {
        None => vec![annual_rate_to_monthly_log_return(annual_bps); (horizon + 1) as usize],
        Some(lr) => (0..=horizon)
            .map(|m| decayed_monthly_log_return(annual_bps, lr, m))
            .collect(),
    }
}

/// The long-run anchor a resolved assumption's growth decays toward, or `None` if it must not
/// decay. See [`drift_series`] for why each source is treated the way it is.
fn long_run_anchor(a: &ResolvedAssumption) -> Option<i64> {
    match a.source {
        AssumptionSource::Derived => Some(a.long_run_growth_bps),
        AssumptionSource::Override
        | AssumptionSource::Cron
        | AssumptionSource::Deterministic
        | AssumptionSource::InsufficientHistory => None,
        // The baseline here is a *residual* — whatever the income streams did not explain — and the
        // streams themselves carry their own dated schedule. Decaying the residual would be decaying
        // a leftover, which says nothing about the long run either way, so it is left flat.
        AssumptionSource::ModelledFromIncome => None,
        // The rate here is already the long-run anchor (or an override that was left alone), because
        // the fitted one was discarded — so there is nothing left to decay toward.
        AssumptionSource::ContributionDriven => None,
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// One month's samples reduced to percentile bands, in the report currency's minor units.
///
/// Deliberately unpanickable on any input, because this is the funnel every knob in the
/// simulation eventually pours through and it runs on a live GET. Two independent measures:
///
/// * non-finite samples are dropped rather than ranked — an `inf` or a `NaN` is not a net
///   worth, and averaging one in makes every percentile of the month meaningless too;
/// * the sort uses [`f64::total_cmp`], a total order over *all* f64s including `NaN`, so
///   there is no `partial_cmp(..).unwrap()` left to panic even if a future knob smuggles one
///   past the filter.
///
/// The old `partial_cmp().unwrap()` is what turned one unbounded volatility override into a
/// 500 that outlived the request — see [`MAX_VOLATILITY_BPS`]. Clamping the knobs is the fix;
/// this is the layer that survives the *next* one.
fn band_from_samples(samples: &mut Vec<f64>, fx: &Fx) -> Band {
    let before = samples.len();
    samples.retain(|v| v.is_finite());
    if samples.len() < before {
        tracing::warn!(
            dropped = before - samples.len(),
            of = before,
            "non-finite simulated values excluded from this month's bands: some assumption is \
             producing an overflowing projection"
        );
    }
    samples.sort_by(f64::total_cmp);
    let mean = if samples.is_empty() {
        0.0
    } else {
        samples.iter().sum::<f64>() / samples.len() as f64
    };
    Band {
        p10_minor: fx.base_minor(percentile(samples, 0.10)),
        p25_minor: fx.base_minor(percentile(samples, 0.25)),
        median_minor: fx.base_minor(percentile(samples, 0.50)),
        mean_minor: fx.base_minor(mean),
        p75_minor: fx.base_minor(percentile(samples, 0.75)),
        p90_minor: fx.base_minor(percentile(samples, 0.90)),
    }
}

/// [`months_between`], for `crate::income`. The payment calendar has to agree with the simulation
/// about what month a date falls in, to the month — two copies of this arithmetic would be two
/// answers.
pub(crate) fn months_between_pub(a: NaiveDate, b: NaiveDate) -> i64 {
    months_between(a, b)
}

/// [`add_months`], for `crate::income`, on the same argument.
pub(crate) fn add_months_pub(d: NaiveDate, n: i64) -> NaiveDate {
    add_months(d, n)
}

fn months_between(a: NaiveDate, b: NaiveDate) -> i64 {
    (b.year() as i64 - a.year() as i64) * 12 + (b.month() as i64 - a.month() as i64)
}

/// `d` plus `n` calendar months, clamping the day-of-month to the target month's length
/// (matches `crons::period_date`'s convention for the same problem).
fn add_months(d: NaiveDate, n: i64) -> NaiveDate {
    let total = d.year() as i64 * 12 + (d.month() as i64 - 1) + n;
    let year = total.div_euclid(12) as i32;
    let month = total.rem_euclid(12) as u32 + 1;
    let day = d.day().min(reports::last_day_of_month(year, month).day());
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// A rate roll-off: when the current fixed period ends, and what to assume after it.
///
/// Only present when the metadata carries *both* an expiry and a rate to assume — an
/// expiry with nothing to switch to is not a schedule, it's a gap.
#[derive(Debug, Clone, Copy)]
struct Refix {
    /// Month index relative to today. Never 0: the projection loop runs `1..=horizon`, so
    /// a fix expiring this month — or one whose `fixed_until` was simply never updated
    /// after it lapsed — has to roll over at the first projected month, or it would test
    /// equal to nothing and silently never roll at all.
    month: i64,
    rate_bps: i64,
    /// One standard deviation, in bps. Zero means the refix is a certainty.
    uncertainty_bps: i64,
}

/// The amortisation terms a mortgage/loan projects from, or `None` if the four the
/// schedule can't do without (principal, rate, term, start) are unset or unparseable —
/// the account then falls back to a trend/rate like a generic asset. Everything else
/// refines the schedule rather than enabling it.
#[derive(Debug, Clone, Copy)]
struct LoanTerms {
    /// Minor units, positive. Only the fallback anchor: a loan with a real balance is
    /// projected from that instead (see [`AmortSchedule::expected`]).
    original_principal: f64,
    rate_bps: i64,
    term_months: i64,
    start: NaiveDate,
    refix: Option<Refix>,
    /// The recorded contractual repayment, normalised to a monthly amount in minor units.
    monthly_repayment: Option<f64>,
}

/// A term beyond this is data entry, not a loan — and a large exponent is exactly where
/// the payment formula overflows to `inf` and poisons the projection with `NaN`.
/// `band_from_samples` sorts with `partial_cmp().unwrap()`, so one `NaN` is a panic on a
/// live endpoint.
const MAX_TERM_MONTHS: i64 = 1_200;
/// Likewise for a rate. A borrowing rate is never negative.
const MAX_RATE_BPS: i64 = 100_000;

fn loan_terms(metadata: &AccountMetadata, today: NaiveDate) -> Option<LoanTerms> {
    #[allow(clippy::type_complexity)]
    let (
        original,
        rate,
        term,
        start,
        rate_type,
        fixed_until,
        refix_rate,
        refix_sd,
        repayment,
        frequency,
    ) = match metadata {
        AccountMetadata::Mortgage(m) => (
            m.original_amount_minor,
            m.interest_rate_bps,
            m.term_months,
            m.start_date.as_deref(),
            m.rate_type,
            m.fixed_until.as_deref(),
            m.refix_rate_bps,
            m.refix_rate_uncertainty_bps,
            m.repayment_minor,
            m.repayment_frequency,
        ),
        AccountMetadata::Loan(l) => (
            l.original_amount_minor,
            l.interest_rate_bps,
            l.term_months,
            l.start_date.as_deref(),
            l.rate_type,
            l.fixed_until.as_deref(),
            l.refix_rate_bps,
            l.refix_rate_uncertainty_bps,
            l.repayment_minor,
            l.repayment_frequency,
        ),
        // Every other profile has no amortisation schedule at all: the caller falls back
        // to a trend/rate like a generic asset for these.
        //
        // `StudentLoan` is the deliberate one. An income-contingent loan is a debt with no
        // principal, no term and no table — it is repaid as a percentage of income — so
        // `StudentLoanMeta` carries none of the fields this function reads, and the profile
        // split is what guarantees it. While a student loan shared `LoanMeta`, four of those
        // fields were required of it and the other two were an edit away, so filling them in
        // silently swapped the real balance for a fabricated straight line to zero;
        // docs/STUDENT-LOAN.md had to carry a "don't set term_months" trap for exactly that.
        // Now the trend fit below is the only projection a student loan can get, which is
        // also the honest one: what the balance has actually been doing.
        AccountMetadata::Depository(_)
        | AccountMetadata::Property(_)
        | AccountMetadata::StudentLoan(_)
        | AccountMetadata::Vehicle(_)
        | AccountMetadata::Shares(_)
        | AccountMetadata::Brokerage(_)
        | AccountMetadata::Crypto(_)
        | AccountMetadata::Generic(_) => return None,
    };

    // A floating rate has no end, so there is nothing for it to roll off onto — even if
    // stale `fixed_until`/`refix_rate_bps` values are still sitting in the metadata from
    // when it was fixed. This mirrors `AccountMetadata::validate_for`, which asks for the
    // refix terms only when the rate type is one that expires.
    let refix = (rate_type != Some(RateType::Floating))
        .then(|| {
            refix_rate.and_then(|rate_bps| {
                let until = reports::parse_date(fixed_until?)?;
                Some(Refix {
                    month: months_between(today, until).max(1),
                    rate_bps: rate_bps.clamp(0, MAX_RATE_BPS),
                    uncertainty_bps: refix_sd.unwrap_or(0).clamp(0, MAX_RATE_BPS),
                })
            })
        })
        .flatten();

    Some(LoanTerms {
        original_principal: original? as f64,
        rate_bps: rate?.clamp(0, MAX_RATE_BPS),
        term_months: term?.clamp(1, MAX_TERM_MONTHS),
        start: reports::parse_date(start?)?,
        refix,
        monthly_repayment: monthly_repayment(repayment, frequency),
    })
}

/// A contractual repayment normalised to a monthly amount. Annualised (×52/12, ×26/12)
/// rather than multiplied by 4 or 2: there are 52 weeks in a year, not 48, and those
/// extra payments are precisely why repaying weekly clears a loan sooner. The sign is
/// ignored — a repayment is an outflow however the user happened to type it.
fn monthly_repayment(
    amount_minor: Option<i64>,
    frequency: Option<RepaymentFrequency>,
) -> Option<f64> {
    let amount = amount_minor?.abs() as f64;
    if amount <= 0.0 {
        return None;
    }
    Some(match frequency.unwrap_or(RepaymentFrequency::Monthly) {
        RepaymentFrequency::Weekly => amount * 52.0 / 12.0,
        RepaymentFrequency::Fortnightly => amount * 26.0 / 12.0,
        RepaymentFrequency::Monthly => amount,
    })
}

fn monthly_rate(annual_bps: i64) -> f64 {
    (annual_bps.clamp(0, MAX_RATE_BPS) as f64 / 10_000.0) / 12.0
}

/// The level payment that amortises `balance` over `remaining_term` months at
/// `monthly_rate`.
///
/// Written with the discount factor rather than the equivalent `B·r·g/(g−1)`: for a long
/// term the growth factor `g` overflows to `inf` and that form yields `NaN`, whereas this
/// one degrades to `B·r` — interest-only, which is the correct limit.
fn table_payment(balance: f64, monthly_rate: f64, remaining_term: i64) -> f64 {
    if balance <= 0.0 {
        return 0.0;
    }
    let n = remaining_term.max(1) as f64;
    if monthly_rate.abs() < 1e-9 {
        return balance / n;
    }
    let discount = (1.0 + monthly_rate).powf(-n);
    if !(discount.is_finite() && discount < 1.0) {
        return balance / n; // pathological rate: straight-line beats NaN
    }
    balance * monthly_rate / (1.0 - discount)
}

/// What one month of a repayment actually costs, in the account's own minor units.
///
/// Split, because the money that leaves the household is `interest + principal` — equal
/// to the scheduled payment every month *except* the last, where only the residual is
/// owed. Debiting the nominal payment there would invent an outflow.
#[derive(Debug, Clone, Copy, Default)]
struct Repayment {
    interest: f64,
    principal: f64,
}

impl Repayment {
    fn cash_out(self) -> f64 {
        self.interest + self.principal
    }
}

/// A loan's repayment schedule as one simulated path sees it: running state, stepped a
/// month at a time, anchored to the balance the account actually has today.
///
/// Not a closed form, because the rate changes mid-flight at a refix and the payment may
/// be one the user recorded rather than one derived from the terms.
#[derive(Debug, Clone, Copy)]
struct AmortSchedule {
    /// Outstanding balance, positive minor units. Monotone non-increasing and never
    /// negative — see [`AmortSchedule::advance`].
    balance: f64,
    monthly_rate: f64,
    /// The payment charged each month at the current rate, positive minor units.
    payment: f64,
    /// Months of term left, floored at 1 so a stale term can't divide by zero.
    remaining_term: i64,
    refix_month: Option<i64>,
    /// The rate this path switches to at `refix_month` — drawn once, per path.
    refix_monthly_rate: f64,
}

impl AmortSchedule {
    /// The schedule with the refix held at its stated rate: what the UI is shown, and what
    /// a loan with no recorded uncertainty gets. The single constructor, so the projected
    /// schedule and the displayed one cannot drift apart.
    fn expected(terms: &LoanTerms, current_value: f64, today: NaiveDate) -> Self {
        // A future `start_date` (a drawdown not yet made) would otherwise give negative
        // elapsed months, a remaining term longer than the loan, and too small a payment.
        let elapsed = months_between(terms.start, today).clamp(0, terms.term_months);
        let remaining_term = (terms.term_months - elapsed).max(1);

        // Anchor on what the account is actually worth today. The point of a schedule is
        // to continue the real balance, not to re-derive a theoretical one that disagrees
        // with it: recomputing from the original principal makes the projection jump at
        // month 1 by however far the loan is ahead of (or behind) its table. Only when
        // there is no balance to read at all — no valuations, no transactions — fall back
        // to the closed form.
        let anchored = current_value.abs();
        let balance = if anchored.is_finite() && anchored >= 1.0 {
            anchored
        } else {
            amortized_remaining(
                terms.original_principal,
                terms.rate_bps,
                terms.term_months,
                elapsed,
            )
        };

        let monthly_rate = monthly_rate(terms.rate_bps);
        Self {
            balance,
            monthly_rate,
            payment: terms
                .monthly_repayment
                .filter(|p| p.is_finite() && *p > 0.0)
                .unwrap_or_else(|| table_payment(balance, monthly_rate, remaining_term)),
            remaining_term,
            refix_month: terms.refix.map(|r| r.month),
            refix_monthly_rate: terms
                .refix
                .map_or(monthly_rate, |r| self::monthly_rate(r.rate_bps)),
        }
    }

    /// [`AmortSchedule::expected`], with this path's post-refix rate drawn from
    /// `Normal(refix_rate, uncertainty)` and floored at zero.
    ///
    /// One draw, held for the whole horizon — deliberately not re-drawn at each rollover.
    /// Independent draws around a constant mean average out, so a re-drawn path is
    /// *narrower* than a persistently wrong rate; persistence is both the wider and the
    /// honest assumption about being wrong on where rates settle.
    ///
    /// Consumes exactly one RNG value when there is an uncertain refix and none otherwise,
    /// which is what keeps a seeded run reproducible — see the note on `simulate`.
    fn open(terms: &LoanTerms, current_value: f64, today: NaiveDate, rng: &mut StdRng) -> Self {
        let mut schedule = Self::expected(terms, current_value, today);
        if let Some(refix) = terms.refix {
            let sd = refix.uncertainty_bps as f64;
            if sd > 0.0 {
                let drawn = Normal::new(refix.rate_bps as f64, sd).unwrap().sample(rng);
                // Clamped, not rejection-sampled: a truncating loop would make the number
                // of RNG values consumed depend on the draws themselves, breaking
                // reproducibility. P(draw < 0) is negligible for any real rate anyway.
                schedule.refix_monthly_rate = monthly_rate(drawn.max(0.0).round() as i64);
            }
        }
        schedule
    }

    /// Take one month's payment, returning what actually leaves the account.
    fn advance(&mut self, month: i64) -> Repayment {
        // Repaid. A loan that finishes inside the horizon must stop draining cash, not go
        // on paying a phantom mortgage to the end of the projection.
        if self.balance <= 0.0 {
            return Repayment::default();
        }
        if self.refix_month == Some(month) {
            self.monthly_rate = self.refix_monthly_rate;
            // The lender re-sets the payment to clear the balance over the remaining term
            // — but never below one the user told us they actually make, so a deliberate
            // overpayment survives the rollover instead of reverting to the minimum.
            self.payment = table_payment(self.balance, self.monthly_rate, self.remaining_term)
                .max(self.payment);
        }
        let interest = self.balance * self.monthly_rate;
        // A payment that no longer covers the interest would flat-line (or grow) the
        // balance forever. Re-amortise: a too-small recorded repayment is bad data, not a
        // negative-amortisation product.
        if self.payment <= interest {
            self.payment = table_payment(self.balance, self.monthly_rate, self.remaining_term);
        }
        // The load-bearing clamp. `0 <= principal <= balance` makes the balance monotone
        // non-increasing and bounded below by zero, so it can never cross into a phantom
        // asset — the assets/liabilities split is by sign.
        let principal = (self.payment - interest).clamp(0.0, self.balance);
        self.balance -= principal;
        self.remaining_term = (self.remaining_term - 1).max(1);
        Repayment {
            interest,
            principal,
        }
    }

    /// This loan's contribution to net worth: negative, because a liability is stored
    /// negative — but exactly `0.0` once repaid, never `-0.0`. The assets/liabilities
    /// split is by sign and `-0.0 >= 0.0` is true, so a repaid loan would otherwise be
    /// filed as an asset.
    fn signed_balance(&self) -> f64 {
        if self.balance <= 0.0 {
            0.0
        } else {
            -self.balance
        }
    }
}

/// Standard fixed-payment amortisation: the remaining principal after `n` monthly
/// payments (clamped to the loan's term), computed in closed form from the loan's own
/// terms. Only the fallback anchor now — a loan with a real balance is projected forward
/// by [`AmortSchedule`] instead of re-derived from origination.
fn amortized_remaining(principal: f64, annual_rate_bps: i64, term_months: i64, n: i64) -> f64 {
    if term_months <= 0 || principal <= 0.0 {
        return 0.0;
    }
    let n = n.clamp(0, term_months);
    let r = monthly_rate(annual_rate_bps);
    if r.abs() < 1e-9 {
        return (principal * (1.0 - n as f64 / term_months as f64)).max(0.0);
    }
    let payment = table_payment(principal, r, term_months);
    let growth_n = (1.0 + r).powf(n as f64);
    if !growth_n.is_finite() {
        return 0.0;
    }
    (principal * growth_n - payment * (growth_n - 1.0) / r).max(0.0)
}

/// This account's value at each month-end from its earliest transaction/valuation
/// through `today` (reusing the exact point-in-time resolution `net_worth` uses, so a
/// provider-synced mortgage or a manually-valued property resolves identically here).
fn monthly_value_series(
    account_id: i64,
    currency: &str,
    today: NaiveDate,
    fx: &Fx,
    tx_by_acct: &HashMap<i64, Vec<(NaiveDate, i64, String)>>,
    val_by_acct: &HashMap<i64, Vec<(NaiveDate, i64, String)>>,
) -> Vec<(NaiveDate, f64)> {
    let earliest = tx_by_acct
        .get(&account_id)
        .and_then(|v| v.iter().map(|(d, _, _)| *d).min())
        .into_iter()
        .chain(
            val_by_acct
                .get(&account_id)
                .and_then(|v| v.iter().map(|(d, _, _)| *d).min()),
        )
        .min();
    let Some(earliest) = earliest else {
        return Vec::new();
    };
    // Only the recent past. An account's whole history can span a change of regime that a
    // single straight line cannot represent, and fitting through one reads out backwards:
    // a student loan drawn down over three years of study and repaid ever since has a net
    // *downward* slope across its full history, so the trend says the debt is growing at
    // the very moment it is being cleared. What matters for projecting forward is what the
    // account has been doing lately.
    let earliest = earliest.max(add_months(today, -ACCOUNT_TREND_MONTHS));
    reports::sample_dates(earliest, today, Interval::Month)
        .into_iter()
        // A month whose value cannot be expressed in one currency is dropped rather than
        // guessed at: `derive_account_rate` fits a trend through whatever points survive, and
        // refuses outright below a minimum span, so a short series is already handled.
        .filter_map(|d| {
            let (value_minor, _) =
                reports::account_value_at(account_id, currency, d, fx, tx_by_acct, val_by_acct)?;
            Some((d, value_minor as f64))
        })
        .collect()
}

/// Growth+volatility from an account's value series, or `None` if there's too little
/// history to trust. Asset/Investment values are always positive here, so their return
/// uses a geometric (log-return) model; a Liability's signed, possibly-near-zero balance
/// uses the linear trend instead (a log-return is undefined once a value crosses zero).
fn derive_account_rate(class: AccountClass, series: &[(NaiveDate, f64)]) -> Option<(i64, i64)> {
    if series.len() < 2 {
        return None;
    }
    let span_days = (series.last().unwrap().0 - series.first().unwrap().0).num_days();
    if span_days < MIN_HISTORY_DAYS {
        return None;
    }
    let values: Vec<f64> = series.iter().map(|(_, v)| *v).collect();
    // A single manual valuation carried forward every month (a property/vehicle that's
    // never been revalued) resamples into a perfectly flat series — many points, but
    // zero real evidence about a rate. That's "no data", not "confidently 0%".
    if values.iter().all(|v| (v - values[0]).abs() < 1.0) {
        return None;
    }
    if class == AccountClass::Liability {
        linear_trend_and_vol(&values).map(|(g, v, _)| (g, v))
    } else {
        geometric_growth_and_vol(&values)
    }
}

/// Annualised CAGR + volatility from a strictly-positive, month-end-resampled value
/// series (assets/investments). `None` if any value is non-positive (log undefined).
fn geometric_growth_and_vol(values: &[f64]) -> Option<(i64, i64)> {
    if values.len() < 2 || values.iter().any(|v| *v <= 0.0) {
        return None;
    }
    let log_returns: Vec<f64> = values.windows(2).map(|w| (w[1] / w[0]).ln()).collect();
    let mean = log_returns.iter().sum::<f64>() / log_returns.len() as f64;
    let annual_growth = (mean * 12.0).exp() - 1.0;
    let monthly_vol = if log_returns.len() >= 2 {
        let var = log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
            / (log_returns.len() - 1) as f64;
        var.sqrt()
    } else {
        0.0
    };
    let annual_vol = monthly_vol * 12f64.sqrt();
    Some(bps_pair(annual_growth, annual_vol))
}

/// OLS trend of a monthly value series, expressed as a relative annual rate (`12 *
/// slope / latest_fitted_value`) so it's directly comparable to
/// [`geometric_growth_and_vol`]'s output — but never takes a log, so it also works for a
/// signed series (a liability's negative balance, or a mortgage paydown crossing toward
/// zero). Also returns the fitted value at the latest point — a category's current
/// run-rate, for the simulation to grow forward from. `None` for fewer than 3 points, or
/// if the fitted latest value is ~0 (a relative rate would blow up).
fn linear_trend_and_vol(values: &[f64]) -> Option<(i64, i64, f64)> {
    let n = values.len();
    if n < 3 {
        return None;
    }
    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let x_mean = xs.iter().sum::<f64>() / n as f64;
    let y_mean = values.iter().sum::<f64>() / n as f64;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    for i in 0..n {
        cov += (xs[i] - x_mean) * (values[i] - y_mean);
        var_x += (xs[i] - x_mean).powi(2);
    }
    if var_x == 0.0 {
        return None;
    }
    let slope = cov / var_x;
    let intercept = y_mean - slope * x_mean;
    let latest_fitted = intercept + slope * xs[n - 1];
    if latest_fitted.abs() < 1.0 {
        return None;
    }
    let residual_var: f64 = xs
        .iter()
        .zip(values)
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum::<f64>()
        / (n.saturating_sub(2)).max(1) as f64;
    let residual_stdev = residual_var.sqrt();

    let annual_growth = 12.0 * slope / latest_fitted;
    let annual_vol = (residual_stdev * 12f64.sqrt() / latest_fitted.abs()).abs();
    let (g, v) = bps_pair(annual_growth, annual_vol);
    Some((g, v, latest_fitted))
}

/// A category's cash-flow assumption, fitted from its own monthly history.
#[derive(Debug, Clone, Copy)]
struct CategoryFit {
    /// The monthly run-rate to grow forward from, base-currency major units.
    baseline: f64,
    /// Annual growth in bps — `0` when the history can't support a direction.
    growth_bps: i64,
    /// Annual volatility in bps. Measured whether or not a slope was fitted — the
    /// month-to-month scatter is real even when its direction isn't.
    vol_bps: i64,
}

/// Fit a category's monthly totals into a baseline, a growth rate and a volatility.
///
/// The baseline is the plain mean of the window — total spend over months elapsed, which
/// is what "what do I spend on this per month" actually means. Deliberately *not* the
/// OLS-fitted value at the latest point, which is what the account path uses: for a
/// household category the series is lumpy and often mostly zeros, and the fitted endpoint
/// chases the most recent spike. (Professional Services in a real database: twelve months
/// near $3 and then one $2,188 month, which put the fitted endpoint an order of magnitude
/// above anything the household actually spends.)
///
/// A slope is only fitted when there is enough activity to mean anything —
/// [`MIN_CATEGORY_TREND_MONTHS`] of span and [`MIN_CATEGORY_ACTIVE_MONTHS`] with real
/// spend. Below that the run-rate is held flat, because the alternative is extrapolating
/// the slope of a mostly-empty series for years. (Same database: Housing had three months
/// of data — $10, $1,079, $70 — from which the old code derived +325%/yr, compounding to
/// roughly 1,400× over a five-year horizon.)
///
/// Both are then clamped. A household category does not compound at triple digits, and a
/// volatility measured as a multiple of a near-zero denominator is a divide-by-small
/// artefact rather than an estimate of anything.
fn category_fit(totals: &[f64]) -> Option<CategoryFit> {
    if totals.is_empty() {
        return None;
    }
    let n = totals.len();
    let baseline = totals.iter().sum::<f64>() / n as f64;
    // No activity at all in the window: the category contributes nothing to project.
    if baseline <= 0.0 {
        return None;
    }

    let active = totals.iter().filter(|v| **v > 0.0).count() as i64;
    let flat = CategoryFit {
        baseline,
        growth_bps: 0,
        vol_bps: 0,
    };
    if (n as i64) < MIN_CATEGORY_TREND_MONTHS || active < MIN_CATEGORY_ACTIVE_MONTHS {
        return Some(flat);
    }

    let xs: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let x_mean = xs.iter().sum::<f64>() / n as f64;
    let mut cov = 0.0;
    let mut var_x = 0.0;
    for i in 0..n {
        cov += (xs[i] - x_mean) * (totals[i] - baseline);
        var_x += (xs[i] - x_mean).powi(2);
    }
    if var_x == 0.0 {
        return Some(flat);
    }
    let slope = cov / var_x;
    let intercept = baseline - slope * x_mean;
    let residual_var: f64 = xs
        .iter()
        .zip(totals)
        .map(|(x, y)| (y - (intercept + slope * x)).powi(2))
        .sum::<f64>()
        / (n.saturating_sub(2)).max(1) as f64;

    // Only read a direction into the series if the slope actually stands out from the
    // scatter around it. Every OLS fit produces *some* slope, and on a dozen lumpy months
    // of household spending that slope is usually describing where the big months happened
    // to fall, not a change in behaviour — projecting it for years then turns an accident
    // of timing into the dominant term. The standard t-statistic on the slope is the
    // textbook way to ask "is this distinguishable from flat?", and |t| >= 2 is roughly the
    // 95% threshold at the window sizes in play (6-24 months).
    let slope_stderr = (residual_var / var_x).sqrt();
    if !(slope_stderr.is_finite() && slope_stderr > 0.0)
        || (slope / slope_stderr).abs() < MIN_TREND_T_STATISTIC
    {
        // Flat run-rate, but keep the measured volatility: the scatter is real even when
        // the direction isn't.
        let vol = (residual_var.sqrt() * 12f64.sqrt() / baseline)
            .clamp(0.0, MAX_DERIVED_CATEGORY_VOL_BPS as f64 / 10_000.0);
        return Some(CategoryFit {
            baseline,
            growth_bps: 0,
            vol_bps: bps_pair(0.0, vol).1,
        });
    }

    // Both relative to the mean, not to the fitted endpoint — a stable denominator.
    let growth = (12.0 * slope / baseline).clamp(
        -(MAX_DERIVED_CATEGORY_GROWTH_BPS as f64 / 10_000.0),
        MAX_DERIVED_CATEGORY_GROWTH_BPS as f64 / 10_000.0,
    );
    let vol = (residual_var.sqrt() * 12f64.sqrt() / baseline)
        .clamp(0.0, MAX_DERIVED_CATEGORY_VOL_BPS as f64 / 10_000.0);
    let (growth_bps, vol_bps) = bps_pair(growth, vol);
    Some(CategoryFit {
        baseline,
        growth_bps,
        vol_bps,
    })
}

fn bps_pair(growth: f64, vol: f64) -> (i64, i64) {
    (
        (growth * 10_000.0).round() as i64,
        (vol * 10_000.0).round() as i64,
    )
}

/// This category's (and its descendants') monthly spend/income totals, converted to
/// base-currency major units, in ascending month order, from its first activity through
/// the last complete calendar month (today's partial month is dropped so it doesn't bias
/// the trend downward) — capped to the trailing `CATEGORY_TREND_MONTHS`. A month with no
/// matching transaction is a real `0`, not a gap to skip — for an occasional category
/// (an annual subscription, a rare purchase) the quiet months are exactly what makes it
/// occasional, and dropping them would silently inflate both the baseline and its
/// apparent volatility.
fn category_monthly_totals(
    spend: &[crate::ports::SpendTransaction],
    cats: &reports::Categories,
    top_id: i64,
    today: NaiveDate,
    fx: &Fx,
) -> Vec<f64> {
    let this_month = (today.year(), today.month());
    let mut totals: std::collections::BTreeMap<(i32, u32), f64> = std::collections::BTreeMap::new();
    let mut earliest: Option<NaiveDate> = None;
    for t in spend {
        let Some(cid) = t.category_id else {
            continue;
        };
        if cats.top_ancestor(cid) != top_id {
            continue;
        }
        let Some(d) = reports::parse_date(&t.posted_at) else {
            continue;
        };
        let key = (d.year(), d.month());
        if key == this_month {
            continue;
        }
        // No rate to the projection's currency: the row is left out of the fitted run-rate
        // (and named in `ForecastResult::unconverted`) rather than folded in at parity, which
        // would then compound over the whole horizon.
        let Some(base_major) = fx.try_to_base_major(t.amount_minor, &t.currency_code) else {
            continue;
        };
        *totals.entry(key).or_default() += base_major.abs();
        earliest = Some(earliest.map_or(d, |e| e.min(d)));
    }
    let Some(earliest) = earliest else {
        return Vec::new();
    };

    let mut vals = Vec::new();
    let (mut y, mut m) = (earliest.year(), earliest.month());
    while (y, m) != this_month && vals.len() < 1200 {
        vals.push(*totals.get(&(y, m)).unwrap_or(&0.0));
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    }
    let cap = CATEGORY_TREND_MONTHS as usize;
    if vals.len() > cap {
        vals.drain(0..vals.len() - cap);
    }
    vals
}

/// Resolve one knob's value + provenance: an explicit override wins, then an existing
/// cron's rate (accounts only — crons have no volatility concept), then a historical
/// default; with none of those, mean/volatility default to 0 with a flag rather than a
/// guess.
fn resolve_growth(
    override_growth: Option<i64>,
    override_vol: Option<i64>,
    cron_growth: Option<i64>,
    derived: Option<(i64, i64)>,
) -> (i64, i64, AssumptionSource) {
    if override_growth.is_some() || override_vol.is_some() {
        let (d_growth, d_vol) = derived.unwrap_or((0, 0));
        let growth = override_growth.or(cron_growth).unwrap_or(d_growth);
        let vol = override_vol.unwrap_or(d_vol);
        return (growth, vol, AssumptionSource::Override);
    }
    if let Some(g) = cron_growth {
        let (_, d_vol) = derived.unwrap_or((0, 0));
        return (g, d_vol, AssumptionSource::Cron);
    }
    match derived {
        Some((g, v)) => (g, v, AssumptionSource::Derived),
        None => (0, 0, AssumptionSource::InsufficientHistory),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A property/vehicle with exactly one manual valuation ever, carried forward every
    /// resampled month, is a flat series with many points but zero real evidence about a
    /// rate — must read as insufficient, not a confident 0%. Caught by sanity-checking
    /// against a real database where a never-revalued house did exactly this.
    #[test]
    fn derive_account_rate_flags_a_single_valuation_carried_forward_as_insufficient() {
        let series: Vec<(NaiveDate, f64)> = (0..12)
            .map(|i| {
                (
                    d("2025-01-01") + chrono::Duration::days(i * 30),
                    770_000_00.0,
                )
            })
            .collect();
        assert!(derive_account_rate(AccountClass::Asset, &series).is_none());
    }

    #[test]
    fn derive_account_rate_derives_from_genuine_variation() {
        let monthly = (1.10f64).powf(1.0 / 12.0);
        let mut v = 500_000_00.0;
        let series: Vec<(NaiveDate, f64)> = (0..24)
            .map(|i| {
                let date = d("2024-01-01") + chrono::Duration::days(i * 30);
                if i > 0 {
                    v *= monthly;
                }
                (date, v)
            })
            .collect();
        assert!(derive_account_rate(AccountClass::Asset, &series).is_some());
    }

    /// A category used only occasionally (e.g. a haircut every few months) must have its
    /// quiet months counted as real `0`s, not skipped — otherwise the derived baseline
    /// only ever sees "months where something happened", which overstates both the
    /// average and its apparent volatility.
    #[test]
    fn category_monthly_totals_fills_gaps_with_zero() {
        let spend = vec![
            crate::ports::SpendTransaction {
                posted_at: "2026-01-15".into(),
                amount_minor: -5_000,
                currency_code: "NZD".into(),
                category_id: Some(1),
                is_one_off: false,
                linked_transaction_id: None,
                account_id: 1,
                account_name: "Bank".into(),
                account_kind: AccountKind::Bank,
                merchant_id: None,
                merchant: None,
                attribution: sure_core::Ownership::Joint,
            },
            crate::ports::SpendTransaction {
                posted_at: "2026-04-15".into(),
                amount_minor: -5_000,
                currency_code: "NZD".into(),
                category_id: Some(1),
                is_one_off: false,
                linked_transaction_id: None,
                account_id: 1,
                account_name: "Bank".into(),
                account_kind: AccountKind::Bank,
                merchant_id: None,
                merchant: None,
                attribution: sure_core::Ownership::Joint,
            },
        ];
        let mut cats = reports::Categories::default_for_test();
        cats.insert_for_test(1, None, "Grooming", CategoryKind::Expense);
        let fx = Fx::parity("NZD");
        let vals = category_monthly_totals(&spend, &cats, 1, d("2026-06-01"), &fx);
        // Jan, Feb, Mar, Apr, May: 5 months spanned, only Jan and Apr non-zero.
        assert_eq!(vals.len(), 5);
        assert_eq!(vals.iter().filter(|v| **v == 0.0).count(), 3);
    }

    #[test]
    fn geometric_growth_matches_a_known_compounding_rate() {
        // Exactly 12%/yr compounded monthly from 100_000.
        let monthly = (1.12f64).powf(1.0 / 12.0);
        let mut v = 100_000.0;
        let mut series = vec![v];
        for _ in 0..24 {
            v *= monthly;
            series.push(v);
        }
        let (growth_bps, vol_bps) = geometric_growth_and_vol(&series).unwrap();
        assert!(
            (growth_bps - 1200).abs() <= 2,
            "expected ~1200bps, got {growth_bps}"
        );
        // Noiseless series: volatility should resolve to (near) zero.
        assert!(vol_bps.abs() <= 1, "expected ~0bps, got {vol_bps}");
    }

    #[test]
    fn geometric_growth_rejects_a_non_positive_series() {
        assert!(geometric_growth_and_vol(&[100.0, -50.0, 200.0]).is_none());
        assert!(geometric_growth_and_vol(&[100.0]).is_none());
    }

    #[test]
    fn linear_trend_handles_a_shrinking_negative_liability() {
        // A mortgage going from -100_000 to -88_000 over 12 months (paid down $1000/mo).
        let series: Vec<f64> = (0..=12).map(|i| -100_000.0 + i as f64 * 1_000.0).collect();
        let (growth_bps, _, _) = linear_trend_and_vol(&series).unwrap();
        // Debt shrinking in magnitude ⇒ the signed value is moving toward zero ⇒ a
        // negative relative rate applied to the (negative) current value.
        assert!(growth_bps < 0, "expected negative bps, got {growth_bps}");
    }

    #[test]
    fn linear_trend_needs_at_least_three_points() {
        assert!(linear_trend_and_vol(&[1.0, 2.0]).is_none());
    }

    /// The Housing case from a real database: three months of data ($10, $1,079, $70) in a
    /// seven-month window. The old code fitted a slope to that and derived +325%/yr, which
    /// compounds to ~1,400× over five years and swamps the whole projection. Too few active
    /// months to read a direction into — hold the run-rate flat instead.
    #[test]
    fn category_fit_holds_a_sparse_series_flat_instead_of_extrapolating_it() {
        let totals = vec![10.0, 0.0, 0.0, 0.0, 0.0, 1_079.0, 70.0];
        let fit = category_fit(&totals).unwrap();
        assert_eq!(fit.growth_bps, 0);
        assert_eq!(fit.growth_bps, 0);
        assert_eq!(fit.vol_bps, 0);
        // …but it still contributes: total spend over months elapsed.
        assert!((fit.baseline - 1_159.0 / 7.0).abs() < 0.01);
    }

    /// The bug this exposes: a baseline used to exist only where a regression succeeded, so
    /// a category the fit rejected vanished from the simulation entirely — projecting zero
    /// spending on it, which is a bigger error than projecting it flat.
    #[test]
    fn category_fit_keeps_a_baseline_for_a_series_too_short_to_trend() {
        let fit = category_fit(&[300.0, 200.0]).unwrap();
        assert_eq!(fit.growth_bps, 0);
        assert!((fit.baseline - 250.0).abs() < 0.01);
    }

    /// A genuine, sustained trend is still read — the clamp only bites the absurd.
    #[test]
    fn category_fit_reads_a_real_trend() {
        // Rising ~4%/month from 1000, twelve months: a real direction, plainly visible.
        let totals: Vec<f64> = (0..12).map(|i| 1_000.0 * 1.04f64.powi(i)).collect();
        let fit = category_fit(&totals).unwrap();
        assert!(
            fit.growth_bps > 1_000,
            "expected a clear rise, got {}",
            fit.growth_bps
        );
        assert!(fit.growth_bps <= MAX_DERIVED_CATEGORY_GROWTH_BPS);
    }

    /// A lumpy series' fitted endpoint chases the latest spike; the mean does not. The
    /// Professional Services case: twelve months near $3, then one $2,188 month. One
    /// expensive month is not a trend, and the t-test is what says so.
    #[test]
    fn category_fit_is_not_dragged_to_the_latest_spike() {
        let mut totals = vec![3.0; 12];
        totals.push(2_188.0);
        let fit = category_fit(&totals).unwrap();
        // The mean is ~$171; the OLS endpoint would be an order of magnitude higher.
        assert!(
            fit.baseline < 250.0,
            "baseline {} chased the spike",
            fit.baseline
        );
        assert_eq!(fit.growth_bps, 0);
        // …but the scatter is real, so the volatility survives.
        assert!(fit.vol_bps > 0);
    }

    /// Noise around a flat mean has a non-zero OLS slope essentially always. Projecting it
    /// for years is how an accident of which months were expensive becomes the dominant
    /// term in a forecast — so it has to read as flat.
    #[test]
    fn category_fit_does_not_trend_on_noise() {
        // Alternating high/low around $500: no direction, plenty of scatter.
        let totals: Vec<f64> = (0..12)
            .map(|i| if i % 2 == 0 { 300.0 } else { 700.0 })
            .collect();
        let fit = category_fit(&totals).unwrap();
        assert_eq!(fit.growth_bps, 0);
        assert_eq!(fit.growth_bps, 0);
        assert!((fit.baseline - 500.0).abs() < 0.01);
    }

    /// Both knobs are bounded, whatever the data does.
    #[test]
    fn category_fit_clamps_growth_and_volatility() {
        // A violently accelerating series: unclamped this fits several hundred percent.
        let totals: Vec<f64> = (0..12).map(|i| 10.0 * 3f64.powi(i)).collect();
        let fit = category_fit(&totals).unwrap();
        assert_eq!(fit.growth_bps, MAX_DERIVED_CATEGORY_GROWTH_BPS);
        assert!(fit.vol_bps <= MAX_DERIVED_CATEGORY_VOL_BPS);
    }

    #[test]
    fn category_fit_ignores_a_category_with_no_spend() {
        assert!(category_fit(&[0.0; 12]).is_none());
        assert!(category_fit(&[]).is_none());
    }

    #[test]
    fn resolve_growth_precedence_override_then_cron_then_derived() {
        assert_eq!(
            resolve_growth(Some(700), None, Some(400), Some((300, 50))).0,
            700
        );
        assert_eq!(
            resolve_growth(None, None, Some(400), Some((300, 50))).0,
            400
        );
        assert_eq!(
            resolve_growth(None, None, None, Some((300, 50))),
            (300, 50, AssumptionSource::Derived)
        );
        assert_eq!(
            resolve_growth(None, None, None, None),
            (0, 0, AssumptionSource::InsufficientHistory)
        );
    }

    fn mortgage_meta() -> sure_core::MortgageMeta {
        sure_core::MortgageMeta {
            original_amount_minor: Some(500_000_00),
            interest_rate_bps: Some(549),
            term_months: Some(360),
            start_date: Some("2024-01-01".into()),
            ..Default::default()
        }
    }

    #[test]
    fn has_amortization_schedule_requires_every_field() {
        let today = d("2026-07-01");
        assert!(loan_terms(&AccountMetadata::Mortgage(mortgage_meta()), today).is_some());

        let partial = sure_core::MortgageMeta {
            original_amount_minor: Some(500_000_00),
            ..Default::default()
        };
        assert!(loan_terms(&AccountMetadata::Mortgage(partial), today).is_none());
    }

    /// `fixed_until` is the one field nobody updates after they actually refix, so a date
    /// in the past is the common case rather than the exotic one. The projection loop runs
    /// `1..=horizon`, so a stale expiry must still roll over at the first projected month
    /// — testing the month for equality against a clamped-to-zero index would mean the
    /// roll-off silently never fires and the loan keeps a rate it no longer has.
    #[test]
    fn loan_terms_rolls_an_already_expired_fix_over_at_the_first_month() {
        let today = d("2026-07-01");
        let meta = sure_core::MortgageMeta {
            rate_type: Some(sure_core::RateType::Fixed),
            fixed_until: Some("2026-01-15".into()),
            refix_rate_bps: Some(699),
            refix_rate_uncertainty_bps: Some(150),
            ..mortgage_meta()
        };
        let refix = loan_terms(&AccountMetadata::Mortgage(meta), today)
            .unwrap()
            .refix
            .expect("an expiry plus a rate is a refix");
        assert_eq!(refix.month, 1);
    }

    /// Switching a loan from fixed to floating leaves the old expiry and refix rate sitting
    /// in the metadata — `buildMetadata` only drops fields the user blanks. A floating rate
    /// has no end, so it must not roll off onto anything, whatever is left lying around.
    #[test]
    fn loan_terms_ignores_a_stale_refix_on_a_floating_loan() {
        let today = d("2026-07-01");
        let meta = sure_core::MortgageMeta {
            rate_type: Some(sure_core::RateType::Floating),
            fixed_until: Some("2027-01-11".into()),
            refix_rate_bps: Some(699),
            refix_rate_uncertainty_bps: Some(150),
            ..mortgage_meta()
        };
        assert!(
            loan_terms(&AccountMetadata::Mortgage(meta), today)
                .unwrap()
                .refix
                .is_none()
        );
    }

    /// An expiry with no rate to switch to isn't a schedule, and a rate with no expiry has
    /// no moment to apply — either alone must not produce a roll-off.
    #[test]
    fn loan_terms_needs_both_an_expiry_and_a_rate_to_refix() {
        let today = d("2026-07-01");
        let expiry_only = sure_core::MortgageMeta {
            fixed_until: Some("2027-01-11".into()),
            ..mortgage_meta()
        };
        let rate_only = sure_core::MortgageMeta {
            refix_rate_bps: Some(699),
            ..mortgage_meta()
        };
        for meta in [expiry_only, rate_only] {
            assert!(
                loan_terms(&AccountMetadata::Mortgage(meta), today)
                    .unwrap()
                    .refix
                    .is_none()
            );
        }
    }

    /// $500/week and $1,000/fortnight are the same annual outlay, so they must normalise
    /// to the same monthly figure — and to 52/12 of the weekly amount, not 4× it.
    #[test]
    fn monthly_repayment_annualises_rather_than_multiplying_by_weeks_in_a_month() {
        use RepaymentFrequency::*;
        let weekly = monthly_repayment(Some(500_00), Some(Weekly)).unwrap();
        let fortnightly = monthly_repayment(Some(1_000_00), Some(Fortnightly)).unwrap();
        assert!((weekly - fortnightly).abs() < 0.01);
        assert!((weekly - 216_666.67).abs() < 1.0, "got {weekly}");
        // A naive ×4 would give 200_000 — materially different over a 30-year term.
        assert!(weekly > 210_000.0);
        assert_eq!(monthly_repayment(Some(0), Some(Monthly)), None);
        // Sign is the user's typing convention, not information.
        assert_eq!(
            monthly_repayment(Some(-1_000), Some(Monthly)),
            Some(1_000.0)
        );
        assert_eq!(monthly_repayment(Some(1_000), None), Some(1_000.0));
    }

    /// The growth-factor form of the annuity payment overflows to `inf` for a long term,
    /// yielding `NaN` — which reaches `band_from_samples`, whose `partial_cmp().unwrap()`
    /// would panic on a live endpoint.
    #[test]
    fn table_payment_stays_finite_for_an_absurd_term() {
        let payment = table_payment(500_000_00.0, monthly_rate(2_000), MAX_TERM_MONTHS);
        assert!(payment.is_finite() && payment > 0.0, "got {payment}");
        assert!(
            loan_terms(
                &AccountMetadata::Mortgage(sure_core::MortgageMeta {
                    term_months: Some(999_999),
                    interest_rate_bps: Some(-500),
                    ..mortgage_meta()
                }),
                d("2026-07-01"),
            )
            .is_some_and(|t| t.term_months == MAX_TERM_MONTHS && t.rate_bps == 0)
        );
    }

    /// The whole point of anchoring: a loan that's been overpaid is worth what the ledger
    /// says, not what its original table implies. Re-deriving from the principal is what
    /// made the projection jump at month 1.
    #[test]
    fn schedule_anchors_to_the_current_balance_not_the_original_principal() {
        let today = d("2026-07-01");
        let terms = loan_terms(&AccountMetadata::Mortgage(mortgage_meta()), today).unwrap();
        let theoretical = amortized_remaining(500_000_00.0, 549, 360, 30);
        let mut schedule = AmortSchedule::expected(&terms, -400_000_00.0, today);

        assert_eq!(schedule.balance, 400_000_00.0);
        assert!(
            (theoretical - 400_000_00.0).abs() > 50_000_00.0,
            "fixture must differ"
        );

        let before = schedule.balance;
        let paid = schedule.advance(1);
        assert!(
            before - schedule.balance < paid.cash_out(),
            "no month-1 jump"
        );
    }

    /// A loan with no valuations and no transactions yet has no balance to anchor to; the
    /// closed form is the only thing left to project from.
    #[test]
    fn schedule_falls_back_to_the_theoretical_balance_with_no_history() {
        let today = d("2026-07-01");
        let terms = loan_terms(&AccountMetadata::Mortgage(mortgage_meta()), today).unwrap();
        let schedule = AmortSchedule::expected(&terms, 0.0, today);
        let expected = amortized_remaining(500_000_00.0, 549, 360, 30);
        assert!((schedule.balance - expected).abs() < 1.0);
    }

    /// The balance must be monotone, never negative, and land on exactly `+0.0` — the
    /// assets/liabilities split is by sign and `-0.0 >= 0.0` is true in Rust, so a repaid
    /// loan would otherwise be filed as an asset. Every dollar of principal must also be
    /// accounted for: the sum of the principal legs is the balance, exactly.
    #[test]
    fn schedule_never_over_amortises_past_zero() {
        let today = d("2026-07-01");
        let terms = loan_terms(
            &AccountMetadata::Mortgage(sure_core::MortgageMeta {
                // Ten times the table payment: this loan clears years early.
                repayment_minor: Some(30_000_00),
                repayment_frequency: Some(RepaymentFrequency::Monthly),
                ..mortgage_meta()
            }),
            today,
        )
        .unwrap();
        let mut schedule = AmortSchedule::expected(&terms, -400_000_00.0, today);

        let mut principal_paid = 0.0;
        let mut previous = schedule.balance;
        for m in 1..=360 {
            let paid = schedule.advance(m);
            principal_paid += paid.principal;
            assert!(schedule.balance >= 0.0, "month {m}: {}", schedule.balance);
            assert!(schedule.balance <= previous, "month {m} went backwards");
            previous = schedule.balance;
        }
        assert_eq!(schedule.balance, 0.0);
        assert!((principal_paid - 400_000_00.0).abs() < 1e-3);
        let signed = schedule.signed_balance();
        assert!(signed == 0.0 && signed.is_sign_positive(), "got {signed:?}");
        // And a repaid loan stops costing anything.
        assert_eq!(schedule.advance(361).cash_out(), 0.0);
    }

    /// A recorded repayment too small to cover the interest would flat-line the balance
    /// forever. That's bad data, not an interest-only product — re-amortise instead.
    #[test]
    fn schedule_replaces_a_repayment_that_cannot_cover_the_interest() {
        let today = d("2026-07-01");
        let terms = loan_terms(
            &AccountMetadata::Mortgage(sure_core::MortgageMeta {
                repayment_minor: Some(1_00),
                repayment_frequency: Some(RepaymentFrequency::Monthly),
                ..mortgage_meta()
            }),
            today,
        )
        .unwrap();
        let mut schedule = AmortSchedule::expected(&terms, -400_000_00.0, today);
        let mut previous = schedule.balance;
        for m in 1..=12 {
            schedule.advance(m);
            assert!(schedule.balance < previous, "month {m} did not pay down");
            previous = schedule.balance;
        }
    }

    /// A higher rate does not change *when* a table loan ends — it changes how much of
    /// each (larger) payment is interest, so the balance falls more slowly early on.
    #[test]
    fn schedule_switches_rate_at_the_refix_month() {
        let today = d("2026-07-01");
        let at = |refix_bps| {
            let terms = loan_terms(
                &AccountMetadata::Mortgage(sure_core::MortgageMeta {
                    rate_type: Some(sure_core::RateType::Fixed),
                    fixed_until: Some("2026-11-01".into()),
                    refix_rate_bps: Some(refix_bps),
                    ..mortgage_meta()
                }),
                today,
            )
            .unwrap();
            let mut schedule = AmortSchedule::expected(&terms, -400_000_00.0, today);
            let mut balances = Vec::new();
            for m in 1..=6 {
                schedule.advance(m);
                balances.push(schedule.balance);
            }
            balances
        };
        let low = at(549);
        let high = at(899);
        assert_eq!(low[..3], high[..3], "identical before the refix");
        assert!(high[5] > low[5], "a higher rate amortises more slowly");
    }

    /// Rounding a repayment up is the commonest way people overpay a mortgage. The lender
    /// re-setting the payment at a refix must not quietly revert them to the minimum.
    #[test]
    fn schedule_keeps_an_overpayment_across_a_refix() {
        let today = d("2026-07-01");
        let terms = loan_terms(
            &AccountMetadata::Mortgage(sure_core::MortgageMeta {
                rate_type: Some(sure_core::RateType::Fixed),
                fixed_until: Some("2026-09-01".into()),
                // Well above the table payment, and the refix is to a *lower* rate.
                refix_rate_bps: Some(100),
                repayment_minor: Some(5_000_00),
                repayment_frequency: Some(RepaymentFrequency::Monthly),
                ..mortgage_meta()
            }),
            today,
        )
        .unwrap();
        let mut schedule = AmortSchedule::expected(&terms, -400_000_00.0, today);
        for m in 1..=3 {
            schedule.advance(m);
        }
        assert!(
            (schedule.payment - 5_000_00.0).abs() < 1.0,
            "got {}",
            schedule.payment
        );
    }

    #[test]
    fn amortized_remaining_reaches_zero_at_term_end() {
        let remaining = amortized_remaining(500_000_00.0, 549, 360, 360);
        assert!(remaining < 1.0, "expected ~0, got {remaining}");
    }

    #[test]
    fn amortized_remaining_pays_down_linearly_at_zero_interest() {
        let remaining = amortized_remaining(120_000.0, 0, 120, 60);
        assert!(
            (remaining - 60_000.0).abs() < 1.0,
            "expected ~60000, got {remaining}"
        );
    }

    /// The rule that replaced `month_index`, and the reason it had to.
    ///
    /// `month_index` clamped a past date to month 0 — which is right for a certainty applied to
    /// today's balances, and wrong for a sampled event: month 0 is today, already inside the history
    /// every baseline was fitted from, so firing there double-applies. Events clamp to month 1
    /// instead, the same rule `Refix.month` uses, and say so via `clamped_early_rate_bps`.
    #[test]
    fn a_sampled_month_in_the_past_is_clamped_into_the_projection_and_flagged() {
        let sims = vec![EventSim {
            event_id: 1,
            label: "Already happened".into(),
            kind: sure_core::LifeEventKind::Custom,
            person_id: None,
            probability: 1.0,
            expected_month: -18.0,
            spread: 0.0,
            effects: Vec::new(),
            after: Vec::new(),
            only_if: Vec::new(),
        }];
        let outcomes = sample_event_outcomes(7, 32, 12, &sims);
        assert!(outcomes.iter().all(|p| p[0].month == Some(1)));
        assert!(outcomes.iter().all(|p| p[0].clamped_early));
    }

    /// A month past the horizon is *kept*, not dropped, so the realised p90 can honestly report
    /// "beyond this chart" rather than being pulled back to the edge.
    #[test]
    fn a_sampled_month_beyond_the_horizon_is_kept_rather_than_dropped() {
        let sims = vec![EventSim {
            event_id: 1,
            label: "Far off".into(),
            kind: sure_core::LifeEventKind::Custom,
            person_id: None,
            probability: 1.0,
            expected_month: 400.0,
            spread: 0.0,
            effects: Vec::new(),
            after: Vec::new(),
            only_if: Vec::new(),
        }];
        let outcomes = sample_event_outcomes(7, 8, 12, &sims);
        assert!(outcomes.iter().all(|p| p[0].month == Some(400)));
        let summary = summarise_events(&sims, &outcomes, d("2026-07-01"), 12);
        assert_eq!(summary[0].occurrence_rate_bps, 10_000);
        // It happens, but never inside the window — two different facts, reported separately.
        assert_eq!(summary[0].in_window_rate_bps, 0);
        assert!(summary[0].truncated);
    }

    /// A uniform hard window means what it says: nothing lands outside it, and every month inside is
    /// reachable. A normal distribution would put ~5% of paths past the stated bound.
    #[test]
    fn the_timing_window_is_uniform_and_hard() {
        let sims = vec![EventSim {
            event_id: 1,
            label: "Child".into(),
            kind: sure_core::LifeEventKind::Child,
            person_id: None,
            probability: 1.0,
            expected_month: 36.0,
            spread: 12.0,
            effects: Vec::new(),
            after: Vec::new(),
            only_if: Vec::new(),
        }];
        let outcomes = sample_event_outcomes(11, 3_000, 120, &sims);
        let months: Vec<i64> = outcomes.iter().filter_map(|p| p[0].month).collect();
        assert_eq!(months.len(), 3_000);
        assert!(
            months.iter().all(|&m| (24..=48).contains(&m)),
            "a hard window must not leak: {:?}..{:?}",
            months.iter().min(),
            months.iter().max()
        );
        // …and the mass is spread across it rather than piled in the middle.
        let lower = months.iter().filter(|&&m| m < 30).count();
        let upper = months.iter().filter(|&&m| m > 42).count();
        assert!(lower > 500 && upper > 500, "lower {lower}, upper {upper}");
    }

    /// `after` moves a child event and says that it did; `only_if` propagates non-occurrence.
    #[test]
    fn relations_clamp_the_child_and_report_that_they_bound() {
        let parent = EventSim {
            event_id: 1,
            label: "Promotion".into(),
            kind: sure_core::LifeEventKind::Promotion,
            person_id: None,
            probability: 1.0,
            expected_month: 24.0,
            spread: 0.0,
            effects: Vec::new(),
            after: Vec::new(),
            only_if: Vec::new(),
        };
        let child = EventSim {
            event_id: 2,
            label: "Child".into(),
            kind: sure_core::LifeEventKind::Child,
            person_id: None,
            probability: 1.0,
            // Would land at month 6 unconstrained — well before the promotion.
            expected_month: 6.0,
            spread: 0.0,
            effects: Vec::new(),
            after: vec![(0, 3)],
            only_if: Vec::new(),
        };
        let sims = vec![parent, child];
        let outcomes = sample_event_outcomes(3, 16, 120, &sims);
        assert!(outcomes.iter().all(|p| p[1].month == Some(27)));
        let summary = summarise_events(&sims, &outcomes, d("2026-07-01"), 120);
        assert_eq!(summary[1].constrained_rate_bps, 10_000);
        // The realised median is later than what was configured — which is why the chart is fed
        // this and not the input.
        assert_eq!(summary[1].month_median, Some(27));

        // A parent that never happens takes its `only_if` child with it…
        let mut sims = sims;
        sims[0].probability = 0.0;
        sims[1].after = Vec::new();
        sims[1].only_if = vec![0];
        let outcomes = sample_event_outcomes(3, 16, 120, &sims);
        assert!(outcomes.iter().all(|p| !p[1].occurred));
        // …but a *timing* edge on a parent that did not occur is vacuous, not blocking.
        sims[1].only_if = Vec::new();
        sims[1].after = vec![(0, 3)];
        let outcomes = sample_event_outcomes(3, 16, 120, &sims);
        assert!(
            outcomes
                .iter()
                .all(|p| p[1].occurred && p[1].month == Some(6))
        );
    }

    /// A 50% event happens on about half the paths, and the same seed gives the same answer twice.
    #[test]
    fn occurrence_is_reproducible_and_matches_the_configured_probability() {
        let sims = vec![EventSim {
            event_id: 42,
            label: "Maybe".into(),
            kind: sure_core::LifeEventKind::Custom,
            person_id: None,
            probability: 0.5,
            expected_month: 12.0,
            spread: 0.0,
            effects: Vec::new(),
            after: Vec::new(),
            only_if: Vec::new(),
        }];
        let a = sample_event_outcomes(99, 4_000, 60, &sims);
        let b = sample_event_outcomes(99, 4_000, 60, &sims);
        let rate = |o: &Vec<Vec<PathEvent>>| o.iter().filter(|p| p[0].occurred).count();
        assert_eq!(rate(&a), rate(&b), "same seed, same answer");
        let n = rate(&a);
        assert!(
            (1_800..=2_200).contains(&n),
            "expected about half of 4000, got {n}"
        );
    }

    #[test]
    fn add_months_clamps_day_of_month() {
        // Jan 31 + 1 month → Feb 28 (2026 isn't a leap year), not an invalid Feb 31.
        assert_eq!(add_months(d("2026-01-31"), 1), d("2026-02-28"));
    }

    /// The guarantee that makes raising the horizon ceiling from 60 to 360 a shippable change
    /// rather than a silent restatement of every projection the user has already looked at.
    ///
    /// Bit-for-bit equality, not approximate: the whole claim is that nothing moves, and a
    /// tolerance here would let a rewrite of the decay arithmetic drift the last few digits of
    /// every band inside five years while this test still passed.
    #[test]
    fn decay_is_the_identity_inside_the_window_a_fit_is_trusted_over() {
        for annual_bps in [-4_000, 0, 137, 2_500, 90_000] {
            let flat = annual_rate_to_monthly_log_return(annual_bps);
            for m in 0..=TREND_FULL_STRENGTH_MONTHS {
                // A deliberately absurd anchor: if decay were reachable inside the window at
                // all, an anchor this far from the fitted rate could not fail to show it.
                assert_eq!(
                    decayed_monthly_log_return(annual_bps, 9_999, m),
                    flat,
                    "month {m} at {annual_bps} bps moved inside the full-strength window"
                );
            }
            assert_ne!(
                decayed_monthly_log_return(annual_bps, 9_999, TREND_FULL_STRENGTH_MONTHS + 1),
                flat,
                "decay never started for {annual_bps} bps"
            );
        }
    }

    /// A derived rate is *not* the only thing in the projection, so the decay must apply to it
    /// and nothing else. `drift_series` takes `None` for every source the user configured.
    #[test]
    fn only_a_derived_rate_decays() {
        let horizon = 120;
        let asserted = drift_series(2_500, None, horizon);
        assert!(
            asserted.windows(2).all(|w| w[0] == w[1]),
            "an override/cron rate must be flat across the whole horizon"
        );

        let derived = drift_series(2_500, Some(0), horizon);
        assert_eq!(
            derived[..=TREND_FULL_STRENGTH_MONTHS as usize],
            asserted[..=TREND_FULL_STRENGTH_MONTHS as usize],
            "a derived rate must match an asserted one inside the fitted window"
        );
        assert!(
            derived[horizon as usize] < derived[TREND_FULL_STRENGTH_MONTHS as usize],
            "a derived rate must have decayed by the end of a 10-year horizon"
        );
    }

    /// The figures quoted in [`decayed_monthly_log_return`]'s doc comment, pinned so the
    /// justification cannot quietly stop being true if a constant is retuned.
    ///
    /// `MAX_DERIVED_CATEGORY_GROWTH_BPS` (+25%/yr) is the interesting input because it is the
    /// clamp's own ceiling: whatever this does to that rate is the worst the projection can do.
    #[test]
    fn decay_bounds_the_pathological_tail_without_flattening_an_ordinary_trend() {
        // Compound the per-month series the simulation actually indexes, rather than a closed
        // form — so this measures the code path, not a restatement of the formula.
        let compound = |annual_bps: i64, long_run: Option<i64>, months: i64| -> f64 {
            drift_series(annual_bps, long_run, months)[1..=months as usize]
                .iter()
                .sum::<f64>()
                .exp()
        };
        let close = |got: f64, want: f64| (got - want).abs() / want < 0.005;

        let ceiling = MAX_DERIVED_CATEGORY_GROWTH_BPS;
        let at_60 = compound(ceiling, Some(0), 60);
        assert!(close(at_60, 3.0518), "60mo at the ceiling: got {at_60}");

        // Undecayed over thirty years the clamp stops bounding anything and becomes the model.
        let undecayed = compound(ceiling, None, 360);
        assert!(close(undecayed, 807.79), "360mo undecayed: got {undecayed}");

        let decayed = compound(ceiling, Some(0), 360);
        assert!(close(decayed, 5.97), "360mo decayed: got {decayed}");

        // …and the ordinary case is barely touched, which is what stops this being a blunt
        // instrument applied to every trend in the household.
        let ordinary = compound(300, Some(0), 360);
        assert!(close(ordinary, 1.262), "360mo at +3%/yr: got {ordinary}");
    }

    /// The path-month budget must not bind at any horizon that was legal before the ceiling
    /// moved, or raising it would silently coarsen every existing projection's bands.
    #[test]
    fn the_path_budget_spares_every_horizon_that_was_already_legal() {
        let budgeted = |horizon: i64| (MAX_PATH_MONTHS / horizon).max(MIN_SIMULATIONS);
        for horizon in [1, 12, 24, 36, TREND_FULL_STRENGTH_MONTHS] {
            assert!(
                budgeted(horizon) >= MAX_SIMULATIONS,
                "horizon {horizon} lost paths to the budget"
            );
        }
        // At the new ceiling it does bind, to a count that still supports a stable P10/P90.
        assert_eq!(budgeted(MAX_HORIZON_MONTHS), 2_000);
    }

    /// The W-08 clamp. Volatility used to be bounded below (so `Normal::new` never saw a
    /// negative variance) and not above, so an override of a few million bps put ±1e14 into
    /// `exp()` — which saturates past ±745 — and made `0.0 * inf` = `NaN` routine within one
    /// path. Both ends are needed, and everything a real series measures is untouched.
    #[test]
    fn annual_vol_to_monthly_sd_clamps_both_ends() {
        let sqrt12 = 12f64.sqrt();
        // 20%/yr, the ordinary case: passes through as σ/√12.
        assert!((annual_vol_to_monthly_sd(2_000) - 0.2 / sqrt12).abs() < 1e-12);
        // Negative variance is not a thing; zero means "no noise", not "panic".
        assert_eq!(annual_vol_to_monthly_sd(-5_000), 0.0);
        assert_eq!(annual_vol_to_monthly_sd(0), 0.0);
        // The ceiling holds however absurd the input.
        let ceiling = annual_vol_to_monthly_sd(MAX_VOLATILITY_BPS);
        for bps in [MAX_VOLATILITY_BPS + 1, 1_000_000_000_000_00, i64::MAX] {
            assert_eq!(annual_vol_to_monthly_sd(bps), ceiling);
        }

        // …and the exponent it implies stays clear of the ~745 at which `exp()` saturates.
        //
        // This bound is stated over the *cumulative* draw, and that framing is load-bearing.
        // While the horizon was 60 months it was possible to assert something far stronger —
        // ten sigma sustained in the same direction every single month — and have it hold
        // (0.866 × 10 × 60 = 520, finite). At 360 months the same claim is 3118 and overflows,
        // and no volatility ceiling worth having would rescue it: holding it would mean
        // clamping to ~72%/yr, and `MAX_VOLATILITY_BPS`'s own comment explains why 300%/yr is
        // a true description of a real category rather than a number to clamp away. So the
        // guarantee is restated as what the loop actually compounds — a random walk whose
        // standard deviation grows with √months, not linearly with them.
        //
        // Ten sigma of that walk is the assertion; saturation needs 45, or 41 alongside the
        // largest drift `MAX_ANNUAL_RATE` permits, neither of which occurs in `MAX_SIMULATIONS`
        // samples. Beyond that, `band_from_samples` drops non-finite samples with a WARN, which
        // is the second line of defence this ceiling was always paired with.
        let cumulative_sd = ceiling * (MAX_HORIZON_MONTHS as f64).sqrt();
        assert!(
            (cumulative_sd * 10.0).exp().is_finite(),
            "ten sigma of the whole-horizon walk must still be finite"
        );
    }

    /// Belt and braces for the same failure: whatever produced them, a `NaN` or an `inf` in
    /// the samples must not panic the sort. The old `partial_cmp().unwrap()` did, and
    /// `CatchPanicLayer` turned that into a 500 that outlived the request.
    #[test]
    fn band_from_samples_is_unpanickable_on_non_finite_samples() {
        let fx = Fx::parity("NZD");
        let mut samples = vec![
            100.0,
            f64::NAN,
            300.0,
            f64::INFINITY,
            200.0,
            f64::NEG_INFINITY,
            f64::NAN,
        ];
        let band = band_from_samples(&mut samples, &fx);
        // The finite samples alone decide the bands — 100/200/300, so the median is 200 and
        // the mean is not dragged to `NaN` by the discarded ones.
        assert_eq!(band.median_minor, 200_00);
        assert_eq!(band.mean_minor, 200_00);
        assert_eq!(band.p10_minor, 100_00);
        assert_eq!(band.p90_minor, 300_00);
        // And a month in which *nothing* was finite is an empty band, not a panic.
        let mut all_bad = vec![f64::NAN, f64::INFINITY];
        assert_eq!(band_from_samples(&mut all_bad, &fx).median_minor, 0);
    }

    // ---- simulate() integration (fake ports) ---------------------------------------

    mod sim {
        use super::*;
        use async_trait::async_trait;
        use sure_core::{
            Account, AccountKind as AK, AccountMetadata as AM, Cron, CronRun, CronRunResult,
            GenericMeta, LoanMeta, MortgageMeta, Ownership, SaveAccount, SaveCron,
            SaveForecastAssumption, SaveForecastEvent, StudentLoanMeta,
        };

        use crate::ports::{
            AccountCurrency, ActiveAccount, AssetAccount, CurrencyDecimals, ExchangeRateRow,
            LedgerTx, LedgerValuation, ReportCategory, SecuredLiabilityAccount, SharesTicker,
            SpendTransaction,
        };

        struct FakeAccounts(Vec<Account>);
        #[async_trait]
        impl AccountRepo for FakeAccounts {
            async fn list(&self, _include_archived: bool) -> AppResult<Vec<Account>> {
                Ok(self.0.clone())
            }
            async fn get(&self, _id: i64) -> AppResult<Account> {
                unreachable!()
            }
            async fn create(&self, _input: SaveAccount) -> AppResult<Account> {
                unreachable!()
            }
            async fn update(&self, _id: i64, _input: SaveAccount) -> AppResult<Account> {
                unreachable!()
            }
            async fn delete(&self, _id: i64) -> AppResult<()> {
                unreachable!()
            }
            async fn set_secured_by(&self, _id: i64, _target: Option<i64>) -> AppResult<Account> {
                unreachable!()
            }
            async fn set_excluded_from_net_worth(&self, _id: i64, _x: bool) -> AppResult<Account> {
                unreachable!("ForecastService never changes net-worth inclusion")
            }
            async fn set_ownership(&self, _id: i64, _ownership: Ownership) -> AppResult<Account> {
                unreachable!()
            }
            async fn set_ownership_bulk(
                &self,
                _ids: &[i64],
                _ownership: Ownership,
            ) -> AppResult<u64> {
                unreachable!()
            }
            async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>> {
                unreachable!()
            }
            async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>> {
                unreachable!()
            }
            async fn list_house_pricer_subscriptions(
                &self,
            ) -> AppResult<Vec<crate::ports::HousePricerSubscription>> {
                unreachable!()
            }
            async fn set_house_pricer_link(
                &self,
                _account_id: i64,
                _link: Option<sure_core::HousePricerLink>,
            ) -> AppResult<Account> {
                unreachable!()
            }
            async fn set_credit_limit(&self, _account_id: i64, _v: i64) -> AppResult<()> {
                unreachable!()
            }
            async fn set_original_amount(&self, _account_id: i64, _v: i64) -> AppResult<()> {
                unreachable!()
            }
            async fn set_institution_if_unset(&self, _account_id: i64, _v: &str) -> AppResult<()> {
                unreachable!()
            }
            async fn set_account_number_if_unset(
                &self,
                _account_id: i64,
                _v: &str,
            ) -> AppResult<()> {
                unreachable!()
            }
        }

        #[derive(Default)]
        struct FakeReports {
            base_currency: String,
            account_currencies: Vec<AccountCurrency>,
            transactions: Vec<LedgerTx>,
            valuations: Vec<LedgerValuation>,
            categories: Vec<ReportCategory>,
            spend_transactions: Vec<SpendTransaction>,
        }
        #[async_trait]
        impl ReportRepo for FakeReports {
            async fn base_currency(&self) -> AppResult<String> {
                Ok(self.base_currency.clone())
            }
            async fn account_currencies(&self) -> AppResult<Vec<AccountCurrency>> {
                Ok(self.account_currencies.clone())
            }
            // The window parameters are ignored: returning every row is a legal answer to a
            // windowed read (`ReportRepo::transactions` only requires a superset of what the
            // aggregation needs), and the forecast asks for the unwindowed ledger anyway.
            async fn transactions(&self, _from: Option<NaiveDate>) -> AppResult<Vec<LedgerTx>> {
                Ok(self.transactions.clone())
            }
            async fn valuations(
                &self,
                _from: Option<NaiveDate>,
            ) -> AppResult<Vec<LedgerValuation>> {
                Ok(self.valuations.clone())
            }
            async fn categories(&self) -> AppResult<Vec<ReportCategory>> {
                Ok(self.categories.clone())
            }
            async fn spend_transactions(
                &self,
                _from: NaiveDate,
                _to: NaiveDate,
            ) -> AppResult<Vec<SpendTransaction>> {
                Ok(self.spend_transactions.clone())
            }
            async fn earliest_transaction_date(&self) -> AppResult<Option<String>> {
                unreachable!()
            }
            async fn earliest_valuation_date(&self) -> AppResult<Option<String>> {
                unreachable!()
            }
            async fn active_accounts(&self) -> AppResult<Vec<ActiveAccount>> {
                unreachable!()
            }
            async fn account(&self, _id: i64) -> AppResult<AssetAccount> {
                unreachable!()
            }
            async fn secured_liabilities(
                &self,
                _asset_id: i64,
            ) -> AppResult<Vec<SecuredLiabilityAccount>> {
                unreachable!()
            }
        }

        struct FakeFx;
        #[async_trait]
        impl FxRatesRepo for FakeFx {
            async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>> {
                Ok(vec![CurrencyDecimals {
                    code: "NZD".into(),
                    decimal_places: 2,
                }])
            }
            async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>> {
                Ok(Vec::new())
            }
        }

        struct FakeCrons;
        #[async_trait]
        impl CronRepo for FakeCrons {
            async fn list(&self) -> AppResult<Vec<Cron>> {
                Ok(Vec::new())
            }
            async fn create(&self, _input: SaveCron) -> AppResult<Cron> {
                unreachable!()
            }
            async fn update(&self, _id: i64, _input: SaveCron) -> AppResult<Cron> {
                unreachable!()
            }
            async fn delete(&self, _id: i64) -> AppResult<()> {
                unreachable!()
            }
            async fn list_runs(&self, _cron_id: i64) -> AppResult<Vec<CronRun>> {
                unreachable!()
            }
            async fn run_one(&self, _id: i64, _to: Option<&str>) -> AppResult<CronRunResult> {
                unreachable!()
            }
            async fn run_all(&self, _to: Option<&str>) -> AppResult<CronRunResult> {
                unreachable!()
            }
            async fn undo_run(&self, _run_id: i64) -> AppResult<()> {
                unreachable!()
            }
        }

        #[derive(Default)]
        struct FakeForecast {
            events: Vec<ForecastEvent>,
            /// Account-target overrides, as `(target_id, annual_growth_bps,
            /// annual_volatility_bps)` — rows a user wrote through `PUT
            /// /api/forecast/assumptions`, the only route by which a user-chosen growth or
            /// volatility reaches the simulation. Held as the three fields that matter rather
            /// than whole `ForecastAssumption`s because that type isn't `Clone` and this port
            /// hands out owned values.
            overrides: Vec<(i64, Option<i64>, Option<i64>)>,
        }
        #[async_trait]
        impl ForecastRepo for FakeForecast {
            async fn list_assumptions(&self) -> AppResult<Vec<ForecastAssumption>> {
                Ok(self
                    .overrides
                    .iter()
                    .map(|&(target_id, annual_growth_bps, annual_volatility_bps)| {
                        ForecastAssumption {
                            id: target_id,
                            target_type: ForecastTargetType::Account,
                            target_id,
                            annual_growth_bps,
                            annual_volatility_bps,
                            long_run_growth_bps: None,
                            annual_fee_bps: None,
                            annual_fixed_fee_minor: None,
                            dividend_yield_bps: None,
                            notes: None,
                            created_at: "2026-08-01T00:00:00Z".to_string(),
                            updated_at: "2026-08-01T00:00:00Z".to_string(),
                        }
                    })
                    .collect())
            }
            async fn upsert_assumption(
                &self,
                _input: SaveForecastAssumption,
            ) -> AppResult<ForecastAssumption> {
                unreachable!()
            }
            async fn clear_assumption(
                &self,
                _target_type: ForecastTargetType,
                _target_id: i64,
            ) -> AppResult<()> {
                unreachable!()
            }
            async fn trailing_dividends_minor(
                &self,
                _account_id: i64,
                _since: &str,
            ) -> AppResult<i64> {
                Ok(0)
            }
            async fn list_events(&self) -> AppResult<Vec<ForecastEvent>> {
                Ok(self.events.clone())
            }
            async fn get_event(&self, _id: i64) -> AppResult<ForecastEvent> {
                unreachable!()
            }
            async fn create_event(&self, _input: SaveForecastEvent) -> AppResult<ForecastEvent> {
                unreachable!()
            }
            async fn update_event(
                &self,
                _id: i64,
                _input: SaveForecastEvent,
            ) -> AppResult<ForecastEvent> {
                unreachable!()
            }
            async fn delete_event(&self, _id: i64) -> AppResult<()> {
                unreachable!()
            }

            // These sim tests are about assumption resolution and the Monte Carlo loop, so the
            // fake household earns nothing modelled — income streams have their own DAL tests
            // and their own e2e coverage. An empty list is also what makes these tests keep
            // asserting the pre-income behaviour they were written for.
            async fn list_income_streams(&self) -> AppResult<Vec<sure_core::IncomeStream>> {
                Ok(Vec::new())
            }
            async fn get_income_stream(&self, _id: i64) -> AppResult<sure_core::IncomeStream> {
                unreachable!()
            }
            async fn create_income_stream(
                &self,
                _person_id: i64,
                _input: sure_core::SaveIncomeStream,
            ) -> AppResult<sure_core::IncomeStream> {
                unreachable!()
            }
            async fn update_income_stream(
                &self,
                _id: i64,
                _input: sure_core::SaveIncomeStream,
            ) -> AppResult<sure_core::IncomeStream> {
                unreachable!()
            }
            async fn delete_income_stream(&self, _id: i64) -> AppResult<()> {
                unreachable!()
            }
            // Empty: `TaxScales::new` then falls back to the built-in figures, which is what these
            // tests were written against.
            async fn list_tax_scales(&self) -> AppResult<Vec<sure_core::StoredTaxScale>> {
                Ok(Vec::new())
            }
            async fn create_tax_scale(
                &self,
                _scale_id: sure_core::TaxScaleId,
                _input: sure_core::SaveTaxScale,
            ) -> AppResult<sure_core::StoredTaxScale> {
                unreachable!()
            }
            async fn update_tax_scale(
                &self,
                _id: i64,
                _input: sure_core::SaveTaxScale,
            ) -> AppResult<sure_core::StoredTaxScale> {
                unreachable!()
            }
            async fn delete_tax_scale(&self, _id: i64) -> AppResult<()> {
                unreachable!()
            }
            async fn restore_tax_scales(&self) -> AppResult<Vec<sure_core::StoredTaxScale>> {
                unreachable!()
            }
            // Detection has its own unit tests over `crate::detect`; an empty ledger here is honest
            // rather than a panic waiting for whoever adds a sim test that touches it.
            async fn income_transactions(
                &self,
                _from: &str,
                _account_id: Option<i64>,
            ) -> AppResult<Vec<sure_core::Transaction>> {
                Ok(Vec::new())
            }
        }

        fn account(id: i64, kind: AK, currency: &str) -> Account {
            Account {
                id,
                name: format!("Account {id}"),
                kind,
                class: kind.class(),
                currency_code: currency.into(),
                institution: None,
                metadata: AM::Generic(GenericMeta::default()),
                archived: false,
                excluded_from_net_worth: false,
                sort_order: 0,
                secured_by_account_id: None,
                created_at: "2020-01-01T00:00:00Z".into(),
                updated_at: "2020-01-01T00:00:00Z".into(),
                ownership: sure_core::Ownership::Joint,
            }
        }

        /// A mortgage with the terms that make it amortise, plus a valuation so it has a
        /// real balance to anchor to (the closed-form fallback would hide anchoring bugs).
        fn mortgage(id: i64, meta: sure_core::MortgageMeta) -> Account {
            Account {
                kind: AK::Mortgage,
                class: AK::Mortgage.class(),
                metadata: AM::Mortgage(meta),
                ..account(id, AK::Mortgage, "NZD")
            }
        }

        fn valued(account_id: i64, as_of: NaiveDate, value_minor: i64) -> LedgerValuation {
            LedgerValuation {
                account_id,
                as_of: as_of.to_string(),
                value_minor,
                currency_code: "NZD".into(),
            }
        }

        fn make_service(
            accounts: Vec<Account>,
            valuations: Vec<LedgerValuation>,
            transactions: Vec<LedgerTx>,
            categories: Vec<ReportCategory>,
            spend: Vec<SpendTransaction>,
            events: Vec<ForecastEvent>,
            today: NaiveDate,
        ) -> ForecastService {
            let account_currencies = accounts
                .iter()
                .map(|a| AccountCurrency {
                    id: a.id,
                    currency_code: a.currency_code.clone(),
                    ownership: sure_core::Ownership::Joint,
                    excluded_from_net_worth: a.excluded_from_net_worth,
                })
                .collect();
            ForecastService::new(
                Arc::new(FakeForecast {
                    events,
                    ..Default::default()
                }),
                Arc::new(FakeReports {
                    base_currency: "NZD".into(),
                    account_currencies,
                    transactions,
                    valuations,
                    categories,
                    spend_transactions: spend,
                }),
                Arc::new(FakeFx),
                Arc::new(FakeAccounts(accounts)),
                Arc::new(FakeCrons),
                Arc::new(crate::test_clock::FixedClock(today)),
            )
        }

        /// The two-step path `GET /api/forecast` takes — [`ForecastService::simulate_inputs`]
        /// on a runtime worker, then [`ForecastService::simulate_from`] on the blocking pool —
        /// must be the *same* simulation as one-shot [`ForecastService::simulate`], down to
        /// the last minor unit of every band and every reported assumption.
        ///
        /// This is the acceptance criterion for having moved the Monte Carlo loop off the
        /// async workers at all. Getting the CPU off the reactor is a *scheduling* change; a
        /// scheduling change that moves a figure is not an optimisation, it is a wrong number
        /// in the household's forecast. The two live risks it pins: the seed being re-drawn on
        /// the compute side (which would make the split path non-reproducible even under an
        /// explicit `seed`), and the per-account/per-category setup loops being reordered
        /// relative to the draws they feed, since `StdRng` is a stream and the *order* of
        /// samples is part of the answer.
        ///
        /// It also runs `simulate_from` outside `block_on` entirely, which is the other half
        /// of the contract: the compute half must not need a reactor, because on the blocking
        /// pool there isn't one.
        #[test]
        fn simulate_matches_the_two_step_split() {
            let today = d("2026-07-01");

            // A stochastic account (valuation series), a cash account with real movements,
            // and an income category — so all three of `account_sims`, the cash pool, and
            // `category_sims` are exercised rather than just the first.
            let monthly = (1.08f64).powf(1.0 / 12.0);
            let mut v = 250_000_00i64;
            let mut valuations = Vec::new();
            for i in 0..24 {
                let date = today - chrono::Duration::days((24 - i) * 30);
                valuations.push(valued(1, date, v));
                v = (v as f64 * monthly) as i64;
            }
            let mut txns = Vec::new();
            let mut spend = Vec::new();
            for i in 0..24 {
                let date = today - chrono::Duration::days((24 - i) * 30);
                txns.push(LedgerTx {
                    account_id: 2,
                    posted_at: date.to_string(),
                    amount_minor: 400_000 + i * 1_000,
                    currency_code: "NZD".to_string(),
                });
                spend.push(SpendTransaction {
                    posted_at: date.to_string(),
                    amount_minor: 400_000 + i * 1_000,
                    currency_code: "NZD".into(),
                    category_id: Some(10),
                    is_one_off: false,
                    linked_transaction_id: None,
                    account_id: 1,
                    account_name: "Bank".into(),
                    account_kind: AK::Bank,
                    merchant_id: None,
                    merchant: None,
                    attribution: sure_core::Ownership::Joint,
                });
            }

            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = make_service(
                vec![
                    account(1, AK::Brokerage, "NZD"),
                    account(2, AK::Bank, "NZD"),
                ],
                valuations,
                txns,
                vec![ReportCategory {
                    id: 10,
                    parent_id: None,
                    name: "Salary".into(),
                    color: None,
                    kind: CategoryKind::Income,
                }],
                spend,
                Vec::new(),
                today,
            );

            let params = SimulationParams {
                horizon_months: 6,
                simulations: 300,
                currency: None,
                seed: Some(1234),
            };

            let one_shot = rt.block_on(svc.simulate(&params)).unwrap();
            let inputs = rt.block_on(svc.simulate_inputs(&params)).unwrap();
            let split = ForecastService::simulate_from(inputs).unwrap();

            // Guards the assertion below against passing on two empty results.
            assert_eq!(one_shot.months.len(), 6);
            assert!(!one_shot.assumptions.is_empty());
            assert_ne!(one_shot.months[0].net_worth.median_minor, 0);

            // Whole-result structural equality, rather than a field-by-field list that a new
            // field could silently escape.
            assert_eq!(format!("{one_shot:#?}"), format!("{split:#?}"));
        }

        /// A brokerage account with two years of steady 10%/yr appreciation should
        /// project forward with that same trend, and the same seed must reproduce
        /// byte-identical output — a Monte Carlo forecast that isn't reproducible under
        /// a fixed seed can't be meaningfully tested or trusted.
        #[test]
        fn simulate_is_reproducible_and_percentile_ordered() {
            let today = d("2026-07-01");
            let monthly = (1.10f64).powf(1.0 / 12.0);
            let mut v = 500_000_00i64;
            let mut valuations = Vec::new();
            for i in 0..24 {
                let date = today - chrono::Duration::days((24 - i) * 30);
                valuations.push(LedgerValuation {
                    account_id: 1,
                    as_of: date.to_string(),
                    value_minor: v,
                    currency_code: "NZD".into(),
                });
                v = (v as f64 * monthly) as i64;
            }
            let accounts = vec![account(1, AK::Brokerage, "NZD")];

            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = make_service(
                accounts,
                valuations,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );

            let params = SimulationParams {
                horizon_months: 6,
                simulations: 200,
                currency: None,
                seed: Some(42),
            };
            let a = rt.block_on(svc.simulate(&params)).unwrap();
            let b = rt.block_on(svc.simulate(&params)).unwrap();

            assert_eq!(a.months.len(), 6);
            for (ma, mb) in a.months.iter().zip(&b.months) {
                assert_eq!(ma.net_worth.median_minor, mb.net_worth.median_minor);
            }
            for month in &a.months {
                assert!(month.net_worth.p10_minor <= month.net_worth.median_minor);
                assert!(month.net_worth.median_minor <= month.net_worth.p90_minor);
            }
            // A steadily-appreciating account with no cash/liabilities: net worth should
            // trend up over the 6-month horizon.
            assert!(
                a.months.last().unwrap().net_worth.median_minor
                    > a.months[0].net_worth.median_minor
            );
        }

        /// A `step_change` forecast event on an income category (a known promotion) must
        /// raise the projected cash position from its effective month on, relative to a
        /// run with no event at all — the exact "teacher's known fixed pay schedule" case
        /// from the feature request.
        #[test]
        fn a_promotion_step_change_raises_the_projection_from_its_month_on() {
            let today = d("2026-07-01");
            let mut cash = Vec::new();
            let mut txns = Vec::new();
            let mut spend = Vec::new();
            for i in 0..12 {
                let date = today - chrono::Duration::days((12 - i) * 30);
                txns.push(LedgerTx {
                    account_id: 1,
                    posted_at: date.to_string(),
                    amount_minor: 500_000,
                    currency_code: "NZD".to_string(),
                });
                spend.push(SpendTransaction {
                    posted_at: date.to_string(),
                    amount_minor: 500_000,
                    currency_code: "NZD".into(),
                    category_id: Some(10),
                    is_one_off: false,
                    linked_transaction_id: None,
                    account_id: 1,
                    account_name: "Bank".into(),
                    account_kind: AK::Bank,
                    merchant_id: None,
                    merchant: None,
                    attribution: sure_core::Ownership::Joint,
                });
                cash.push(500_000);
            }
            let accounts = vec![account(1, AK::Bank, "NZD")];
            let categories = vec![ReportCategory {
                id: 10,
                parent_id: None,
                name: "Salary".into(),
                color: None,
                kind: CategoryKind::Income,
            }];

            let rt = tokio::runtime::Runtime::new().unwrap();

            let base_svc = make_service(
                accounts.clone(),
                Vec::new(),
                txns.clone(),
                categories.clone(),
                spend.clone(),
                Vec::new(),
                today,
            );
            let promoted_svc = make_service(
                accounts,
                Vec::new(),
                txns,
                categories,
                spend,
                vec![ForecastEvent {
                    id: 1,
                    label: "Promotion".into(),
                    kind: sure_core::LifeEventKind::Promotion,
                    person_id: None,
                    expected_on: today.to_string(),
                    // A certainty — 100% likely, no timing spread — which is exactly what every
                    // event meant before probability existed. That is what keeps this test the
                    // right assertion about a step change under the unified model.
                    timing_spread_months: 0,
                    probability_bps: 10_000,
                    notes: None,
                    effects: vec![sure_core::ForecastEventEffect {
                        id: 1,
                        event_id: 1,
                        sort_order: 0,
                        spec: LifeEffectSpec::SetBaseline {
                            target: EffectTarget::Category { category_id: 10 },
                            amount_minor: 1_000_000, // double the ~$5,000/mo baseline
                        },
                    }],
                    relations: Vec::new(),
                    created_at: "2026-07-01T00:00:00Z".into(),
                    updated_at: "2026-07-01T00:00:00Z".into(),
                }],
                today,
            );

            let params = SimulationParams {
                horizon_months: 3,
                simulations: 500,
                currency: None,
                seed: Some(7),
            };
            let base = rt.block_on(base_svc.simulate(&params)).unwrap();
            let promoted = rt.block_on(promoted_svc.simulate(&params)).unwrap();

            for (b, p) in base.months.iter().zip(&promoted.months) {
                assert!(
                    p.net_worth.median_minor > b.net_worth.median_minor,
                    "promoted median {} should exceed base median {}",
                    p.net_worth.median_minor,
                    b.net_worth.median_minor
                );
            }
        }

        /// Terms modelled on a representative ASB loan: $485k over 27
        /// years at 5.12%, fixed for another few months and then refixed.
        fn asb_terms(fixed_until: Option<&str>, uncertainty_bps: Option<i64>) -> MortgageMeta {
            MortgageMeta {
                lender: Some("ASB".into()),
                original_amount_minor: Some(485_000_00),
                interest_rate_bps: Some(512),
                rate_type: Some(sure_core::RateType::Fixed),
                fixed_until: fixed_until.map(Into::into),
                term_months: Some(324),
                start_date: Some("2025-12-11".into()),
                refix_rate_bps: fixed_until.map(|_| 512),
                refix_rate_uncertainty_bps: uncertainty_bps,
                ..Default::default()
            }
        }

        fn mortgage_service(meta: MortgageMeta, today: NaiveDate) -> ForecastService {
            make_service(
                vec![mortgage(1, meta)],
                vec![valued(1, today - chrono::Duration::days(1), -478_940_17)],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            )
        }

        /// The crux of the feature. Until the fixed rate expires the balance is genuinely
        /// certain, so every path must agree to the cent; once it rolls off onto a rate
        /// drawn per path, the band has to open up. A forecast that shows a mortgage as a
        /// single confident line for thirty years is the thing being fixed.
        #[test]
        fn simulate_widens_the_liability_band_only_after_the_refix() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = mortgage_service(asb_terms(Some("2026-11-01"), Some(150)), today);

            let result = rt
                .block_on(svc.simulate(&SimulationParams {
                    horizon_months: 24,
                    simulations: 1000,
                    currency: None,
                    seed: Some(11),
                }))
                .unwrap();

            // Months 1 and 2 — the refix itself lands on month 3, so that one already
            // carries a drawn rate.
            for m in &result.months[..2] {
                assert_eq!(
                    m.liabilities.p10_minor, m.liabilities.p90_minor,
                    "{}: a fixed rate is certain, so every path must agree",
                    m.as_of
                );
            }
            assert!(
                result.months[2].liabilities.p90_minor - result.months[2].liabilities.p10_minor > 0,
                "the band should open the moment the rate is drawn"
            );
            let last = &result.months[23];
            assert!(
                last.liabilities.p90_minor - last.liabilities.p10_minor > 0,
                "{}: the refix draw must open the band",
                last.as_of
            );
            assert!(last.liabilities.p10_minor <= last.liabilities.median_minor);
            assert!(last.liabilities.median_minor <= last.liabilities.p90_minor);
        }

        /// A certain refix is still deterministic: zero uncertainty must collapse the band
        /// again, so the width is attributable to the draw and nothing else.
        #[test]
        fn simulate_keeps_a_certain_refix_deterministic() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = mortgage_service(asb_terms(Some("2026-11-01"), Some(0)), today);

            let result = rt
                .block_on(svc.simulate(&SimulationParams {
                    horizon_months: 24,
                    simulations: 300,
                    currency: None,
                    seed: Some(11),
                }))
                .unwrap();
            for m in &result.months {
                assert_eq!(
                    m.liabilities.p10_minor, m.liabilities.p90_minor,
                    "{}",
                    m.as_of
                );
            }
        }

        /// Out of net worth has to mean out of the *projection* of net worth. The Forecast page
        /// draws its history from `/api/reports/net-worth` and its projection from here, on one
        /// axis meeting at today — so an account excluded from the first and kept in the second
        /// puts a step at the seam exactly the size of that account.
        ///
        /// An *appreciating* account, so that leaving it in would diverge further every month
        /// rather than only offsetting month zero.
        #[test]
        fn an_excluded_account_is_out_of_the_projection() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let yesterday = today - chrono::Duration::days(1);
            let build = |excluded: bool| {
                let mut house = account(2, AK::RealEstate, "NZD");
                house.excluded_from_net_worth = excluded;
                make_service(
                    vec![account(1, AK::Bank, "NZD"), house],
                    vec![
                        valued(1, yesterday, 10_000_00),
                        valued(2, yesterday, 800_000_00),
                    ],
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    today,
                )
            };
            let params = SimulationParams {
                horizon_months: 12,
                simulations: 100,
                currency: None,
                seed: Some(7),
            };
            let with = rt.block_on(build(false).simulate(&params)).unwrap();
            let without = rt.block_on(build(true).simulate(&params)).unwrap();

            // Month zero is the seam with the reports, so it must move by the house exactly.
            assert_eq!(
                with.months[0].net_worth.median_minor - without.months[0].net_worth.median_minor,
                800_000_00
            );
            // And the house must not be growing inside the excluded projection at all.
            assert!(
                with.months[11].net_worth.median_minor - without.months[11].net_worth.median_minor
                    >= 800_000_00,
                "the excluded house must contribute nothing, including its growth"
            );
        }

        /// The same, for a *cash* account — which never reaches `account_sims` at all (the
        /// class check skips it) and is instead pooled into `cash_start`. A fix applied only
        /// to the simulation loop would pass the test above and silently fail this one.
        #[test]
        fn an_excluded_cash_account_is_out_of_the_starting_cash_pool() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let yesterday = today - chrono::Duration::days(1);
            let build = |excluded: bool| {
                let mut savings = account(2, AK::Savings, "NZD");
                savings.excluded_from_net_worth = excluded;
                make_service(
                    vec![account(1, AK::Bank, "NZD"), savings],
                    vec![
                        valued(1, yesterday, 10_000_00),
                        valued(2, yesterday, 50_000_00),
                    ],
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    today,
                )
            };
            let params = SimulationParams {
                horizon_months: 6,
                simulations: 50,
                currency: None,
                seed: Some(11),
            };
            let with = rt.block_on(build(false).simulate(&params)).unwrap();
            let without = rt.block_on(build(true).simulate(&params)).unwrap();

            assert_eq!(
                with.months[0].net_worth.median_minor - without.months[0].net_worth.median_minor,
                50_000_00
            );
        }

        /// Servicing a mortgage costs real money. The debt shrinking while no cash leaves
        /// was worth roughly a year's interest in over-projected net worth.
        ///
        /// The bank account is what makes the two halves separable: with cash comfortably
        /// positive it stays in `assets`, so `liabilities` is the loan alone and the
        /// repayment shows up as assets falling rather than as the pool going negative.
        #[test]
        fn simulate_debits_cash_for_the_repayment() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let yesterday = today - chrono::Duration::days(1);
            let svc = make_service(
                vec![
                    mortgage(1, asb_terms(None, None)),
                    account(2, AK::Bank, "NZD"),
                ],
                vec![
                    valued(1, yesterday, -478_940_17),
                    valued(2, yesterday, 200_000_00),
                ],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );
            let result = rt
                .block_on(svc.simulate(&SimulationParams {
                    horizon_months: 12,
                    simulations: 100,
                    currency: None,
                    seed: Some(3),
                }))
                .unwrap();

            let first = &result.months[0];
            let last = &result.months[11];
            // The debt is paid down…
            assert!(
                last.liabilities.median_minor > first.liabilities.median_minor,
                "the balance should be shrinking toward zero: {} -> {}",
                first.liabilities.median_minor,
                last.liabilities.median_minor
            );
            // …and the cash to do it actually leaves.
            assert!(
                last.assets.median_minor < first.assets.median_minor,
                "cash should be spent on the repayment: {} -> {}",
                first.assets.median_minor,
                last.assets.median_minor
            );
            // Net worth falls by the interest: the principal just moves between the two
            // sides, but the interest is gone. ~5.12% on ~$479k is ~$2,043/mo at the
            // start, so eleven months of it is comfortably over $20k.
            let drop = first.net_worth.median_minor - last.net_worth.median_minor;
            assert!(
                (20_000_00..30_000_00).contains(&drop),
                "expected roughly a year of interest, got {drop}"
            );
        }

        /// A student loan's repayment comes out of pay before the salary ever lands, so it
        /// is already inside the income baseline; debiting it again would double-count with
        /// nothing in the ledger to reveal it. That used to rest on `repayment_debits_cash`,
        /// because a student loan could be handed a schedule. It now rests on the profile:
        /// `StudentLoanMeta` has no terms, so the account is projected from its trend and
        /// the branch that debits the pool is unreachable for it.
        #[test]
        fn simulate_does_not_debit_cash_for_a_student_loan() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let student = Account {
                kind: AK::StudentLoan,
                class: AK::StudentLoan.class(),
                metadata: AM::StudentLoan(StudentLoanMeta {
                    lender: Some("Inland Revenue".into()),
                    interest_rate_bps: Some(0),
                    ..Default::default()
                }),
                ..account(1, AK::StudentLoan, "NZD")
            };
            // Two years of PAYE deductions at ~$250/mo, which is the only thing the
            // projection now has to go on — and, for this loan, the only thing that was ever
            // true. It is what the myIR import and the balance-delta task build (see
            // docs/STUDENT-LOAN.md), so it is also what a real account arrives with.
            let mut valuations = Vec::new();
            for i in 0..24 {
                valuations.push(valued(
                    1,
                    add_months(d("2024-08-01"), i),
                    -36_000_00 + (i * 250_00),
                ));
            }
            let svc = make_service(
                vec![student],
                valuations,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );
            let result = rt
                .block_on(svc.simulate(&SimulationParams {
                    horizon_months: 6,
                    simulations: 100,
                    currency: None,
                    seed: Some(5),
                }))
                .unwrap();

            // The debt keeps shrinking on the trend it has been shrinking on, and nothing
            // offsets it, so net worth rises.
            assert!(
                result.months[5].net_worth.median_minor > result.months[0].net_worth.median_minor
            );
            // No cash pool exists here, so any debit would show as a negative asset side.
            assert_eq!(result.months[5].assets.median_minor, 0);
        }

        /// The structural half of the above, stated on its own so a regression names itself:
        /// a student loan cannot be projected as an amortisation schedule, however complete
        /// its metadata is, because its profile has nowhere to put a principal or a term.
        /// This is the check that replaces docs/STUDENT-LOAN.md's "don't set `term_months`"
        /// trap — the field it warned about no longer exists on the profile.
        #[test]
        fn a_student_loan_is_never_projected_as_a_schedule() {
            let today = d("2026-08-01");
            let complete = AM::StudentLoan(StudentLoanMeta {
                lender: Some("Inland Revenue".into()),
                interest_rate_bps: Some(0),
                url: Some("https://myir.ird.govt.nz".into()),
                notes: None,
            });
            assert!(loan_terms(&complete, today).is_none());
            // …while a table loan carrying the same terms a student loan used to be asked for
            // still is one, so this is the profile split doing the work, not a lost feature.
            let as_a_table_loan = AM::Loan(LoanMeta {
                original_amount_minor: Some(50_000_00),
                interest_rate_bps: Some(0),
                term_months: Some(120),
                start_date: Some("2024-01-01".into()),
                ..Default::default()
            });
            assert!(loan_terms(&as_a_table_loan, today).is_some());
        }

        /// A student loan drawn down over years of study and repaid ever since: a single
        /// straight line through the *whole* history slopes downward (it ends deeper in debt
        /// than it began), so the derived rate said the debt was growing at the very moment
        /// it was being cleared. Only the recent past should feed the fit.
        #[test]
        fn a_liability_trend_follows_recent_behaviour_not_the_whole_history() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            // Drawn down to -$56k over three years, then repaid to -$30k over the next two.
            let mut valuations = Vec::new();
            for i in 0..66 {
                let date = add_months(d("2021-03-01"), i);
                let value = if i <= 33 {
                    -13_500_00 - (i * 1_290_00)
                } else {
                    -56_100_00 + ((i - 33) * 790_00)
                };
                valuations.push(valued(1, date, value));
            }
            let loan = Account {
                kind: AK::StudentLoan,
                class: AK::StudentLoan.class(),
                ..account(1, AK::StudentLoan, "NZD")
            };
            let svc = make_service(
                vec![loan],
                valuations,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );

            let a = rt.block_on(svc.resolved_assumptions()).unwrap();
            let loan = a
                .iter()
                .find(|a| a.target_type == ForecastTargetType::Account && a.target_id == 1)
                .unwrap();
            // A *negative* relative rate on a negative balance moves it toward zero.
            assert!(
                loan.annual_growth_bps < 0,
                "the debt is being repaid, so the trend must point at zero, got {}",
                loan.annual_growth_bps
            );
        }

        /// …and the projection has to actually get there. A liability's rate is fitted as a
        /// straight line, so compounding it instead leaves a loan three years from payoff
        /// still showing a balance a decade out.
        #[test]
        fn a_repaid_liability_reaches_zero_and_stays_there() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            // -$12,000, repaid at a steady $1,000/month: cleared in a year.
            let mut valuations = Vec::new();
            for i in 0..24 {
                let date = add_months(d("2024-08-01"), i);
                valuations.push(valued(1, date, -36_000_00 + (i * 1_000_00)));
            }
            let loan = Account {
                kind: AK::StudentLoan,
                class: AK::StudentLoan.class(),
                ..account(1, AK::StudentLoan, "NZD")
            };
            let svc = make_service(
                vec![loan],
                valuations,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );
            let result = rt
                .block_on(svc.simulate(&SimulationParams {
                    horizon_months: 36,
                    simulations: 200,
                    currency: None,
                    seed: Some(4),
                }))
                .unwrap();

            // Cleared well inside the horizon, and it does not drift past zero into being
            // an asset once it gets there.
            let last = &result.months[35];
            assert_eq!(
                last.liabilities.median_minor, 0,
                "expected the loan cleared, got {}",
                last.liabilities.median_minor
            );
            assert!(last.liabilities.p90_minor <= 0);
            assert!(
                result.months[0].liabilities.median_minor < 0,
                "starts owing"
            );
        }

        /// Lumpy spending must not compound into the projection. A category with violent
        /// month-to-month swings but a stable long-run rate should widen the band roughly
        /// with √months (deviations averaging out), not with months³ᐟ² — the signature of
        /// the old model, where each month's noise was folded back into the run-rate and an
        /// expensive January raised every month after it.
        ///
        /// Checked as a ratio rather than an absolute: the 48-month spread should be about
        /// √4 = 2× the 12-month spread, and is nowhere near the 8× a random walk gives.
        #[test]
        fn category_noise_averages_out_instead_of_compounding() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let salary = ReportCategory {
                id: 10,
                parent_id: None,
                name: "Salary".into(),
                color: None,
                kind: CategoryKind::Income,
            };
            // Wildly lumpy but with no trend: alternating $2k and $10k months for two years.
            let mut txns = Vec::new();
            let mut spend = Vec::new();
            for i in 0..24 {
                let date = add_months(today, -(24 - i));
                let amount = if i % 2 == 0 { 200_000 } else { 1_000_000 };
                txns.push(LedgerTx {
                    account_id: 1,
                    posted_at: date.to_string(),
                    amount_minor: amount,
                    currency_code: "NZD".to_string(),
                });
                spend.push(SpendTransaction {
                    posted_at: date.to_string(),
                    amount_minor: amount,
                    currency_code: "NZD".into(),
                    category_id: Some(10),
                    is_one_off: false,
                    linked_transaction_id: None,
                    account_id: 1,
                    account_name: "Bank".into(),
                    account_kind: AK::Bank,
                    merchant_id: None,
                    merchant: None,
                    attribution: sure_core::Ownership::Joint,
                });
            }
            let svc = make_service(
                vec![account(1, AK::Bank, "NZD")],
                Vec::new(),
                txns,
                vec![salary],
                spend,
                Vec::new(),
                today,
            );
            let result = rt
                .block_on(svc.simulate(&SimulationParams {
                    horizon_months: 48,
                    simulations: 2000,
                    currency: None,
                    seed: Some(21),
                }))
                .unwrap();

            let spread = |i: usize| {
                (result.months[i].net_worth.p90_minor - result.months[i].net_worth.p10_minor) as f64
            };
            let ratio = spread(47) / spread(11);
            assert!(
                (1.2..4.0).contains(&ratio),
                "48mo spread should be ~2x the 12mo spread (sqrt-of-time), got {ratio:.1}x"
            );
        }

        /// A card balance was invisible to the forecast: skipped as an account (it has no
        /// growth rate) *and* left out of the cash pool, so the projection started above
        /// the net worth the reports show for today.
        #[test]
        fn simulate_pools_a_credit_card_balance_into_cash() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let params = SimulationParams {
                horizon_months: 3,
                simulations: 100,
                currency: None,
                seed: Some(9),
            };
            let house = account(1, AK::RealEstate, "NZD");
            let card = account(2, AK::CreditCard, "NZD");
            let valuations = vec![
                valued(1, today - chrono::Duration::days(1), 770_000_00),
                valued(2, today - chrono::Duration::days(1), -2_000_00),
            ];

            let without = make_service(
                vec![house.clone()],
                vec![valuations[0].clone()],
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );
            let with = make_service(
                vec![house, card],
                valuations,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                today,
            );

            let a = rt.block_on(without.simulate(&params)).unwrap();
            let b = rt.block_on(with.simulate(&params)).unwrap();
            assert_eq!(
                a.months[0].net_worth.median_minor - b.months[0].net_worth.median_minor,
                2_000_00,
                "the card balance must reduce projected net worth"
            );
        }

        /// The Forecast page shows "amortisation schedule" and nothing else for a
        /// mortgage; the schedule is what lets it show its working instead.
        #[test]
        fn resolved_assumptions_surfaces_the_schedule() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = mortgage_service(asb_terms(Some("2027-01-11"), Some(150)), today);

            let assumptions = rt.block_on(svc.resolved_assumptions()).unwrap();
            let a = assumptions
                .iter()
                .find(|a| a.target_type == ForecastTargetType::Account && a.target_id == 1)
                .unwrap();

            assert_eq!(a.source, AssumptionSource::Deterministic);
            assert_eq!(a.currency_code.as_deref(), Some("NZD"));
            let s = a.schedule.expect("a complete mortgage has a schedule");
            assert_eq!(s.current_rate_bps, 512);
            // 324 months from 2025-12-11, of which 8 have elapsed by 2026-08-01.
            assert_eq!(s.remaining_term_months, 316);
            assert_eq!(s.refix_in_months, Some(5));
            assert_eq!(s.refix_rate_bps, Some(512));
            assert_eq!(s.refix_rate_uncertainty_bps, Some(150));
            // $478,940.17 at 5.12% over 316 months. Cross-check: a comparable loan pays
            // $1,292.15/fortnight, i.e. $2,799/mo — a little above the table payment,
            // which is what recording `repayment_minor` is for.
            assert!(
                (s.monthly_payment_minor - 2_763_00).abs() < 5_00,
                "payment {} should be about $2,763/mo",
                s.monthly_payment_minor
            );
        }

        /// A brokerage account with a stored volatility override, and a valuation so it has a
        /// balance to compound.
        fn service_with_override(
            annual_volatility_bps: Option<i64>,
            today: NaiveDate,
        ) -> ForecastService {
            let accounts = vec![account(1, AK::Brokerage, "NZD")];
            ForecastService::new(
                Arc::new(FakeForecast {
                    events: Vec::new(),
                    overrides: vec![(1, Some(700), annual_volatility_bps)],
                }),
                Arc::new(FakeReports {
                    base_currency: "NZD".into(),
                    account_currencies: accounts
                        .iter()
                        .map(|a| AccountCurrency {
                            id: a.id,
                            currency_code: a.currency_code.clone(),
                            ownership: sure_core::Ownership::Joint,
                            excluded_from_net_worth: a.excluded_from_net_worth,
                        })
                        .collect(),
                    valuations: vec![valued(1, today - chrono::Duration::days(1), 500_000_00)],
                    ..Default::default()
                }),
                Arc::new(FakeFx),
                Arc::new(FakeAccounts(accounts)),
                Arc::new(FakeCrons),
                Arc::new(crate::test_clock::FixedClock(today)),
            )
        }

        /// The W-08 recurrence guard, end to end. `upsert_assumption` now refuses a volatility
        /// this large, but a row written before that check existed is still sitting in the
        /// table — and it must not be able to take `GET /api/forecast` down, because the only
        /// control that could clear it lives on the page the 500 breaks.
        ///
        /// Unclamped, ~1e14 bps makes `exp()` both underflow to `0.0` and overflow to `inf`
        /// within a single path; `0.0 * inf` is `NaN`, `NaN >= 0.0` is false so it files itself
        /// under liabilities, and the percentile sort panicked on it.
        #[test]
        fn simulate_survives_an_absurd_stored_volatility_override() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let params = SimulationParams {
                horizon_months: 24,
                simulations: 2000,
                currency: None,
                seed: Some(13),
            };
            let result = rt
                .block_on(
                    service_with_override(Some(1_000_000_000_000_00), today).simulate(&params),
                )
                .expect("an absurd stored volatility must not fail the projection");

            for m in &result.months {
                for band in [&m.net_worth, &m.assets, &m.liabilities] {
                    // `i64` cannot hold a `NaN`, so the assertion that matters is ordering:
                    // `NaN` sorted anywhere and the percentiles came out unordered even when
                    // the sort didn't panic outright.
                    assert!(band.p10_minor <= band.median_minor, "{}", m.as_of);
                    assert!(band.median_minor <= band.p90_minor, "{}", m.as_of);
                }
                // Clamped to 300%/yr the projection is wild but finite: no month may collapse
                // a $500k holding to nothing, which is what an `exp()` underflow looked like.
                assert!(m.assets.p90_minor > 0, "{} lost the account", m.as_of);
            }
        }

        /// …and the clamp is a ceiling, not a flattening: an ordinary override still drives
        /// the noise it asks for, so the guard cannot be mistaken for "volatility ignored".
        #[test]
        fn simulate_still_honours_an_ordinary_volatility_override() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let params = SimulationParams {
                horizon_months: 12,
                simulations: 1000,
                currency: None,
                seed: Some(13),
            };
            let spread = |vol| {
                let r = rt
                    .block_on(service_with_override(vol, today).simulate(&params))
                    .unwrap();
                let m = &r.months[11];
                m.net_worth.p90_minor - m.net_worth.p10_minor
            };
            assert_eq!(spread(Some(0)), 0, "no volatility, no band");
            assert!(spread(Some(2_000)) > 0, "20%/yr must open the band");
            assert!(
                spread(Some(10_000)) > spread(Some(2_000)),
                "more volatility, wider band"
            );
        }

        /// W-16 on the forecast: `?currency=` takes the same door as the reports' and gets the
        /// same answer. A projection denominated in a currency that has no `currencies` row
        /// has no scale and no rate, so every account falls out of it — a 200 describing
        /// nothing, where a 400 naming the code belongs.
        #[test]
        fn simulate_rejects_an_unknown_currency() {
            let today = d("2026-08-01");
            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = service_with_override(Some(1_500), today);
            let at = |currency: Option<&str>| {
                rt.block_on(svc.simulate(&SimulationParams {
                    horizon_months: 3,
                    simulations: 100,
                    currency: currency.map(str::to_string),
                    seed: Some(2),
                }))
            };

            let err = at(Some("ZZZ")).expect_err("ZZZ is not a currency");
            assert_eq!(err.code(), "bad_request", "got {err:?}");
            assert!(err.to_string().contains("ZZZ"), "{err}");

            // The base currency (the only one `FakeFx` knows) still works, in either case, and
            // omitting the param is unchanged.
            for currency in [None, Some("NZD"), Some("nzd"), Some("")] {
                assert_eq!(at(currency).unwrap().currency, "NZD", "{currency:?}");
            }
        }
    }
}
