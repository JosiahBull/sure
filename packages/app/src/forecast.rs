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
    ForecastAssumption, ForecastEvent, ForecastEventKind, ForecastTargetType, Interval,
};

use crate::fx::Fx;
use crate::ports::{AccountRepo, Clock, CronRepo, ForecastRepo, FxRatesRepo, ReportRepo};
use crate::reports;

/// Below this many days of valuation/transaction history, a derived default would be
/// noise rather than signal — flagged `InsufficientHistory` instead of guessing.
const MIN_HISTORY_DAYS: i64 = 60;
/// How many trailing complete months of category totals feed the trend regression.
const CATEGORY_TREND_MONTHS: i64 = 24;
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
    pub source: AssumptionSource,
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

    /// Every account/category's resolved forecast assumption.
    pub async fn resolved_assumptions(&self) -> AppResult<Vec<ResolvedAssumption>> {
        let overrides = self.forecast.list_assumptions().await?;
        let mut by_target: HashMap<(ForecastTargetType, i64), ForecastAssumption> = HashMap::new();
        for o in overrides {
            by_target.insert((o.target_type, o.target_id), o);
        }

        let today = self.clock.today();
        let mut out = self.resolve_account_assumptions(today, &by_target).await?;
        out.extend(self.resolve_category_assumptions(today, &by_target).await?);
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

            if amortization_terms(&a.metadata).is_some() {
                out.push(ResolvedAssumption {
                    target_type: ForecastTargetType::Account,
                    target_id: a.id,
                    label: a.name.clone(),
                    annual_growth_bps: 0,
                    annual_volatility_bps: 0,
                    dividend_yield_bps: None,
                    baseline_minor: None,
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
                source,
            });
        }
        Ok(out)
    }

    async fn resolve_category_assumptions(
        &self,
        today: NaiveDate,
        overrides: &HashMap<(ForecastTargetType, i64), ForecastAssumption>,
    ) -> AppResult<Vec<ResolvedAssumption>> {
        let base = self.base_currency(None).await?;
        let fx = Fx::load(self.fx.as_ref(), base).await?;
        let cats = reports::Categories::load(self.reports.as_ref()).await?;
        let from = today - chrono::Duration::days(31 * (CATEGORY_TREND_MONTHS + 1));
        let spend = reports::load_spend(self.reports.as_ref(), &cats, from, today, false).await?;

        let mut out = Vec::new();
        for (id, kind) in cats.top_level_kinds() {
            match kind {
                // Transfer categories have no cash-flow assumption to make.
                CategoryKind::Transfer => continue,
                CategoryKind::Income | CategoryKind::Expense => {}
            }

            let totals = category_monthly_totals(&spend, &cats, id, today, &fx);
            let derived3 = if totals.len() >= 3 {
                linear_trend_and_vol(&totals)
            } else {
                None
            };
            let derived = derived3.map(|(g, v, _)| (g, v));
            let baseline_minor = derived3.map(|(_, _, fitted)| fx.base_minor(fitted));

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
    pub async fn simulate(&self, params: &SimulationParams) -> AppResult<ForecastResult> {
        let today = self.clock.today();
        let horizon = params
            .horizon_months
            .clamp(MIN_HORIZON_MONTHS, MAX_HORIZON_MONTHS);
        let n_paths = params.simulations.clamp(MIN_SIMULATIONS, MAX_SIMULATIONS) as usize;

        let base = self.base_currency(params.currency.as_deref()).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;

        let assumptions = self.resolved_assumptions().await?;
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

            let projection = if let Some((principal, rate_bps, term_months, start)) =
                amortization_terms(&a.metadata)
            {
                AccountProjection::Deterministic {
                    principal,
                    rate_bps,
                    term_months,
                    elapsed_at_today: months_between(start, today),
                }
            } else {
                let resolved = by_target.get(&(ForecastTargetType::Account, a.id));
                let annual_growth = resolved.map(|r| r.annual_growth_bps).unwrap_or(0);
                let annual_vol = resolved.map(|r| r.annual_volatility_bps).unwrap_or(0);
                AccountProjection::Stochastic {
                    monthly_log_return: annual_rate_to_monthly_log_return(annual_growth),
                    monthly_vol: (annual_vol as f64 / 10_000.0).max(0.0) / 12f64.sqrt(),
                }
            };

            // Events only apply to non-deterministic accounts for now — a fully
            // amortising mortgage/loan projects from its own terms alone.
            let (step_changes, one_offs) =
                if matches!(projection, AccountProjection::Stochastic { .. }) {
                    account_events(&events, a.id, today, horizon)
                } else {
                    (Vec::new(), Vec::new())
                };

            account_sims.push(AccountSim {
                currency_code: a.currency_code.clone(),
                current,
                projection,
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
            let baseline = fx.to_base_major(baseline_minor, &base);
            let (step_changes, one_offs) =
                category_events(&events, a.target_id, today, horizon, &fx, &base);
            category_sims.push(CategorySim {
                is_income: source_kind_is_income(self, a.target_id).await?,
                baseline,
                monthly_log_return: annual_rate_to_monthly_log_return(a.annual_growth_bps),
                monthly_vol_fraction: (a.annual_volatility_bps as f64 / 10_000.0).max(0.0)
                    / 12f64.sqrt(),
                step_changes,
                one_offs,
            });
        }

        let cash_start: f64 = accounts
            .iter()
            .filter(|a| a.kind.class() == AccountClass::Cash)
            .map(|a| {
                let (v, _) = reports::account_value_at(
                    a.id,
                    &a.currency_code,
                    today,
                    &tx_by_acct,
                    &val_by_acct,
                );
                fx.to_base_major(v, &a.currency_code)
            })
            .sum();

        let seed = params.seed.unwrap_or_else(rand::random);
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

            for m in 1..=horizon {
                for (i, sim) in account_sims.iter().enumerate() {
                    match sim.projection {
                        AccountProjection::Deterministic {
                            principal,
                            rate_bps,
                            term_months,
                            elapsed_at_today,
                        } => {
                            let remaining = amortized_remaining(
                                principal,
                                rate_bps,
                                term_months,
                                elapsed_at_today + m,
                            );
                            acc_values[i] = -remaining;
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
                    }
                }

                let mut net_flow = 0.0;
                for (i, sim) in category_sims.iter().enumerate() {
                    if let Some(&(_, new_baseline)) =
                        sim.step_changes.iter().find(|&&(idx, _)| idx == m)
                    {
                        cat_baselines[i] = new_baseline;
                    } else {
                        let sd = cat_baselines[i].abs() * sim.monthly_vol_fraction;
                        let noise = if sd > 0.0 {
                            Normal::new(0.0, sd).unwrap().sample(&mut rng)
                        } else {
                            0.0
                        };
                        cat_baselines[i] =
                            (cat_baselines[i] * sim.monthly_log_return.exp() + noise).max(0.0);
                    }
                    let one_off: f64 = sim
                        .one_offs
                        .iter()
                        .filter(|&&(idx, _)| idx == m)
                        .map(|&(_, d)| d)
                        .sum();
                    let contribution = cat_baselines[i] + one_off;
                    net_flow += if sim.is_income {
                        contribution
                    } else {
                        -contribution
                    };
                }
                cash += net_flow;

                let mut assets = 0.0;
                let mut liabilities = 0.0;
                for (i, sim) in account_sims.iter().enumerate() {
                    let base_val = to_base(&fx, acc_values[i], &sim.currency_code);
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
    /// A mortgage/loan with a complete amortisation schedule: an exact function of
    /// elapsed months, no sampling.
    Deterministic {
        principal: f64,
        rate_bps: i64,
        term_months: i64,
        elapsed_at_today: i64,
    },
    /// `value *= exp(monthly_log_return + noise)` each month.
    Stochastic {
        monthly_log_return: f64,
        monthly_vol: f64,
    },
}

struct AccountSim {
    currency_code: String,
    current: f64,
    projection: AccountProjection,
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

/// Convert a raw (float) minor-unit amount in `ccy` into base-currency major units —
/// `Fx::to_base_major` without requiring an `i64`, since simulated account values are
/// carried as floats between months.
fn to_base(fx: &Fx, minor: f64, ccy: &str) -> f64 {
    (minor / 10f64.powi(fx.dp(ccy))) * fx.factor(ccy)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn band_from_samples(samples: &mut [f64], fx: &Fx) -> Band {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
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

/// The amortisation terms a mortgage/loan needs to project its exact remaining balance,
/// or `None` if any field is unset (or unparseable) — the account then falls back to a
/// trend/rate like a generic asset.
fn amortization_terms(metadata: &AccountMetadata) -> Option<(f64, i64, i64, NaiveDate)> {
    let (original, rate, term, start) = match metadata {
        AccountMetadata::Mortgage(m) => (
            m.original_amount_minor,
            m.interest_rate_bps,
            m.term_months,
            m.start_date.as_deref(),
        ),
        AccountMetadata::Loan(l) => (
            l.original_amount_minor,
            l.interest_rate_bps,
            l.term_months,
            l.start_date.as_deref(),
        ),
        // Every other profile has no amortisation schedule at all: the caller falls back
        // to a trend/rate like a generic asset for these.
        AccountMetadata::Depository(_)
        | AccountMetadata::Property(_)
        | AccountMetadata::Vehicle(_)
        | AccountMetadata::Shares(_)
        | AccountMetadata::Brokerage(_)
        | AccountMetadata::Crypto(_)
        | AccountMetadata::Generic(_) => return None,
    };
    let start_date = reports::parse_date(start?)?;
    Some((original? as f64, rate?, term?, start_date))
}

/// Standard fixed-payment amortisation: the remaining principal after `n` monthly
/// payments (clamped to the loan's term), recomputed from the loan's own terms each
/// time rather than tracked as running state.
fn amortized_remaining(principal: f64, annual_rate_bps: i64, term_months: i64, n: i64) -> f64 {
    if term_months <= 0 || principal <= 0.0 {
        return 0.0;
    }
    let n = n.clamp(0, term_months);
    let r = (annual_rate_bps as f64 / 10_000.0) / 12.0;
    if r.abs() < 1e-9 {
        return (principal * (1.0 - n as f64 / term_months as f64)).max(0.0);
    }
    let growth_term = (1.0 + r).powi(term_months as i32);
    let payment = principal * r * growth_term / (growth_term - 1.0);
    let growth_n = (1.0 + r).powi(n as i32);
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
        *totals.entry(key).or_default() += fx.to_base_major(t.amount_minor, &t.currency_code).abs();
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
            },
            crate::ports::SpendTransaction {
                posted_at: "2026-04-15".into(),
                amount_minor: -5_000,
                currency_code: "NZD".into(),
                category_id: Some(1),
                is_one_off: false,
                linked_transaction_id: None,
                account_kind: AccountKind::Bank,
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

    #[test]
    fn has_amortization_schedule_requires_every_field() {
        use sure_core::MortgageMeta;
        let complete = MortgageMeta {
            original_amount_minor: Some(500_000_00),
            interest_rate_bps: Some(549),
            term_months: Some(360),
            start_date: Some("2024-01-01".into()),
            ..Default::default()
        };
        assert!(amortization_terms(&AccountMetadata::Mortgage(complete)).is_some());

        let partial = MortgageMeta {
            original_amount_minor: Some(500_000_00),
            ..Default::default()
        };
        assert!(amortization_terms(&AccountMetadata::Mortgage(partial)).is_none());
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

    // ---- simulate() integration (fake ports) ---------------------------------------

    mod sim {
        use super::*;
        use async_trait::async_trait;
        use sure_core::{
            Account, AccountKind as AK, AccountMetadata as AM, Cron, CronRun, CronRunResult,
            GenericMeta, SaveAccount, SaveCron, SaveForecastAssumption, SaveForecastEvent,
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
            async fn transactions(&self) -> AppResult<Vec<LedgerTx>> {
                Ok(self.transactions.clone())
            }
            async fn valuations(&self) -> AppResult<Vec<LedgerValuation>> {
                Ok(self.valuations.clone())
            }
            async fn categories(&self) -> AppResult<Vec<ReportCategory>> {
                Ok(self.categories.clone())
            }
            async fn spend_transactions(&self) -> AppResult<Vec<SpendTransaction>> {
                Ok(self.spend_transactions.clone())
            }
            async fn earliest_transaction_date(&self) -> AppResult<Option<String>> {
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
        }
        #[async_trait]
        impl ForecastRepo for FakeForecast {
            async fn list_assumptions(&self) -> AppResult<Vec<ForecastAssumption>> {
                Ok(Vec::new())
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
                })
                .collect();
            ForecastService::new(
                Arc::new(FakeForecast { events }),
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
    }
}
