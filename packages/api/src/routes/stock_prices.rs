use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use sure_core::{AccountKind, AccountMetadata};
use utoipa::IntoParams;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub use sure_core::StockPrice;

// OTEL span names for this module's handlers.
const STOCK_PRICES_GET: &str = "stock_prices.get";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct AsOfQuery {
    /// ISO-8601 date (`YYYY-MM-DD`); defaults to today. An unparseable value also
    /// falls back to today, same as the equity endpoints' `as_of`.
    pub as_of: Option<String>,
}

/// A shares account's closing price as of a given date (defaults to today),
/// backfilling the historical cache from the configured provider on a miss.
#[utoipa::path(get, path = "/api/accounts/{id}/stock-price", tag = "accounts",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = StockPrice), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = STOCK_PRICES_GET,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_price(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<StockPrice>> {
    let account = sure_dal::accounts::get(&st.db, id).await?;
    if !matches!(account.kind, AccountKind::SharesNz | AccountKind::SharesUs) {
        return Err(AppError::NotFound("stock ticker"));
    }
    let AccountMetadata::Shares(meta) = account.metadata else {
        return Err(AppError::NotFound("stock ticker"));
    };
    let ticker = meta.ticker.filter(|t| !t.trim().is_empty()).ok_or(AppError::NotFound("stock ticker"))?;
    let exchange = meta.exchange.unwrap_or_default();

    let as_of = q
        .as_of
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive());

    let provider = sure_providers::YahooFinanceProvider::new();
    crate::stock_prices::price_at(&st.db, &provider, &ticker, &exchange, as_of)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound("stock price"))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/accounts/{id}/stock-price", get(get_price))
}
