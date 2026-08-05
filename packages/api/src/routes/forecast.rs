//! Forecast HTTP handlers. Assumption resolution (override → cron → historical default)
//! and the Monte Carlo projection both live in `sure_app::forecast`; these handlers
//! extract query params, forward to it, and convert the plain result into the
//! wire-facing (`ToSchema`) response, per the same DTO-twin rationale as
//! `routes::reports`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::compute;
use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{
    EffectTarget, ForecastEvent, ForecastEventEffect, ForecastEventRelation, LifeEffectKind,
    LifeEffectSpec, LifeEventKind, RelationKind, SaveForecastAssumption, SaveForecastEvent,
    SaveForecastEventRelation, StepAmount,
};
use sure_core::{ForecastAssumption, ForecastTargetType};

const FORECAST_ASSUMPTIONS: &str = "forecast.assumptions";
const FORECAST_SIMULATE: &str = "forecast.simulate";
const FORECAST_UPSERT_ASSUMPTION: &str = "forecast.upsert_assumption";
const FORECAST_CLEAR_ASSUMPTION: &str = "forecast.clear_assumption";
const FORECAST_LIST_EVENTS: &str = "forecast.list_events";
const FORECAST_CREATE_EVENT: &str = "forecast.create_event";
const FORECAST_DELETE_EVENT: &str = "forecast.delete_event";
const FORECAST_GET_EVENT: &str = "forecast.get_event";
const FORECAST_UPDATE_EVENT: &str = "forecast.update_event";

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssumptionSource {
    /// An explicit `forecast_assumptions` override is set for this target.
    Override,
    /// No override, but an enabled appreciation/depreciation/interest cron already
    /// configures this account's rate.
    Cron,
    /// Computed from this target's own transaction/valuation history.
    Derived,
    /// A mortgage/loan with a complete amortisation schedule — projected exactly.
    Deterministic,
    /// Not enough history to derive a default; mean/volatility are 0 rather than a
    /// guess — set an override to use this target in a forecast.
    InsufficientHistory,
    /// This account receives payroll contributions, so its own measured growth rate was discarded:
    /// a balance that rose while money flowed into it cannot tell market growth and contributions
    /// apart, and using both would count them twice. Growth comes from an override, else the
    /// long-run rate, else flat — see `warnings`.
    ContributionDriven,
    /// This category's cash flow comes from per-person income streams rather than its own fitted
    /// trend. `baseline_minor` is then the *residual* — the part of the category the streams do
    /// not explain — so a non-zero one means some income here is still un-modelled.
    ModelledFromIncome,
}

impl From<sure_app::forecast::AssumptionSource> for AssumptionSource {
    fn from(s: sure_app::forecast::AssumptionSource) -> Self {
        use sure_app::forecast::AssumptionSource as S;
        match s {
            S::Override => AssumptionSource::Override,
            S::Cron => AssumptionSource::Cron,
            S::Derived => AssumptionSource::Derived,
            S::Deterministic => AssumptionSource::Deterministic,
            S::InsufficientHistory => AssumptionSource::InsufficientHistory,
            S::ModelledFromIncome => AssumptionSource::ModelledFromIncome,
            S::ContributionDriven => AssumptionSource::ContributionDriven,
        }
    }
}

/// The repayment schedule a deterministic mortgage/loan is projected from, at the assumed
/// refix rate (not the mean of the simulated draws — the payment is convex in the rate).
#[derive(Debug, Serialize, ToSchema)]
pub struct LoanScheduleSummary {
    /// Minor units of the account's own `currency_code`.
    pub monthly_payment_minor: i64,
    pub current_rate_bps: i64,
    pub remaining_term_months: i64,
    /// Months from today until the fixed rate rolls off; absent if none is modelled.
    pub refix_in_months: Option<i64>,
    pub refix_rate_bps: Option<i64>,
    /// One standard deviation of uncertainty on the refix rate, in basis points.
    pub refix_rate_uncertainty_bps: Option<i64>,
}

impl From<sure_app::forecast::LoanScheduleSummary> for LoanScheduleSummary {
    fn from(s: sure_app::forecast::LoanScheduleSummary) -> Self {
        LoanScheduleSummary {
            monthly_payment_minor: s.monthly_payment_minor,
            current_rate_bps: s.current_rate_bps,
            remaining_term_months: s.remaining_term_months,
            refix_in_months: s.refix_in_months,
            refix_rate_bps: s.refix_rate_bps,
            refix_rate_uncertainty_bps: s.refix_rate_uncertainty_bps,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedAssumption {
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    pub label: String,
    pub annual_growth_bps: i64,
    pub annual_volatility_bps: i64,
    /// The annual rate `annual_growth_bps` decays toward beyond the five years it was fitted
    /// over, in basis points. Only applied when `source` is `derived` — an override or a
    /// cron-configured rate is something the user asserted, and an assertion is not decayed.
    pub long_run_growth_bps: i64,
    /// The fund's annual fee in basis points, deducted from this account's growth every month.
    /// Absent means "not modelled" rather than zero — a fund charging nothing is a claim worth
    /// making on purpose, and assuming it is flattering.
    pub annual_fee_bps: Option<i64>,
    /// A flat annual membership fee, in the account's own minor units.
    pub annual_fixed_fee_minor: Option<i64>,
    /// Only set for Investment-class accounts (brokerage/shares).
    pub dividend_yield_bps: Option<i64>,
    /// Only set for categories: the current fitted monthly run-rate the simulation
    /// grows forward from.
    pub baseline_minor: Option<i64>,
    /// Only set for a mortgage/loan projected from an amortisation schedule.
    pub schedule: Option<LoanScheduleSummary>,
    /// The account's own currency, for formatting `schedule`. Absent for a category.
    pub currency_code: Option<String>,
    pub source: AssumptionSource,
}

impl From<sure_app::forecast::ResolvedAssumption> for ResolvedAssumption {
    fn from(r: sure_app::forecast::ResolvedAssumption) -> Self {
        ResolvedAssumption {
            target_type: r.target_type,
            target_id: r.target_id,
            label: r.label,
            annual_growth_bps: r.annual_growth_bps,
            annual_volatility_bps: r.annual_volatility_bps,
            long_run_growth_bps: r.long_run_growth_bps,
            annual_fee_bps: r.annual_fee_bps,
            annual_fixed_fee_minor: r.annual_fixed_fee_minor,
            dividend_yield_bps: r.dividend_yield_bps,
            baseline_minor: r.baseline_minor,
            schedule: r.schedule.map(Into::into),
            currency_code: r.currency_code,
            source: r.source.into(),
        }
    }
}

// ---- simulation ------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ForecastQuery {
    /// How many months forward to project (1-360). Defaults to 12. A value past the ceiling is
    /// clamped rather than refused; `ForecastResult::horizon_months` reports what was run.
    pub horizon_months: Option<i64>,
    /// Monte Carlo path count (100-5000, more = smoother percentiles, slower).
    /// Defaults to 2000. Long horizons are additionally capped by a path-month budget, so a
    /// 30-year projection runs 2000 paths however many were asked for —
    /// `ForecastResult::simulations` reports what was run.
    pub simulations: Option<i64>,
    /// Report currency; defaults to the configured base currency. An unknown code is a 400
    /// rather than a projection at parity — see `sure_app::forecast`'s `currency_and_fx`.
    pub currency: Option<String>,
    /// Fixed RNG seed for reproducible output; omit for a fresh random draw each call.
    pub seed: Option<u64>,
}

impl From<&ForecastQuery> for sure_app::forecast::SimulationParams {
    fn from(q: &ForecastQuery) -> Self {
        let defaults = sure_app::forecast::SimulationParams::default();
        sure_app::forecast::SimulationParams {
            horizon_months: q.horizon_months.unwrap_or(defaults.horizon_months),
            simulations: q.simulations.unwrap_or(defaults.simulations),
            currency: q.currency.clone(),
            seed: q.seed,
        }
    }
}

#[derive(Debug, Serialize, ToSchema, Default)]
pub struct Band {
    pub p10_minor: i64,
    pub p25_minor: i64,
    pub median_minor: i64,
    pub mean_minor: i64,
    pub p75_minor: i64,
    pub p90_minor: i64,
}

impl From<sure_app::forecast::Band> for Band {
    fn from(b: sure_app::forecast::Band) -> Self {
        Band {
            p10_minor: b.p10_minor,
            p25_minor: b.p25_minor,
            median_minor: b.median_minor,
            mean_minor: b.mean_minor,
            p75_minor: b.p75_minor,
            p90_minor: b.p90_minor,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ForecastMonth {
    pub as_of: String,
    pub net_worth: Band,
    pub assets: Band,
    pub liabilities: Band,
}

impl From<sure_app::forecast::ForecastMonth> for ForecastMonth {
    fn from(m: sure_app::forecast::ForecastMonth) -> Self {
        ForecastMonth {
            as_of: m.as_of,
            net_worth: m.net_worth.into(),
            assets: m.assets.into(),
            liabilities: m.liabilities.into(),
        }
    }
}

/// What the income streams linked to one category claim, beside what that category's own history
/// recorded. A modelled figure well above the observed one is the signature of a gross salary being
/// modelled as take-home.
#[derive(Debug, Serialize, ToSchema)]
pub struct StreamReconciliation {
    pub person_id: i64,
    pub category_id: i64,
    pub category_label: String,
    /// Monthly net the streams model as of today.
    pub modelled_net_minor: i64,
    /// The category's own fitted monthly baseline — what history saw.
    pub observed_net_minor: i64,
    /// `modelled / observed`, in basis points. Over 10 000 means the streams claim more than the
    /// category ever recorded: a wrong link or a wrong figure, not good news.
    pub coverage_bps: i64,
    /// What is left for the fitted trend once the streams are netted out.
    pub residual_minor: i64,
}

impl From<sure_app::forecast::StreamReconciliation> for StreamReconciliation {
    fn from(r: sure_app::forecast::StreamReconciliation) -> Self {
        StreamReconciliation {
            person_id: r.person_id,
            category_id: r.category_id,
            category_label: r.category_label,
            modelled_net_minor: r.modelled_net_minor,
            observed_net_minor: r.observed_net_minor,
            coverage_bps: r.coverage_bps,
            residual_minor: r.residual_minor,
        }
    }
}

/// How an event actually landed across the simulated paths — not what was typed.
///
/// Relations move timing, so the configured `expected_on ± spread` and the realised distribution
/// genuinely differ. The chart draws this; the editor shows the input. Drawing the input would be a
/// lie about precisely the thing the chart exists to show.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventOutcome {
    pub event_id: i64,
    pub label: String,
    pub kind: LifeEventKind,
    /// Whose event it is, so the chart can colour its band with that person's swatch. Absent for a
    /// household event.
    pub person_id: Option<i64>,
    /// What was configured, for comparison with `occurrence_rate_bps` below.
    pub probability_bps: i64,
    /// Paths it occurred on. Differs from `probability_bps` exactly when an `only_if` bound.
    pub occurrence_rate_bps: i64,
    /// …of which also landed inside the horizon.
    pub in_window_rate_bps: i64,
    /// Realised timing as month offsets from today; `null` if it never occurred. Taken over *all*
    /// occurring paths, so a p90 beyond the chart says so rather than being pulled back to the edge.
    pub month_p10: Option<i64>,
    pub month_median: Option<i64>,
    pub month_p90: Option<i64>,
    pub date_p10: Option<String>,
    pub date_median: Option<String>,
    pub date_p90: Option<String>,
    /// Of occurring paths, how many had the date moved by an ordering constraint. The honesty
    /// signal: "your ±2y window was pushed by 'after the promotion' in 34% of runs."
    pub constrained_rate_bps: i64,
    /// Of occurring paths, how many sampled a month at or before today — so "your expected date is
    /// in the past" is visible rather than inferred.
    pub clamped_early_rate_bps: i64,
    /// The p90 ran past the horizon, so the chart should draw an open end.
    pub truncated: bool,
}

impl From<sure_app::forecast::EventOutcome> for EventOutcome {
    fn from(o: sure_app::forecast::EventOutcome) -> Self {
        EventOutcome {
            event_id: o.event_id,
            label: o.label,
            kind: o.kind,
            person_id: o.person_id,
            probability_bps: o.probability_bps,
            occurrence_rate_bps: o.occurrence_rate_bps,
            in_window_rate_bps: o.in_window_rate_bps,
            month_p10: o.month_p10,
            month_median: o.month_median,
            month_p90: o.month_p90,
            date_p10: o.date_p10,
            date_median: o.date_median,
            date_p90: o.date_p90,
            constrained_rate_bps: o.constrained_rate_bps,
            clamped_early_rate_bps: o.clamped_early_rate_bps,
            truncated: o.truncated,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ForecastResult {
    pub currency: String,
    pub months: Vec<ForecastMonth>,
    /// The resolved assumptions this projection actually used, for transparency.
    pub assumptions: Vec<ResolvedAssumption>,
    /// Currency codes left out of every band above, for want of an exchange rate to
    /// `currency`. Their accounts are not projected at all rather than projected at parity,
    /// so a non-empty list means these figures describe part of the household.
    pub unconverted: Vec<String>,
    /// Newest date across the exchange rates used (ISO-8601), `null` if none are on record.
    pub rates_as_of: Option<String>,
    /// The horizon actually projected, after clamping. Equal to `months.length`.
    pub horizon_months: i64,
    /// The Monte Carlo path count actually run, after the path-month budget. Asking for 5000
    /// paths over 360 months yields 2000, and this is how a caller can tell.
    pub simulations: i64,
    /// Household net income landing in each projected month. Same length as `months`.
    ///
    /// A band rather than a figure because it will not stay deterministic: once a change can pause
    /// or step someone's pay on some paths and not others, this is where that spread appears.
    pub income_net: Vec<Band>,
    /// Per linked income category, what the streams claim against what history recorded. Reported
    /// beside the projection rather than folded into it — a diagnostic that silently changes the
    /// thing it diagnoses stops being one.
    /// How each event landed across the paths. What the chart draws.
    pub events: Vec<EventOutcome>,
    pub reconciliations: Vec<StreamReconciliation>,
    /// Figures the projection is standing in for, and places where linking something changed what an
    /// account's numbers mean. Prose, because each needs to say what to do about it.
    pub warnings: Vec<String>,
    /// Income streams left out of the projection, and why. A figure the user can see is incomplete
    /// beats one they cannot.
    pub unmodelled_streams: Vec<String>,
    /// Per month, the fraction of simulated paths whose pooled cash balance was negative, in
    /// basis points. Same length as `months`.
    ///
    /// A band around net worth cannot answer "could we actually afford this": a path that ends
    /// rich having gone thousands overdrawn in year three looks identical to one that never
    /// did. This counts that directly.
    pub negative_cash_rate_bps: Vec<i64>,
}

impl From<sure_app::forecast::ForecastResult> for ForecastResult {
    fn from(r: sure_app::forecast::ForecastResult) -> Self {
        ForecastResult {
            currency: r.currency,
            months: r.months.into_iter().map(Into::into).collect(),
            assumptions: r.assumptions.into_iter().map(Into::into).collect(),
            unconverted: r.unconverted,
            rates_as_of: r.rates_as_of,
            horizon_months: r.horizon_months,
            simulations: r.simulations,
            income_net: r.income_net.into_iter().map(Into::into).collect(),
            events: r.events.into_iter().map(Into::into).collect(),
            reconciliations: r.reconciliations.into_iter().map(Into::into).collect(),
            warnings: r.warnings,
            unmodelled_streams: r.unmodelled_streams,
            negative_cash_rate_bps: r.negative_cash_rate_bps,
        }
    }
}

/// Every asset/investment/liability account and top-level income/expense category's
/// resolved forecast assumption: an override if set, else an existing cron's rate, else
/// a value derived from history (or a deterministic amortisation schedule for a
/// mortgage/loan with complete metadata).
#[utoipa::path(get, path = "/api/forecast/assumptions", tag = "forecast",
    responses((status = 200, body = [ResolvedAssumption])))]
#[tracing::instrument(
    name = FORECAST_ASSUMPTIONS,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_assumptions(
    State(st): State<AppState>,
) -> AppResult<Json<Vec<ResolvedAssumption>>> {
    Ok(Json(
        st.forecast
            .resolved_assumptions()
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// A Monte Carlo net-worth/cash-flow projection: `simulations` independent monthly
/// paths out to `horizon_months`, aggregated into percentile bands (P10/P25/median/
/// mean/P75/P90) per month, plus the resolved assumptions actually used.
// Kept out of the doc comment (utoipa publishes that verbatim as the endpoint description, and
// an internal scheduling detail tells an API consumer nothing):
//
// This is the most CPU-bound handler in the crate (the report aggregations are the other kind)
// — `simulations × horizon_months × accounts` random draws, with no `.await` anywhere inside
// the loop. Two consequences followed from running that inline, and both are why it is split
// here:
//
//  * the deadline was fiction. `tokio::time::timeout` (`crate::cache::timeout`) can only fire
//    at an await point *inside* the future it wraps, and there is none in the loop — so the
//    30s budget was not observed until the work had already finished, at which point a
//    complete response was thrown away and the client got a 408 for CPU already spent.
//  * the worker was held. On a four-worker box four concurrent `GET /api/forecast`s meant no
//    connections accepted, `/api/health` silent, no scheduler tick and no shutdown watcher —
//    with no external failure needed, since one SPA dashboard load fans out several report
//    calls.
//
// So: `simulate_inputs` awaits the loads on the runtime as before, then the arithmetic runs on
// the blocking pool under a `compute` slot. Not a single figure changes — the RNG is an owned
// `StdRng` seeded before the closure is built, never a thread-local — which
// `sure_app::forecast`'s `simulate_matches_the_two_step_split` pins.
#[utoipa::path(get, path = "/api/forecast", tag = "forecast", params(ForecastQuery),
    responses(
        (status = 200, body = ForecastResult),
        (status = 400, description = "unknown `currency`", body = crate::error::ErrorBody),
        // Declared, because the refusal arrives in the standard `{ error: { code, message } }`
        // envelope with code `overloaded` like every other "busy" answer — a client that
        // expected an empty 503 body would fail to read it.
        (status = 503, description = "every compute slot is busy; retry after `Retry-After`",
         body = crate::error::ErrorBody),
    ))]
#[tracing::instrument(
    name = FORECAST_SIMULATE,
    level = "debug",
    skip_all,
    fields(query = ?q),
    // No `ret`: the response is now a `Response`, which logs as opaque bytes rather than the
    // bands a reader would want. `err` still carries the interesting case.
    err(level = tracing::Level::WARN),
)]
pub async fn simulate(
    State(st): State<AppState>,
    Query(q): Query<ForecastQuery>,
) -> AppResult<Response> {
    let inputs = st.forecast.simulate_inputs(&(&q).into()).await?;

    // Acquired *after* the loads and released when the handler returns, so a slot is only held
    // while a core is actually being used. Shed rather than queued: a client waiting behind a
    // pile of full simulations has given up long before its turn arrives.
    let Some(_slot) = compute::try_slot() else {
        return Ok(compute::shed(FORECAST_SIMULATE));
    };

    // `spawn_blocking` yields a `JoinHandle`, hence the two nested results: the outer is
    // "did the task complete", the inner is the simulation's own. A `JoinError` means the
    // closure panicked — mapped to the same scrubbed 500 `CatchPanicLayer` produces for an
    // inline panic, never unwrapped (that would re-panic here, on the runtime worker awaiting
    // the join). See `crate::compute::joined`.
    let result = st
        .shutdown
        .spawn_blocking(move || sure_app::forecast::ForecastService::simulate_from(inputs))
        .await
        .map_err(|e| compute::joined(e, FORECAST_SIMULATE))??;

    Ok(Json(ForecastResult::from(result)).into_response())
}

// ---- assumption overrides ---------------------------------------------------------

/// Set (or replace) the override for a target: a field left out clears that knob back
/// to "derive from history".
#[utoipa::path(put, path = "/api/forecast/assumptions", tag = "forecast",
    request_body = SaveForecastAssumption,
    responses((status = 200, body = ForecastAssumption), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = FORECAST_UPSERT_ASSUMPTION,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn upsert_assumption(
    State(st): State<AppState>,
    Json(input): Json<SaveForecastAssumption>,
) -> AppResult<Json<ForecastAssumption>> {
    Ok(Json(st.forecast.upsert_assumption(input).await?))
}

/// Clear a target's override, if one exists — it then falls back to a cron-derived or
/// historical default.
#[utoipa::path(delete, path = "/api/forecast/assumptions/{target_type}/{target_id}", tag = "forecast",
    params(("target_type" = String, Path,), ("target_id" = i64, Path,)),
    responses((status = 204), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = FORECAST_CLEAR_ASSUMPTION,
    level = "debug",
    skip_all,
    fields(target_type = %target_type, target_id = %target_id),
    err(level = tracing::Level::WARN),
)]
pub async fn clear_assumption(
    State(st): State<AppState>,
    Path((target_type, target_id)): Path<(String, i64)>,
) -> AppResult<StatusCode> {
    let target_type: ForecastTargetType = target_type.parse().map_err(AppError::validation)?;
    st.forecast.clear_assumption(target_type, target_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- known future events -----------------------------------------------------------

/// Every known future step-change/one-off, soonest first.
#[utoipa::path(get, path = "/api/forecast/events", tag = "forecast",
    responses((status = 200, body = [ForecastEvent])))]
#[tracing::instrument(
    name = FORECAST_LIST_EVENTS,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_events(State(st): State<AppState>) -> AppResult<Json<Vec<ForecastEvent>>> {
    Ok(Json(st.forecast.list_events().await?))
}

/// Record a change to the future: a promotion, a child, a career break, a job starting or ending,
/// or a dated adjustment you are certain about.
///
/// Effects and relations travel in the same body and are saved in one transaction. A partial save
/// would leave a state the user cannot see and did not ask for, every problem across every effect
/// can then be collected into one 422, and the cycle check needs the complete proposed graph rather
/// than one edge at a time.
#[utoipa::path(post, path = "/api/forecast/events", tag = "forecast",
    request_body = SaveForecastEvent,
    responses((status = 201, body = ForecastEvent), (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = FORECAST_CREATE_EVENT,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_event(
    State(st): State<AppState>,
    Json(input): Json<SaveForecastEvent>,
) -> AppResult<(StatusCode, Json<ForecastEvent>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.forecast.create_event(input).await?),
    ))
}

/// One event, with its effects and relations.
#[utoipa::path(get, path = "/api/forecast/events/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 200, body = ForecastEvent), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = FORECAST_GET_EVENT,
    level = "debug",
    skip_all,
    fields(event_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn get_event(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ForecastEvent>> {
    Ok(Json(st.forecast.get_event(id).await?))
}

/// Replace an event, its effects and relations included — so removing one is omitting it.
#[utoipa::path(put, path = "/api/forecast/events/{id}", tag = "forecast",
    params(("id" = i64, Path,)), request_body = SaveForecastEvent,
    responses((status = 200, body = ForecastEvent), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = FORECAST_UPDATE_EVENT,
    level = "debug",
    skip_all,
    fields(event_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn update_event(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveForecastEvent>,
) -> AppResult<Json<ForecastEvent>> {
    Ok(Json(st.forecast.update_event(id, input).await?))
}

/// Remove an event.
///
/// Ordering rules pointing at it are dropped — an ordering is meaningless without the thing it
/// orders against, and refusing would trap you in a graph you could only escape by editing every
/// dependent first. But a 409 when something happens *only if* this does: that event would quietly
/// become certain, which is a change of meaning with no trace.
#[utoipa::path(delete, path = "/api/forecast/events/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = FORECAST_DELETE_EVENT,
    level = "debug",
    skip_all,
    fields(event_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn delete_event(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    st.forecast.delete_event(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route(
            "/forecast/assumptions",
            get(list_assumptions).put(upsert_assumption),
        )
        .route(
            "/forecast/assumptions/{target_type}/{target_id}",
            axum::routing::delete(clear_assumption),
        )
        .route("/forecast", get(simulate))
        .route("/forecast/events", get(list_events).post(create_event))
        .route(
            "/forecast/events/{id}",
            get(get_event).put(update_event).delete(delete_event),
        )
}
