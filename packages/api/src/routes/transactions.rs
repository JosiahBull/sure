use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{
    BulkDelete, BulkResult, BulkUpdate, LinkRequest, Ownership, SaveTransaction, Transaction,
    TransferRequest, TxQuery,
};

/// The wire form of [`TxQuery`]. A twin, for one field: `attributed_to` arrives as text
/// (`joint`, or a person id) and is parsed into the domain enum right here — the query
/// string is the one legal place that value is a string (CLAUDE.md rule 1), and an
/// unrecognised one is a 400 rather than a filter that silently matches everybody. Same
/// shape and rationale as `routes::reports`'s `NetWorthQuery`.
#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxQueryParams {
    pub account_id: Option<i64>,
    pub category_id: Option<i64>,
    /// Inclusive lower bound on the transaction date (ISO-8601).
    pub from: Option<String>,
    /// Inclusive upper bound on the transaction date (ISO-8601).
    pub to: Option<String>,
    /// When false, one-off transactions are excluded. Defaults to true.
    pub include_one_off: Option<bool>,
    /// Case-insensitive substring match on description/merchant/notes.
    pub search: Option<String>,
    /// `true` keeps only uncategorised rows, `false` only categorised ones; omitted means
    /// both. `category_id` cannot express this — there is no id standing for "none".
    pub uncategorized: Option<bool>,
    /// Whose transactions to show: `joint`, or a household member's id. Matches on the
    /// *effective* attribution — a transaction's own override, or its account's owner.
    pub attributed_to: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl TryFrom<TxQueryParams> for TxQuery {
    type Error = AppError;

    fn try_from(q: TxQueryParams) -> Result<Self, Self::Error> {
        Ok(TxQuery {
            attributed_to: q
                .attributed_to
                .as_deref()
                .map(str::parse::<Ownership>)
                .transpose()
                .map_err(AppError::bad_request)?,
            account_id: q.account_id,
            category_id: q.category_id,
            from: q.from,
            to: q.to,
            include_one_off: q.include_one_off,
            search: q.search,
            uncategorized: q.uncategorized,
            limit: q.limit,
            offset: q.offset,
        })
    }
}

// OTEL span names for this module's handlers.
const TRANSACTIONS_LIST: &str = "transactions.list";
const TRANSACTIONS_GET: &str = "transactions.get";
const TRANSACTIONS_CREATE: &str = "transactions.create";
const TRANSACTIONS_UPDATE: &str = "transactions.update";
const TRANSACTIONS_DELETE: &str = "transactions.delete";
const TRANSACTIONS_BULK_UPDATE: &str = "transactions.bulk_update";
const TRANSACTIONS_BULK_DELETE: &str = "transactions.bulk_delete";
const TRANSACTIONS_LINK: &str = "transactions.link";
const TRANSACTIONS_UNLINK: &str = "transactions.unlink";
const TRANSACTIONS_CREATE_TRANSFER: &str = "transactions.create_transfer";

/// List transactions, most recent first, with optional filters.
#[utoipa::path(get, path = "/api/transactions", tag = "transactions", params(TxQueryParams),
    responses((status = 200, body = [Transaction]), (status = 400, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_LIST,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(
    State(st): State<AppState>,
    Query(q): Query<TxQueryParams>,
) -> AppResult<Json<Vec<Transaction>>> {
    Ok(Json(st.transactions.list(q.try_into()?).await?))
}

/// Fetch one transaction.
#[utoipa::path(get, path = "/api/transactions/{id}", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_GET,
    level = "debug",
    skip_all,
    fields(transaction_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(st.transactions.get(id).await?))
}

/// Create a transaction.
#[utoipa::path(post, path = "/api/transactions", tag = "transactions",
    request_body = SaveTransaction,
    responses((status = 201, body = Transaction), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveTransaction>,
) -> AppResult<(StatusCode, Json<Transaction>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.transactions.create(input).await?),
    ))
}

/// Replace a transaction. Manually setting the category clears the "categorised by
/// rule" marker, so a later rule re-run won't clobber the manual choice unless it matches.
#[utoipa::path(put, path = "/api/transactions/{id}", tag = "transactions",
    params(("id" = i64, Path,)), request_body = SaveTransaction,
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_UPDATE,
    level = "debug",
    skip_all,
    fields(transaction_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveTransaction>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(st.transactions.update(id, input).await?))
}

/// Delete a transaction (also clears the other side of any transfer link).
#[utoipa::path(delete, path = "/api/transactions/{id}", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_DELETE,
    level = "debug",
    skip_all,
    fields(transaction_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.transactions.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Apply a partial patch (category / merchant / one-off) to many transactions at once.
/// Omitted fields are left untouched; an explicit `null` clears a category/merchant.
#[utoipa::path(post, path = "/api/transactions/bulk-update", tag = "transactions",
    request_body = BulkUpdate,
    responses((status = 200, body = BulkResult), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_BULK_UPDATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn bulk_update(
    State(st): State<AppState>,
    Json(input): Json<BulkUpdate>,
) -> AppResult<Json<BulkResult>> {
    let affected = st.transactions.bulk_update(input).await?;
    Ok(Json(BulkResult { affected }))
}

/// Delete many transactions at once (also clears the other side of any transfer links).
#[utoipa::path(post, path = "/api/transactions/bulk-delete", tag = "transactions",
    request_body = BulkDelete,
    responses((status = 200, body = BulkResult)))]
#[tracing::instrument(
    name = TRANSACTIONS_BULK_DELETE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn bulk_delete(
    State(st): State<AppState>,
    Json(input): Json<BulkDelete>,
) -> AppResult<Json<BulkResult>> {
    let affected = st.transactions.bulk_delete(&input.ids).await?;
    Ok(Json(BulkResult { affected }))
}

/// Link two existing transactions as the two sides of a transfer (reciprocal).
#[utoipa::path(post, path = "/api/transactions/{id}/link", tag = "transactions",
    params(("id" = i64, Path,)), request_body = LinkRequest,
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_LINK,
    level = "debug",
    skip_all,
    fields(transaction_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn link(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<LinkRequest>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(st.transactions.link(id, req).await?))
}

/// Remove a transfer link from both sides.
#[utoipa::path(delete, path = "/api/transactions/{id}/link", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 200, body = Transaction), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_UNLINK,
    level = "debug",
    skip_all,
    fields(transaction_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn unlink(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Transaction>> {
    Ok(Json(st.transactions.unlink(id).await?))
}

/// Create a transfer: two reciprocally-linked transactions (outflow + inflow).
#[utoipa::path(post, path = "/api/transfers", tag = "transactions",
    request_body = TransferRequest,
    responses((status = 201, body = [Transaction]), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_CREATE_TRANSFER,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_transfer(
    State(st): State<AppState>,
    Json(req): Json<TransferRequest>,
) -> AppResult<(StatusCode, Json<Vec<Transaction>>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.transactions.create_transfer(req).await?),
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/transactions", get(list).post(create))
        .route("/transactions/bulk-update", post(bulk_update))
        .route("/transactions/bulk-delete", post(bulk_delete))
        .route(
            "/transactions/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route("/transactions/{id}/link", post(link).delete(unlink))
        .route("/transfers", post(create_transfer))
}
