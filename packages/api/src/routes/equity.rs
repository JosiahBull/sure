use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_dal::equity::{
    AccountEquity, EquityExercise, EquityGrant, SaveExercise, SaveGrant, VestingStatus,
};

#[derive(Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct AsOfQuery {
    pub as_of: Option<String>,
}

#[utoipa::path(get, path = "/api/accounts/{id}/equity-grants", tag = "equity",
    params(("id" = i64, Path,)), responses((status = 200, body = [EquityGrant])))]
pub async fn list_grants(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<EquityGrant>>> {
    Ok(Json(sure_dal::equity::list_grants(&st.db, id).await?))
}

#[utoipa::path(post, path = "/api/accounts/{id}/equity-grants", tag = "equity",
    params(("id" = i64, Path,)), request_body = SaveGrant,
    responses((status = 201, body = EquityGrant), (status = 404, body = crate::error::ErrorBody)))]
pub async fn create_grant(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveGrant>,
) -> AppResult<(StatusCode, Json<EquityGrant>)> {
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::equity::create_grant(&st.db, id, input).await?),
    ))
}

#[utoipa::path(put, path = "/api/equity-grants/{id}", tag = "equity", params(("id" = i64, Path,)),
    request_body = SaveGrant,
    responses((status = 200, body = EquityGrant), (status = 404, body = crate::error::ErrorBody)))]
pub async fn update_grant(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveGrant>,
) -> AppResult<Json<EquityGrant>> {
    Ok(Json(sure_dal::equity::update_grant(&st.db, id, input).await?))
}

#[utoipa::path(delete, path = "/api/equity-grants/{id}", tag = "equity", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete_grant(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::equity::delete_grant(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/equity-grants/{id}/exercises", tag = "equity",
    params(("id" = i64, Path,)), responses((status = 200, body = [EquityExercise])))]
pub async fn list_exercises(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<EquityExercise>>> {
    Ok(Json(sure_dal::equity::list_exercises(&st.db, id).await?))
}

#[utoipa::path(post, path = "/api/equity-grants/{id}/exercises", tag = "equity",
    params(("id" = i64, Path,)), request_body = SaveExercise,
    responses((status = 201, body = EquityExercise), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn create_exercise(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveExercise>,
) -> AppResult<(StatusCode, Json<EquityExercise>)> {
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::equity::create_exercise(&st.db, id, input).await?),
    ))
}

#[utoipa::path(delete, path = "/api/equity-exercises/{id}", tag = "equity", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete_exercise(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    sure_dal::equity::delete_exercise(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/equity-grants/{id}/vesting", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = VestingStatus), (status = 404, body = crate::error::ErrorBody)))]
pub async fn grant_vesting(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<VestingStatus>> {
    Ok(Json(
        sure_dal::equity::grant_vesting(&st.db, id, q.as_of.as_deref()).await?,
    ))
}

/// Vesting status of every grant on an account, plus total intrinsic value.
#[utoipa::path(get, path = "/api/accounts/{id}/equity", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = AccountEquity)))]
pub async fn account_equity(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<AccountEquity>> {
    Ok(Json(
        sure_dal::equity::account_equity(&st.db, id, q.as_of.as_deref()).await?,
    ))
}

/// Snapshot the account's current equity intrinsic value into a valuation, so it
/// flows into net worth.
#[utoipa::path(post, path = "/api/accounts/{id}/equity/revalue", tag = "equity",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = AccountEquity)))]
pub async fn revalue(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<AccountEquity>> {
    Ok(Json(
        sure_dal::equity::revalue(&st.db, id, q.as_of.as_deref()).await?,
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/equity-grants", get(list_grants).post(create_grant))
        .route("/accounts/{id}/equity", get(account_equity))
        .route("/accounts/{id}/equity/revalue", post(revalue))
        .route("/equity-grants/{id}", axum::routing::put(update_grant).delete(delete_grant))
        .route("/equity-grants/{id}/exercises", get(list_exercises).post(create_exercise))
        .route("/equity-grants/{id}/vesting", get(grant_vesting))
        .route("/equity-exercises/{id}", axum::routing::delete(delete_exercise))
}
