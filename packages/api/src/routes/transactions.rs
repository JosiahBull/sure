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
    BulkDelete, BulkResult, BulkUpdate, DayTotal, FEED_DEFAULT_LIMIT, FEED_MAX_LIMIT, Flow,
    LinkRequest, Ownership, SaveTransaction, SortDir, Transaction, TransferRequest, TxCursor,
    TxFeedQuery, TxPage, TxQuery, TxSort, TxSummary,
};

/// The wire form of [`TxQuery`]. A twin, for one field: `attributed_to` arrives as text
/// (`joint`, or a person id) and is parsed into the domain enum right here — the query
/// string is the one legal place that value is a string (CLAUDE.md rule 1), and an
/// unrecognised one is a 400 rather than a filter that silently matches everybody. Same
/// shape and rationale as `routes::reports`'s `NetWorthQuery`.
#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxQueryParams {
    /// Only cashflow category transactions; excludes internal transfers and account crossings.
    pub cashflow_categories_only: Option<bool>,
    /// Cashflow across the perimeter to/from this account, excluding its bookkeeping.
    pub cashflow_account_id: Option<i64>,
    pub account_id: Option<i64>,
    pub category_id: Option<i64>,
    /// Inclusive lower bound on the transaction date (ISO-8601).
    pub from: Option<String>,
    /// Inclusive upper bound on the transaction date (ISO-8601).
    pub to: Option<String>,
    /// When false, transactions excluded from cashflow are excluded. Defaults to true.
    pub include_excluded_from_cashflow: Option<bool>,
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
            ids: None,
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
            include_excluded_from_cashflow: q.include_excluded_from_cashflow,
            search: q.search,
            uncategorized: q.uncategorized,
            flow: None,
            include_subcategories: false,
            limit: q.limit,
            offset: q.offset,
        })
    }
}

/// The filters `/transactions/feed` and `/transactions/summary` share. Unlike the plain list,
/// `category_id` here takes in the category's whole subtree — the way the reports roll spend
/// up, and so the SPA's category filter agrees with them.
#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxFilterParams {
    /// Only cashflow category transactions; excludes internal transfers and account crossings.
    pub cashflow_categories_only: Option<bool>,
    /// Cashflow across the perimeter to/from this account, excluding its bookkeeping.
    pub cashflow_account_id: Option<i64>,
    pub account_id: Option<i64>,
    /// This category and every category beneath it.
    pub category_id: Option<i64>,
    /// Inclusive lower bound on the transaction date (ISO-8601).
    pub from: Option<String>,
    /// Inclusive upper bound on the transaction date (ISO-8601).
    pub to: Option<String>,
    /// When false, transactions excluded from cashflow are excluded. Defaults to true.
    pub include_excluded_from_cashflow: Option<bool>,
    /// Case-insensitive substring match on description, merchant text, the linked merchant's
    /// name, and notes.
    pub search: Option<String>,
    /// `true` keeps only uncategorised rows, `false` only categorised ones; omitted means both.
    pub uncategorized: Option<bool>,
    /// Whose transactions to show: `joint`, or a household member's id.
    pub attributed_to: Option<String>,
    /// Money in (`income`, amount ≥ 0) or out (`expense`); omitted means both.
    #[param(inline)]
    pub flow: Option<Flow>,
}

/// `/transactions/feed`'s query: [`TxFilterParams`] plus an order and a place in it. Flat
/// rather than `#[serde(flatten)]`ed, because flattening routes a query string through serde's
/// untyped buffer, where `?account_id=3` is the string `"3"` and no longer parses as a number.
#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxFeedParams {
    pub cashflow_categories_only: Option<bool>,
    pub cashflow_account_id: Option<i64>,
    pub account_id: Option<i64>,
    /// This category and every category beneath it.
    pub category_id: Option<i64>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub include_excluded_from_cashflow: Option<bool>,
    pub search: Option<String>,
    pub uncategorized: Option<bool>,
    pub attributed_to: Option<String>,
    #[param(inline)]
    pub flow: Option<Flow>,
    /// The column to order by. Defaults to `date`. Ties always break on ascending id.
    #[param(inline)]
    pub sort: Option<TxSort>,
    /// Defaults to `desc`.
    #[param(inline)]
    pub dir: Option<SortDir>,
    /// The previous page's `next_cursor`; omit for the first page. Only valid with the same
    /// `sort` and `dir` it was issued under — anything else is a 400.
    pub cursor: Option<String>,
    /// Rows per page, 1–500. Defaults to 100.
    pub limit: Option<i64>,
}

impl TxFeedParams {
    fn split(
        self,
    ) -> (
        TxFilterParams,
        Option<TxSort>,
        Option<SortDir>,
        Option<String>,
        Option<i64>,
    ) {
        let TxFeedParams {
            cashflow_categories_only,
            cashflow_account_id,
            account_id,
            category_id,
            from,
            to,
            include_excluded_from_cashflow,
            search,
            uncategorized,
            attributed_to,
            flow,
            sort,
            dir,
            cursor,
            limit,
        } = self;
        let filter = TxFilterParams {
            cashflow_categories_only,
            cashflow_account_id,
            account_id,
            category_id,
            from,
            to,
            include_excluded_from_cashflow,
            search,
            uncategorized,
            attributed_to,
            flow,
        };
        (filter, sort, dir, cursor, limit)
    }
}

/// Parse the shared filters into a [`TxQuery`], resolving the two cashflow modes into the id set
/// the reports would count — the same resolution the plain list does.
async fn resolve_filter(st: &AppState, f: TxFilterParams) -> AppResult<TxQuery> {
    let cashflow = f.cashflow_categories_only.unwrap_or(false) || f.cashflow_account_id.is_some();
    let mut query = TxQuery {
        ids: None,
        attributed_to: f
            .attributed_to
            .as_deref()
            .map(str::parse::<Ownership>)
            .transpose()
            .map_err(AppError::bad_request)?,
        account_id: f.account_id,
        category_id: f.category_id,
        from: f.from,
        to: f.to,
        include_excluded_from_cashflow: f.include_excluded_from_cashflow,
        search: f.search,
        uncategorized: f.uncategorized,
        flow: f.flow,
        include_subcategories: true,
        limit: None,
        offset: None,
    };
    if cashflow {
        query.ids = Some(cashflow_ids(st, &query, f.cashflow_account_id).await?);
    }
    Ok(query)
}

async fn cashflow_ids(
    st: &AppState,
    query: &TxQuery,
    counterparty: Option<i64>,
) -> AppResult<Vec<i64>> {
    st.reports
        .cashflow_transaction_ids(
            &sure_app::reports::ReportQuery {
                from: query.from.clone(),
                to: query.to.clone(),
                include_excluded_from_cashflow: query.include_excluded_from_cashflow,
                attributed_to: query.attributed_to,
                ..Default::default()
            },
            counterparty,
        )
        .await
}

// OTEL span names for this module's handlers.
const TRANSACTIONS_LIST: &str = "transactions.list";
const TRANSACTIONS_FEED: &str = "transactions.feed";
const TRANSACTIONS_SUMMARY: &str = "transactions.summary";
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
    let cashflow = q.cashflow_categories_only.unwrap_or(false) || q.cashflow_account_id.is_some();
    let counterparty = q.cashflow_account_id;
    let mut query: TxQuery = q.try_into()?;
    if cashflow {
        query.ids = Some(cashflow_ids(&st, &query, counterparty).await?);
    }
    Ok(Json(st.transactions.list(query).await?))
}

/// One page of transactions in any order, for an infinitely-scrolling list. Follow
/// `next_cursor` until it is `null`; `days` carries the full-set totals of each day on the page.
#[utoipa::path(get, path = "/api/transactions/feed", tag = "transactions", params(TxFeedParams),
    responses((status = 200, body = TxPage), (status = 400, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_FEED,
    level = "debug",
    skip_all,
    fields(query = ?q),
    err(level = tracing::Level::WARN),
)]
pub async fn feed(
    State(st): State<AppState>,
    Query(q): Query<TxFeedParams>,
) -> AppResult<Json<TxPage>> {
    let (filter, sort, dir, cursor, limit) = q.split();
    let sort = sort.unwrap_or_default();
    let dir = dir.unwrap_or_default();
    let cursor = cursor
        .as_deref()
        .map(|raw| TxCursor::decode(raw, sort, dir))
        .transpose()?;
    let limit = limit.unwrap_or(FEED_DEFAULT_LIMIT);
    if !(1..=FEED_MAX_LIMIT).contains(&limit) {
        return Err(AppError::bad_request(format!(
            "limit must be between 1 and {FEED_MAX_LIMIT}"
        )));
    }
    let filter = resolve_filter(&st, filter).await?;
    Ok(Json(
        st.transactions
            .feed(TxFeedQuery {
                filter,
                sort,
                dir,
                cursor,
                limit,
            })
            .await?,
    ))
}

/// Count, income and outgoings over everything the filters match — the stat bar's figures,
/// which cover the whole set rather than the pages loaded so far.
#[utoipa::path(get, path = "/api/transactions/summary", tag = "transactions", params(TxFilterParams),
    responses((status = 200, body = TxSummary), (status = 400, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = TRANSACTIONS_SUMMARY,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn summary(
    State(st): State<AppState>,
    Query(q): Query<TxFilterParams>,
) -> AppResult<Json<TxSummary>> {
    let filter = resolve_filter(&st, q).await?;
    Ok(Json(st.transactions.summary(filter).await?))
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
    let created = st.transactions.create(input).await?;
    // The ledger moved, so whatever is derived from it should catch up now rather than on
    // the next tick — a hand-entered salary deposit is the obvious case.
    st.nudge.wake_all(sure_app::tasks::LEDGER_CHANGED);
    Ok((StatusCode::CREATED, Json(created)))
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
    // The ledger moved, so whatever is derived from it should catch up now rather than on
    // the next tick — a hand-entered salary deposit is the obvious case.
    st.nudge.wake_all(sure_app::tasks::LEDGER_CHANGED);
    Ok(StatusCode::NO_CONTENT)
}

/// Apply a partial patch (category / merchant / cashflow exclusion) to many transactions at once.
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
    // The ledger moved, so whatever is derived from it should catch up now rather than on
    // the next tick — a hand-entered salary deposit is the obvious case.
    st.nudge.wake_all(sure_app::tasks::LEDGER_CHANGED);
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
    // The ledger moved, so whatever is derived from it should catch up now rather than on
    // the next tick — a hand-entered salary deposit is the obvious case.
    st.nudge.wake_all(sure_app::tasks::LEDGER_CHANGED);
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
        .route("/transactions/feed", get(feed))
        .route("/transactions/summary", get(summary))
        .route("/transactions/bulk-update", post(bulk_update))
        .route("/transactions/bulk-delete", post(bulk_delete))
        .route(
            "/transactions/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route("/transactions/{id}/link", post(link).delete(unlink))
        .route("/transfers", post(create_transfer))
}
