use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_dal::valuations::{NewValuation, Valuation};

/// List an account's valuations, newest first.
#[utoipa::path(get, path = "/api/accounts/{id}/valuations", tag = "valuations",
    params(("id" = i64, Path,)), responses((status = 200, body = [Valuation])))]
pub async fn list(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<Vec<Valuation>>> {
    Ok(Json(sure_dal::valuations::list_for_account(&st.db, id).await?))
}

/// Record a valuation for an account.
#[utoipa::path(post, path = "/api/accounts/{id}/valuations", tag = "valuations",
    params(("id" = i64, Path,)), request_body = NewValuation,
    responses((status = 201, body = Valuation), (status = 404, body = crate::error::ErrorBody)))]
pub async fn create(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<NewValuation>,
) -> AppResult<(StatusCode, Json<Valuation>)> {
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::valuations::create(&st.db, id, input).await?),
    ))
}

/// Delete a valuation.
#[utoipa::path(delete, path = "/api/valuations/{id}", tag = "valuations",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::valuations::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/valuations", get(list).post(create))
        .route("/valuations/{id}", axum::routing::delete(delete))
}
