//! Brokerage account endpoints: the computed value snapshot, the raw holdings/dividends
//! ledgers, manual lot entry, and manual revalue/backfill triggers. The heavy lifting
//! (persistence, pricing) lives in `sure_app::brokerage`'s `BrokerageService` (behind the
//! `BrokerageRepo`/`AccountRepo` ports); these handlers are thin glue.
//!
//! Importing a Sharesies export is [`crate::routes::import`]'s job now, with every other file
//! upload — and so is the valuation backfill that import starts, which moved there as the one
//! `sure_app::import::FollowUp` a completed import hands back to the transport to spawn.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use sure_core::AccountKind;
use utoipa::IntoParams;

use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{
    BrokerageActivity30d, BrokerageSnapshot, Dividend, DividendDetail, DividendWithholding,
    HoldingLot, LotKind, Position, SaveHoldingLot, WalletBalance,
};

// OTEL span names for this module's handlers.
const BROKERAGE_SNAPSHOT: &str = "brokerage.snapshot";
const BROKERAGE_LIST_HOLDINGS: &str = "brokerage.list_holdings";
const BROKERAGE_CREATE_HOLDING: &str = "brokerage.create_holding";
const BROKERAGE_DELETE_HOLDING: &str = "brokerage.delete_holding";
const BROKERAGE_LIST_DIVIDENDS: &str = "brokerage.list_dividends";
const BROKERAGE_REVALUE: &str = "brokerage.revalue";
const BROKERAGE_BACKFILL: &str = "brokerage.backfill";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct AsOfQuery {
    /// ISO-8601 date (`YYYY-MM-DD`); defaults to today. An unparseable value also falls
    /// back to today, matching the equity/stock-price endpoints.
    pub as_of: Option<String>,
}

fn parse_as_of(q: &AsOfQuery) -> NaiveDate {
    q.as_of
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn ensure_brokerage(st: &AppState, id: i64) -> AppResult<()> {
    let account = st.accounts.get(id).await?;
    if account.kind != AccountKind::Brokerage {
        return Err(AppError::validation("account is not a brokerage account"));
    }
    Ok(())
}

/// The account's computed value snapshot (positions priced + wallet cash) as of a date.
#[utoipa::path(get, path = "/api/accounts/{id}/brokerage", tag = "brokerage",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = BrokerageSnapshot), (status = 404, body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_SNAPSHOT,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn snapshot(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<BrokerageSnapshot>> {
    ensure_brokerage(&st, id).await?;
    Ok(Json(
        st.brokerage
            .snapshot(Some(st.stock_price_provider.as_ref()), id, parse_as_of(&q))
            .await?,
    ))
}

/// The raw holdings ledger (every buy/sell/corporate lot) for an audit view.
#[utoipa::path(get, path = "/api/accounts/{id}/brokerage/holdings", tag = "brokerage",
    params(("id" = i64, Path,)), responses((status = 200, body = [HoldingLot])))]
#[tracing::instrument(
    name = BROKERAGE_LIST_HOLDINGS,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_holdings(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<HoldingLot>>> {
    Ok(Json(st.brokerage.list_holdings(id).await?))
}

/// Manually record a lot (most arrive via import; this is parity with equity's manual grant).
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/holdings", tag = "brokerage",
    params(("id" = i64, Path,)), request_body = SaveHoldingLot,
    responses((status = 201, body = HoldingLot), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_CREATE_HOLDING,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_holding(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveHoldingLot>,
) -> AppResult<(StatusCode, Json<HoldingLot>)> {
    ensure_brokerage(&st, id).await?;
    Ok((
        StatusCode::CREATED,
        Json(st.brokerage.create_holding(id, input).await?),
    ))
}

#[utoipa::path(delete, path = "/api/brokerage/holdings/{id}", tag = "brokerage",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_DELETE_HOLDING,
    level = "debug",
    skip_all,
    fields(holding_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete_holding(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    st.brokerage.delete_holding(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Dividend/distribution history with per-jurisdiction withholding detail.
#[utoipa::path(get, path = "/api/accounts/{id}/brokerage/dividends", tag = "brokerage",
    params(("id" = i64, Path,)), responses((status = 200, body = [DividendDetail])))]
#[tracing::instrument(
    name = BROKERAGE_LIST_DIVIDENDS,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_dividends(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<DividendDetail>>> {
    Ok(Json(st.brokerage.list_dividends(id).await?))
}

/// Snapshot the account's current value into a `source='brokerage'` valuation (mirrors
/// equity's "Revalue").
///
/// 422 when a holding's currency has no exchange rate to the account's: the snapshot would
/// understate the account, and a stored valuation carries no hint that it did.
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/revalue", tag = "brokerage",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = BrokerageSnapshot), (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_REVALUE,
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
) -> AppResult<Json<BrokerageSnapshot>> {
    ensure_brokerage(&st, id).await?;
    Ok(Json(
        st.brokerage
            .revalue(Some(st.stock_price_provider.as_ref()), id, parse_as_of(&q))
            .await?,
    ))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct BackfillResult {
    /// Number of days for which a valuation was (re)computed.
    pub days: i64,
}

/// Rebuild the whole daily valuation history from the price cache/provider. Runs
/// synchronously (unlike the post-import background backfill) so the response reports how
/// many days were valued — the manual retry escape hatch.
///
/// Note this route's response set differs from `snapshot`'s and `revalue`'s, and not by
/// oversight: **it cannot answer 502.** `BrokerageService::backfill_history` catches each
/// ticker's `fetch_daily_prices` failure and only warns — one delisted ticker must not sink a
/// whole history — then walks the days with `provider=None`, which opens no socket. So a total
/// price-feed outage answers 200 here, with a history of cash-only valuations. Declaring a 502 a
/// client can never receive is worse than declaring nothing: it invites a caller to wait for a
/// signal that does not come. The 422 below is the one it really can produce, propagated out of
/// `revalue`'s refusal to persist a total it could not convert.
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/backfill", tag = "brokerage",
    params(("id" = i64, Path,)),
    responses((status = 200, body = BackfillResult), (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_BACKFILL,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn backfill(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<BackfillResult>> {
    ensure_brokerage(&st, id).await?;
    let days = st
        .brokerage
        .backfill_history(st.stock_price_provider.as_ref(), id)
        .await? as i64;
    Ok(Json(BackfillResult { days }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/brokerage", get(snapshot))
        .route(
            "/accounts/{id}/brokerage/holdings",
            get(list_holdings).post(create_holding),
        )
        .route(
            "/brokerage/holdings/{id}",
            axum::routing::delete(delete_holding),
        )
        .route("/accounts/{id}/brokerage/dividends", get(list_dividends))
        .route("/accounts/{id}/brokerage/revalue", post(revalue))
        .route("/accounts/{id}/brokerage/backfill", post(backfill))
}
