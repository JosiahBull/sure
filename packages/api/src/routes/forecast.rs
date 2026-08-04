//! Forecast HTTP handlers. Assumption resolution (override → cron → historical default)
//! and the Monte Carlo projection both live in `sure_app::forecast`; these handlers
//! extract query params, forward to it, and convert the plain result into the
//! wire-facing (`ToSchema`) response, per the same DTO-twin rationale as
//! `routes::reports`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use sure_core::{ForecastAssumption, ForecastTargetType};
pub use sure_core::{ForecastEvent, SaveForecastAssumption, SaveForecastEvent};

const FORECAST_ASSUMPTIONS: &str = "forecast.assumptions";
const FORECAST_SIMULATE: &str = "forecast.simulate";
const FORECAST_UPSERT_ASSUMPTION: &str = "forecast.upsert_assumption";
const FORECAST_CLEAR_ASSUMPTION: &str = "forecast.clear_assumption";
const FORECAST_LIST_EVENTS: &str = "forecast.list_events";
const FORECAST_CREATE_EVENT: &str = "forecast.create_event";
const FORECAST_DELETE_EVENT: &str = "forecast.delete_event";

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
    /// How many months forward to project (1-60). Defaults to 12.
    pub horizon_months: Option<i64>,
    /// Monte Carlo path count (100-5000, more = smoother percentiles, slower).
    /// Defaults to 2000.
    pub simulations: Option<i64>,
    /// Report currency; defaults to the configured base currency.
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
}

impl From<sure_app::forecast::ForecastResult> for ForecastResult {
    fn from(r: sure_app::forecast::ForecastResult) -> Self {
        ForecastResult {
            currency: r.currency,
            months: r.months.into_iter().map(Into::into).collect(),
            assumptions: r.assumptions.into_iter().map(Into::into).collect(),
            unconverted: r.unconverted,
            rates_as_of: r.rates_as_of,
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
#[utoipa::path(get, path = "/api/forecast", tag = "forecast", params(ForecastQuery),
    responses((status = 200, body = ForecastResult)))]
#[tracing::instrument(
    name = FORECAST_SIMULATE,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn simulate(
    State(st): State<AppState>,
    Query(q): Query<ForecastQuery>,
) -> AppResult<Json<ForecastResult>> {
    Ok(Json(st.forecast.simulate(&(&q).into()).await?.into()))
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

/// Record a known future step-change (a promotion, a fixed appreciation rate) or
/// one-off (a planned bonus, a lump-sum contribution).
#[utoipa::path(post, path = "/api/forecast/events", tag = "forecast",
    request_body = SaveForecastEvent,
    responses((status = 201, body = ForecastEvent), (status = 422, body = crate::error::ErrorBody)))]
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

#[utoipa::path(delete, path = "/api/forecast/events/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
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
        .route("/forecast/events/{id}", axum::routing::delete(delete_event))
}
