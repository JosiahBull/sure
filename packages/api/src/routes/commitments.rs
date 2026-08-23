//! Expense-commitment CRUD. Thin — the projection's use of these lives in `sure_app::forecast`
//! (see `CommitmentNetting` for the invariant that makes them safe to net out), and the DAL owns
//! the queries, so these handlers extract, forward and convert.
//!
//! Deliberately the repo directly rather than a service, for the same reason `AppState::income` is:
//! there is no use-case logic here, and wrapping five delegating methods in a service would add a
//! layer whose only content is the delegation.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

// Re-exported so the OpenAPI registration and the handler annotations resolve, matching
// `routes::income`.
pub use sure_core::{ExpenseCommitment, SaveExpenseCommitment};

const COMMITMENTS_LIST: &str = "commitments.list";
const COMMITMENTS_GET: &str = "commitments.get";
const COMMITMENTS_CREATE: &str = "commitments.create";
const COMMITMENTS_UPDATE: &str = "commitments.update";
const COMMITMENTS_DELETE: &str = "commitments.delete";

/// Every expense commitment, enabled or not.
#[utoipa::path(get, path = "/api/expense-commitments", tag = "forecast",
    responses((status = 200, body = Vec<ExpenseCommitment>)))]
#[tracing::instrument(name = COMMITMENTS_LIST, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<ExpenseCommitment>>> {
    Ok(Json(st.commitments.list_commitments().await?))
}

#[utoipa::path(get, path = "/api/expense-commitments/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 200, body = ExpenseCommitment),
        (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = COMMITMENTS_GET, level = "debug", skip_all, fields(id = %id), err(level = tracing::Level::WARN))]
pub async fn get_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<ExpenseCommitment>> {
    Ok(Json(st.commitments.get_commitment(id).await?))
}

#[utoipa::path(post, path = "/api/expense-commitments", tag = "forecast",
    request_body = SaveExpenseCommitment,
    responses((status = 201, body = ExpenseCommitment),
        (status = 422, description = "unknown category/currency/merchant, or an end before the start",
            body = crate::error::ErrorBody)))]
#[tracing::instrument(name = COMMITMENTS_CREATE, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveExpenseCommitment>,
) -> AppResult<(StatusCode, Json<ExpenseCommitment>)> {
    let created = st.commitments.create_commitment(input).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(put, path = "/api/expense-commitments/{id}", tag = "forecast",
    params(("id" = i64, Path,)), request_body = SaveExpenseCommitment,
    responses((status = 200, body = ExpenseCommitment),
        (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = COMMITMENTS_UPDATE, level = "debug", skip_all, fields(id = %id), err(level = tracing::Level::WARN))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveExpenseCommitment>,
) -> AppResult<Json<ExpenseCommitment>> {
    Ok(Json(st.commitments.update_commitment(id, input).await?))
}

#[utoipa::path(delete, path = "/api/expense-commitments/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 204, description = "deleted"),
        (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = COMMITMENTS_DELETE, level = "debug", skip_all, fields(id = %id), err(level = tracing::Level::WARN))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.commitments.delete_commitment(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/expense-commitments", get(list).post(create))
        .route(
            "/expense-commitments/{id}",
            get(get_one).put(update).delete(delete),
        )
}
