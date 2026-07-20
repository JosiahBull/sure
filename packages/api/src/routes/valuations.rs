use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_core::{NewValuation, Valuation};

// OTEL span names for this module's handlers.
const VALUATIONS_LIST: &str = "valuations.list";
const VALUATIONS_CREATE: &str = "valuations.create";
const VALUATIONS_DELETE: &str = "valuations.delete";

/// List an account's valuations, newest first.
#[utoipa::path(get, path = "/api/accounts/{id}/valuations", tag = "valuations",
    params(("id" = i64, Path,)), responses((status = 200, body = [Valuation])))]
#[tracing::instrument(
    name = VALUATIONS_LIST,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<Valuation>>> {
    Ok(Json(st.valuations.list_for_account(id).await?))
}

/// Record a valuation for an account.
#[utoipa::path(post, path = "/api/accounts/{id}/valuations", tag = "valuations",
    params(("id" = i64, Path,)), request_body = NewValuation,
    responses((status = 201, body = Valuation), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = VALUATIONS_CREATE,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<NewValuation>,
) -> AppResult<(StatusCode, Json<Valuation>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.valuations.create(id, input).await?),
    ))
}

/// Delete a valuation.
#[utoipa::path(delete, path = "/api/valuations/{id}", tag = "valuations",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = VALUATIONS_DELETE,
    level = "debug",
    skip_all,
    fields(valuation_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.valuations.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/valuations", get(list).post(create))
        .route("/valuations/{id}", axum::routing::delete(delete))
}
