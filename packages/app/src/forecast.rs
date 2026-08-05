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
use rand::SeedableRng;
use rand_distr::{Distribution, Normal};

use sure_core::{
    AccountClass, AccountKind, AccountMetadata, AppResult, CategoryKind, CronKind,
    ForecastAssumption, ForecastEvent, ForecastEventKind, ForecastTargetType, Interval, RateType,
    RepaymentFrequency,
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
const MAX_HORIZON_MONTHS: i64 = 60;
const MIN_SIMULATIONS: i64 = 100;
const MAX_SIMULATIONS: i64 = 5000;
const DEFAULT_HORIZON_MONTHS: i64 = 12;
const DEFAULT_SIMULATIONS: i64 = 2000;
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
}

#[derive(Debug, Clone)]
pub struct ResolvedAssumption {
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    pub label: String,
    pub annual_growth_bps: i64,
    pub annual_volatility_bps: i64,
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
    tx_by_acct: HashMap<i64, Vec<(NaiveDate, i64)>>,
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
        let mut out = self.resolve_account_assumptions(today, &by_target).await?;
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
    ) -> AppResult<Vec<ResolvedAssumption>> {
        let accounts = self.accounts.list(false).await?;
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
                let (current_minor, _) = reports::account_value_at(
                    a.id,
                    &a.currency_code,
                    today,
                    &tx_by_acct,
                    &val_by_acct,
                );
                // The same constructor the simulation uses, at the stated refix rate — so
                // the figure shown and the figure projected cannot drift apart.
                let schedule = AmortSchedule::expected(&terms, current_minor as f64, today);
                out.push(ResolvedAssumption {
                    target_type: ForecastTargetType::Account,
                    target_id: a.id,
                    label: a.name.clone(),
                    annual_growth_bps: 0,
                    annual_volatility_bps: 0,
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
                monthly_value_series(a.id, &a.currency_code, today, &tx_by_acct, &val_by_acct);
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

    pub async fn create_event(
        &self,
        input: sure_core::SaveForecastEvent,
    ) -> AppResult<ForecastEvent> {
        self.forecast.create_event(input).await
    }

    pub async fn delete_event(&self, id: i64) -> AppResult<()> {
        self.forecast.delete_event(id).await
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
        let today = self.clock.today();
        let horizon = params
            .horizon_months
            .clamp(MIN_HORIZON_MONTHS, MAX_HORIZON_MONTHS);
        let n_paths = params.simulations.clamp(MIN_SIMULATIONS, MAX_SIMULATIONS) as usize;

        let (base, fx) = self.currency_and_fx(params.currency.as_deref()).await?;

        let assumptions = self.resolved_assumptions_with(&fx).await?;
        let by_target: HashMap<(ForecastTargetType, i64), &ResolvedAssumption> = assumptions
            .iter()
            .map(|a| ((a.target_type, a.target_id), a))
            .collect();
        let events = self.forecast.list_events().await?;

        let accounts = self.accounts.list(false).await?;
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

            let (current_minor, _) =
                reports::account_value_at(a.id, &a.currency_code, today, &tx_by_acct, &val_by_acct);
            let current = current_minor as f64;

            // Resolved once per account, not once per (path × month × account): the whole
            // projection is carried in native units and only converted for the monthly
            // totals. `None` means no rate reaches the projection currency, so the account is
            // out of the simulation entirely and its currency is reported in `unconverted` —
            // a starting balance taken at parity would be wrong in every month of every path.
            let Some(base_scale) = fx.try_base_scale(&a.currency_code) else {
                continue;
            };

            let projection = if let Some(terms) = loan_terms(&a.metadata, today) {
                AccountProjection::Deterministic(terms)
            } else {
                let resolved = by_target.get(&(ForecastTargetType::Account, a.id));
                let annual_growth = resolved.map(|r| r.annual_growth_bps).unwrap_or(0);
                let annual_vol = resolved.map(|r| r.annual_volatility_bps).unwrap_or(0);
                let monthly_log_return = annual_rate_to_monthly_log_return(annual_growth);
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
                    AccountProjection::LinearPaydown {
                        monthly_delta: current * (monthly_log_return.exp() - 1.0),
                        monthly_vol_abs: current.abs() * monthly_vol,
                    }
                } else {
                    AccountProjection::Stochastic {
                        monthly_log_return,
                        monthly_vol,
                    }
                }
            };

            // Events only apply to non-deterministic accounts for now — a fully
            // amortising mortgage/loan projects from its own terms alone.
            let (step_changes, one_offs) =
                if matches!(projection, AccountProjection::Deterministic(_)) {
                    (Vec::new(), Vec::new())
                } else {
                    account_events(&events, a.id, today, horizon)
                };

            account_sims.push(AccountSim {
                base_scale,
                current,
                projection,
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
                step_changes,
                one_offs,
            });
        }

        let mut category_sims = Vec::new();
        for a in &assumptions {
            if a.target_type != ForecastTargetType::Category {
                continue;
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
            let (step_changes, one_offs) =
                category_events(&events, a.target_id, today, horizon, &fx, &base);
            category_sims.push(CategorySim {
                is_income: source_kind_is_income(self, a.target_id).await?,
                baseline,
                monthly_log_return: annual_rate_to_monthly_log_return(a.annual_growth_bps),
                monthly_vol_fraction: annual_vol_to_monthly_sd(a.annual_volatility_bps),
                step_changes,
                one_offs,
            });
        }

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
                let (v, _) = reports::account_value_at(
                    a.id,
                    &a.currency_code,
                    today,
                    &tx_by_acct,
                    &val_by_acct,
                );
                fx.try_to_base_major(v, &a.currency_code)
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

        for _ in 0..n_paths {
            let mut acc_values: Vec<f64> = account_sims.iter().map(|s| s.current).collect();
            let mut cat_baselines: Vec<f64> = category_sims.iter().map(|s| s.baseline).collect();
            let mut cash = cash_start;

            apply_due(&mut acc_values, &account_sims, 0);
            for (i, sim) in category_sims.iter().enumerate() {
                if let Some(&(_, val)) = sim.step_changes.iter().find(|&&(idx, _)| idx == 0) {
                    cat_baselines[i] = val;
                }
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
                        AccountProjection::Stochastic {
                            monthly_log_return,
                            monthly_vol,
                        } => {
                            if let Some(&(_, val)) =
                                sim.step_changes.iter().find(|&&(idx, _)| idx == m)
                            {
                                acc_values[i] = val;
                            } else {
                                let noise = if monthly_vol > 0.0 {
                                    Normal::new(0.0, monthly_vol).unwrap().sample(&mut rng)
                                } else {
                                    0.0
                                };
                                acc_values[i] *= (monthly_log_return + noise).exp();
                            }
                            let one_off: f64 = sim
                                .one_offs
                                .iter()
                                .filter(|&&(idx, _)| idx == m)
                                .map(|&(_, d)| d)
                                .sum();
                            acc_values[i] += one_off;
                        }
                        AccountProjection::LinearPaydown {
                            monthly_delta,
                            monthly_vol_abs,
                        } => {
                            if let Some(&(_, val)) =
                                sim.step_changes.iter().find(|&&(idx, _)| idx == m)
                            {
                                acc_values[i] = val;
                            } else {
                                let noise = if monthly_vol_abs > 0.0 {
                                    Normal::new(0.0, monthly_vol_abs).unwrap().sample(&mut rng)
                                } else {
                                    0.0
                                };
                                acc_values[i] += monthly_delta + noise;
                            }
                            let one_off: f64 = sim
                                .one_offs
                                .iter()
                                .filter(|&&(idx, _)| idx == m)
                                .map(|&(_, d)| d)
                                .sum();
                            acc_values[i] += one_off;
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
                        sim.step_changes.iter().find(|&&(idx, _)| idx == m)
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
                        cat_baselines[i] *= sim.monthly_log_return.exp();
                    }
                    let one_off: f64 = sim
                        .one_offs
                        .iter()
                        .filter(|&&(idx, _)| idx == m)
                        .map(|&(_, d)| d)
                        .sum();
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
                // Servicing the debt is real money leaving. Net worth therefore falls by
                // exactly the interest each month: the principal moves from cash to the
                // liability and nets out, the interest simply goes.
                cash += net_flow - repayments;

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
    /// `value *= exp(monthly_log_return + noise)` each month. Assets and investments, whose
    /// rate is fitted as a compounding return in the first place.
    Stochastic {
        monthly_log_return: f64,
        monthly_vol: f64,
    },
    /// `value += monthly_delta + noise` each month, stopping at zero. A liability without a
    /// repayment schedule of its own: its rate is fitted as a straight line, so it is
    /// projected as one, and a debt being paid down reaches zero and stays there rather
    /// than shrinking by a fixed percentage forever.
    LinearPaydown {
        /// Signed, native minor units. Positive moves a (negative) debt toward zero.
        monthly_delta: f64,
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
    /// `(month_index, new_value)`, native currency — a known future revaluation.
    step_changes: Vec<(i64, f64)>,
    /// `(month_index, delta)`, native currency — a one-time contribution/withdrawal.
    one_offs: Vec<(i64, f64)>,
}

struct CategorySim {
    is_income: bool,
    /// Base-currency major units (dollars) — the current fitted monthly run-rate.
    baseline: f64,
    monthly_log_return: f64,
    /// Fraction of the (then-current) baseline, not yet scaled to an absolute $ stdev —
    /// scaled per-month so noise grows with the baseline over the horizon.
    monthly_vol_fraction: f64,
    /// `(month_index, new_baseline)`, base-currency dollars — a promotion/pay change.
    step_changes: Vec<(i64, f64)>,
    /// `(month_index, delta)`, base-currency dollars — a one-time bonus/expense.
    one_offs: Vec<(i64, f64)>,
}

/// `(month_index, value)` pairs an event contributes: a step-change's new value, or a
/// one-off's delta.
type MonthlyDeltas = Vec<(i64, f64)>;

struct MonthSamples {
    assets: Vec<f64>,
    liabilities: Vec<f64>,
    net_worth: Vec<f64>,
}

fn apply_due(values: &mut [f64], sims: &[AccountSim], month: i64) {
    for (i, sim) in sims.iter().enumerate() {
        if let Some(&(_, val)) = sim.step_changes.iter().find(|&&(idx, _)| idx == month) {
            values[i] = val;
        }
        let one_off: f64 = sim
            .one_offs
            .iter()
            .filter(|&&(idx, _)| idx == month)
            .map(|&(_, d)| d)
            .sum();
        values[i] += one_off;
    }
}

/// This account's forecast_events, split into step-changes/one-offs due within the
/// simulation window, converted to `(month_index, value/delta)` in native minor units.
fn account_events(
    events: &[ForecastEvent],
    account_id: i64,
    today: NaiveDate,
    horizon: i64,
) -> (MonthlyDeltas, MonthlyDeltas) {
    let mut step_changes = Vec::new();
    let mut one_offs = Vec::new();
    for e in events {
        if e.target_type != ForecastTargetType::Account || e.target_id != account_id {
            continue;
        }
        let Some(idx) = month_index(today, &e.effective_date, horizon) else {
            continue;
        };
        match e.kind {
            ForecastEventKind::StepChange => step_changes.push((idx, e.amount_minor as f64)),
            ForecastEventKind::OneOffAmount => one_offs.push((idx, e.amount_minor as f64)),
        }
    }
    (step_changes, one_offs)
}

/// As [`account_events`], but for a category — `amount_minor` is interpreted in the
/// household's base reporting currency (a category has no intrinsic currency of its
/// own), converted here to base-currency major units.
fn category_events(
    events: &[ForecastEvent],
    category_id: i64,
    today: NaiveDate,
    horizon: i64,
    fx: &Fx,
    base: &str,
) -> (MonthlyDeltas, MonthlyDeltas) {
    let mut step_changes = Vec::new();
    let mut one_offs = Vec::new();
    for e in events {
        if e.target_type != ForecastTargetType::Category || e.target_id != category_id {
            continue;
        }
        let Some(idx) = month_index(today, &e.effective_date, horizon) else {
            continue;
        };
        let major = e.amount_minor as f64 / 10f64.powi(fx.dp(base));
        match e.kind {
            ForecastEventKind::StepChange => step_changes.push((idx, major)),
            ForecastEventKind::OneOffAmount => one_offs.push((idx, major)),
        }
    }
    (step_changes, one_offs)
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

/// `effective`'s month index relative to `today` (0 = this month or earlier, clamped to
/// "apply immediately"), or `None` if it falls beyond `horizon` months out (not "not yet
/// due" — genuinely outside the requested projection window, so it should be ignored
/// rather than smeared into the last month).
fn month_index(today: NaiveDate, effective: &str, horizon: i64) -> Option<i64> {
    let effective = reports::parse_date(effective)?;
    let idx = months_between(today, effective);
    if idx > horizon {
        None
    } else {
        Some(idx.max(0))
    }
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
    tx_by_acct: &HashMap<i64, Vec<(NaiveDate, i64)>>,
    val_by_acct: &HashMap<i64, Vec<(NaiveDate, i64, String)>>,
) -> Vec<(NaiveDate, f64)> {
    let earliest = tx_by_acct
        .get(&account_id)
        .and_then(|v| v.iter().map(|(d, _)| *d).min())
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
        .map(|d| {
            let (value_minor, _) =
                reports::account_value_at(account_id, currency, d, tx_by_acct, val_by_acct);
            (d, value_minor as f64)
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
                account_kind: AccountKind::Bank,
                attribution: sure_core::Ownership::Joint,
            },
            crate::ports::SpendTransaction {
                posted_at: "2026-04-15".into(),
                amount_minor: -5_000,
                currency_code: "NZD".into(),
                category_id: Some(1),
                is_one_off: false,
                linked_transaction_id: None,
                account_kind: AccountKind::Bank,
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
        assert!(loan_terms(&AccountMetadata::Mortgage(meta), today)
            .unwrap()
            .refix
            .is_none());
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
            assert!(loan_terms(&AccountMetadata::Mortgage(meta), today)
                .unwrap()
                .refix
                .is_none());
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
        assert!(loan_terms(
            &AccountMetadata::Mortgage(sure_core::MortgageMeta {
                term_months: Some(999_999),
                interest_rate_bps: Some(-500),
                ..mortgage_meta()
            }),
            d("2026-07-01"),
        )
        .is_some_and(|t| t.term_months == MAX_TERM_MONTHS && t.rate_bps == 0));
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

    #[test]
    fn month_index_clamps_the_past_and_excludes_beyond_horizon() {
        let today = d("2026-07-01");
        assert_eq!(month_index(today, "2026-01-01", 12), Some(0));
        assert_eq!(month_index(today, "2026-10-01", 12), Some(3));
        assert_eq!(month_index(today, "2028-01-01", 12), None);
    }

    #[test]
    fn add_months_clamps_day_of_month() {
        // Jan 31 + 1 month → Feb 28 (2026 isn't a leap year), not an invalid Feb 31.
        assert_eq!(add_months(d("2026-01-31"), 1), d("2026-02-28"));
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
        // The ceiling holds however absurd the input, and the exponent it implies stays
        // nowhere near the ~745 at which `exp()` saturates, even compounded over the longest
        // horizon at several standard deviations.
        let ceiling = annual_vol_to_monthly_sd(MAX_VOLATILITY_BPS);
        for bps in [MAX_VOLATILITY_BPS + 1, 1_000_000_000_000_00, i64::MAX] {
            assert_eq!(annual_vol_to_monthly_sd(bps), ceiling);
        }
        assert!(
            (ceiling * 10.0 * MAX_HORIZON_MONTHS as f64)
                .exp()
                .is_finite(),
            "ten sigma every month for the whole horizon must still be finite"
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
            async fn create_event(&self, _input: SaveForecastEvent) -> AppResult<ForecastEvent> {
                unreachable!()
            }
            async fn delete_event(&self, _id: i64) -> AppResult<()> {
                unreachable!()
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
                });
                spend.push(SpendTransaction {
                    posted_at: date.to_string(),
                    amount_minor: 400_000 + i * 1_000,
                    currency_code: "NZD".into(),
                    category_id: Some(10),
                    is_one_off: false,
                    linked_transaction_id: None,
                    account_kind: AK::Bank,
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
                });
                spend.push(SpendTransaction {
                    posted_at: date.to_string(),
                    amount_minor: 500_000,
                    currency_code: "NZD".into(),
                    category_id: Some(10),
                    is_one_off: false,
                    linked_transaction_id: None,
                    account_kind: AK::Bank,
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
                    target_type: ForecastTargetType::Category,
                    target_id: 10,
                    kind: ForecastEventKind::StepChange,
                    effective_date: today.to_string(),
                    amount_minor: 1_000_000, // double the ~$5,000/mo baseline
                    label: "Promotion".into(),
                    created_at: "2026-07-01T00:00:00Z".into(),
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
                });
                spend.push(SpendTransaction {
                    posted_at: date.to_string(),
                    amount_minor: amount,
                    currency_code: "NZD".into(),
                    category_id: Some(10),
                    is_one_off: false,
                    linked_transaction_id: None,
                    account_kind: AK::Bank,
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
