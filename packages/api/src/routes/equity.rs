use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{
    AccountEquity, EquityEvent, EquityEventKind, EquityExercise, EquityGrant, EquityMark,
    RebuildResult, SaveExercise, SaveGrant, SaveMark, VestingStatus,
};

// OTEL span names for this module's handlers.
const EQUITY_LIST_GRANTS: &str = "equity.list_grants";
const EQUITY_CREATE_GRANT: &str = "equity.create_grant";
const EQUITY_UPDATE_GRANT: &str = "equity.update_grant";
const EQUITY_DELETE_GRANT: &str = "equity.delete_grant";
const EQUITY_LIST_EXERCISES: &str = "equity.list_exercises";
const EQUITY_CREATE_EXERCISE: &str = "equity.create_exercise";
const EQUITY_DELETE_EXERCISE: &str = "equity.delete_exercise";
const EQUITY_GRANT_VESTING: &str = "equity.grant_vesting";
const EQUITY_ACCOUNT_EQUITY: &str = "equity.account_equity";
const EQUITY_REVALUE: &str = "equity.revalue";
const EQUITY_LIST_MARKS: &str = "equity.list_marks";
const EQUITY_CREATE_MARK: &str = "equity.create_mark";
const EQUITY_DELETE_MARK: &str = "equity.delete_mark";
const EQUITY_LIST_EVENTS: &str = "equity.list_events";
const EQUITY_REBUILD: &str = "equity.rebuild_history";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct AsOfQuery {
    pub as_of: Option<String>,
}

#[utoipa::path(get, path = "/api/accounts/{id}/equity-grants", tag = "equity",
    params(("id" = i64, Path,)), responses((status = 200, body = [EquityGrant])))]
#[tracing::instrument(
    name = EQUITY_LIST_GRANTS,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_grants(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<EquityGrant>>> {
    Ok(Json(st.equity.list_grants(id).await?))
}

#[utoipa::path(post, path = "/api/accounts/{id}/equity-grants", tag = "equity",
    params(("id" = i64, Path,)), request_body = SaveGrant,
    responses((status = 201, body = EquityGrant), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_CREATE_GRANT,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_grant(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveGrant>,
) -> AppResult<(StatusCode, Json<EquityGrant>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.equity.create_grant(id, input).await?),
    ))
}

#[utoipa::path(put, path = "/api/equity-grants/{id}", tag = "equity", params(("id" = i64, Path,)),
    request_body = SaveGrant,
    responses((status = 200, body = EquityGrant), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_UPDATE_GRANT,
    level = "debug",
    skip_all,
    fields(grant_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update_grant(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveGrant>,
) -> AppResult<Json<EquityGrant>> {
    Ok(Json(st.equity.update_grant(id, input).await?))
}

#[utoipa::path(delete, path = "/api/equity-grants/{id}", tag = "equity", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_DELETE_GRANT,
    level = "debug",
    skip_all,
    fields(grant_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete_grant(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    st.equity.delete_grant(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/equity-grants/{id}/exercises", tag = "equity",
    params(("id" = i64, Path,)), responses((status = 200, body = [EquityExercise])))]
#[tracing::instrument(
    name = EQUITY_LIST_EXERCISES,
    level = "debug",
    skip_all,
    fields(grant_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_exercises(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<EquityExercise>>> {
    Ok(Json(st.equity.list_exercises(id).await?))
}

#[utoipa::path(post, path = "/api/equity-grants/{id}/exercises", tag = "equity",
    params(("id" = i64, Path,)), request_body = SaveExercise,
    responses((status = 201, body = EquityExercise), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_CREATE_EXERCISE,
    level = "debug",
    skip_all,
    fields(grant_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_exercise(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveExercise>,
) -> AppResult<(StatusCode, Json<EquityExercise>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.equity.create_exercise(id, input).await?),
    ))
}

#[utoipa::path(delete, path = "/api/equity-exercises/{id}", tag = "equity", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_DELETE_EXERCISE,
    level = "debug",
    skip_all,
    fields(exercise_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete_exercise(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    st.equity.delete_exercise(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/equity-grants/{id}/vesting", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = VestingStatus), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_GRANT_VESTING,
    level = "debug",
    skip_all,
    fields(grant_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn grant_vesting(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<VestingStatus>> {
    Ok(Json(st.equity.grant_vesting(id, q.as_of.as_deref()).await?))
}

/// Vesting status of every grant on an account, plus total intrinsic value.
#[utoipa::path(get, path = "/api/accounts/{id}/equity", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = AccountEquity)))]
#[tracing::instrument(
    name = EQUITY_ACCOUNT_EQUITY,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn account_equity(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<AccountEquity>> {
    Ok(Json(
        st.equity.account_equity(id, q.as_of.as_deref()).await?,
    ))
}

/// Snapshot the account's current equity intrinsic value into a valuation, so it
/// flows into net worth.
#[utoipa::path(post, path = "/api/accounts/{id}/equity/revalue", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = AccountEquity)))]
#[tracing::instrument(
    name = EQUITY_REVALUE,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn revalue(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<AccountEquity>> {
    Ok(Json(st.equity.revalue(id, q.as_of.as_deref()).await?))
}

/// Every mark on an account, newest first — the price ledger behind its valuations.
#[utoipa::path(get, path = "/api/accounts/{id}/equity-marks", tag = "equity",
    params(("id" = i64, Path,)), responses((status = 200, body = [EquityMark])))]
#[tracing::instrument(
    name = EQUITY_LIST_MARKS,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_marks(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<EquityMark>>> {
    Ok(Json(st.equity.list_marks(id).await?))
}

/// Record what one unit is worth from a date on. A mark already on that date is replaced, so
/// correcting a figure needs no delete first.
#[utoipa::path(post, path = "/api/accounts/{id}/equity-marks", tag = "equity",
    params(("id" = i64, Path,)), request_body = SaveMark,
    responses((status = 201, body = EquityMark), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_CREATE_MARK,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_mark(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveMark>,
) -> AppResult<(StatusCode, Json<EquityMark>)> {
    let mark = st.equity.create_mark(id, input).await?;
    Ok((StatusCode::CREATED, Json(mark)))
}

#[utoipa::path(delete, path = "/api/equity-marks/{id}", tag = "equity", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_DELETE_MARK,
    level = "debug",
    skip_all,
    fields(mark_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete_mark(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.equity.delete_mark(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The dated vesting/exercise ledger for an account, newest first — the quantity side of its
/// value. Vesting rows are computed from each grant's schedule rather than stored.
#[utoipa::path(get, path = "/api/accounts/{id}/equity-events", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery), responses((status = 200, body = [EquityEvent])))]
#[tracing::instrument(
    name = EQUITY_LIST_EVENTS,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_events(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<Vec<EquityEvent>>> {
    Ok(Json(st.equity.list_events(id, q.as_of.as_deref()).await?))
}

/// Rebuild the account's whole valuation history from its grant schedule and mark ledger.
///
/// One valuation per date the position's value could change — every vesting tranche, every
/// exercise, every mark. Idempotent: re-running after correcting a mark restates the series
/// rather than doubling it. `as_of` caps how far forward to go, defaulting to today.
#[utoipa::path(post, path = "/api/accounts/{id}/equity/rebuild", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = RebuildResult), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = EQUITY_REBUILD,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn rebuild_history(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<RebuildResult>> {
    Ok(Json(
        st.equity.rebuild_history(id, q.as_of.as_deref()).await?,
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/accounts/{id}/equity-grants",
            get(list_grants).post(create_grant),
        )
        .route("/accounts/{id}/equity", get(account_equity))
        .route("/accounts/{id}/equity/revalue", post(revalue))
        .route("/accounts/{id}/equity/rebuild", post(rebuild_history))
        .route(
            "/accounts/{id}/equity-marks",
            get(list_marks).post(create_mark),
        )
        .route("/accounts/{id}/equity-events", get(list_events))
        .route("/equity-marks/{id}", axum::routing::delete(delete_mark))
        .route(
            "/equity-grants/{id}",
            axum::routing::put(update_grant).delete(delete_grant),
        )
        .route(
            "/equity-grants/{id}/exercises",
            get(list_exercises).post(create_exercise),
        )
        .route("/equity-grants/{id}/vesting", get(grant_vesting))
        .route(
            "/equity-exercises/{id}",
            axum::routing::delete(delete_exercise),
        )
}
