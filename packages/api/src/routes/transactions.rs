use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_dal::transactions::{
    LinkRequest, SaveTransaction, Transaction, TransferRequest, TxQuery,
};

/// List transactions, most recent first, with optional filters.
#[utoipa::path(get, path = "/api/transactions", tag = "transactions", params(TxQuery),
    responses((status = 200, body = [Transaction])))]
pub async fn list(
    State(st): State<AppState>,
    Query(q): Query<TxQuery>,
) -> AppResult<Json<Vec<Transaction>>> {
    Ok(Json(sure_dal::transactions::list(&st.db, q).await?))
}

/// Fetch one transaction.
#[utoipa::path(get, path = "/api/transactions/{id}", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody)))]
pub async fn get_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(sure_dal::transactions::get(&st.db, id).await?))
}

/// Create a transaction.
#[utoipa::path(post, path = "/api/transactions", tag = "transactions",
    request_body = SaveTransaction,
    responses((status = 201, body = Transaction), (status = 422, body = crate::error::ErrorBody)))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveTransaction>,
) -> AppResult<(StatusCode, Json<Transaction>)> {
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::transactions::create(&st.db, input).await?),
    ))
}

/// Replace a transaction. Manually setting the category clears the "categorised by
/// rule" marker, so a later rule re-run won't clobber the manual choice unless it matches.
#[utoipa::path(put, path = "/api/transactions/{id}", tag = "transactions",
    params(("id" = i64, Path,)), request_body = SaveTransaction,
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveTransaction>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(sure_dal::transactions::update(&st.db, id, input).await?))
}

/// Delete a transaction (also clears the other side of any transfer link).
#[utoipa::path(delete, path = "/api/transactions/{id}", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::transactions::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Link two existing transactions as the two sides of a transfer (reciprocal).
#[utoipa::path(post, path = "/api/transactions/{id}/link", tag = "transactions",
    params(("id" = i64, Path,)), request_body = LinkRequest,
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn link(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<LinkRequest>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(sure_dal::transactions::link(&st.db, id, req).await?))
}

/// Remove a transfer link from both sides.
#[utoipa::path(delete, path = "/api/transactions/{id}/link", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody)))]
pub async fn unlink(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(sure_dal::transactions::unlink(&st.db, id).await?))
}

/// Create a transfer: two reciprocally-linked transactions (outflow + inflow).
#[utoipa::path(post, path = "/api/transfers", tag = "transactions",
    request_body = TransferRequest,
    responses((status = 201, body = [Transaction]), (status = 422, body = crate::error::ErrorBody)))]
pub async fn create_transfer(
    State(st): State<AppState>,
    Json(req): Json<TransferRequest>,
) -> AppResult<(StatusCode, Json<Vec<Transaction>>)> {
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::transactions::create_transfer(&st.db, req).await?),
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/transactions", get(list).post(create))
        .route("/transactions/{id}", get(get_one).put(update).delete(delete))
        .route("/transactions/{id}/link", post(link).delete(unlink))
        .route("/transfers", post(create_transfer))
}
