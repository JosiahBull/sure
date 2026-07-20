use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

// Types + queries live in the DAL; re-export so the OpenAPI registration
// (`crate::routes::currencies::Currency`, ...) and handler annotations still resolve.
pub use sure_core::{Currency, NewCurrency};

// OTEL span names for this module's handlers.
const CURRENCIES_LIST: &str = "currencies.list";
const CURRENCIES_CREATE: &str = "currencies.create";
const CURRENCIES_DELETE: &str = "currencies.delete";

/// List all currencies.
#[utoipa::path(get, path = "/api/currencies", tag = "currencies",
    responses((status = 200, body = [Currency])))]
#[tracing::instrument(
    name = CURRENCIES_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Currency>>> {
    Ok(Json(st.currencies.list().await?))
}

/// Create or replace a currency (upsert on `code`).
#[utoipa::path(post, path = "/api/currencies", tag = "currencies",
    request_body = NewCurrency,
    responses((status = 201, body = Currency), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CURRENCIES_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<NewCurrency>,
) -> AppResult<(StatusCode, Json<Currency>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.currencies.upsert(input).await?),
    ))
}

/// Delete a currency (fails if referenced by accounts/transactions).
#[utoipa::path(delete, path = "/api/currencies/{code}", tag = "currencies",
    params(("code" = String, Path,)),
    responses((status = 204), (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CURRENCIES_DELETE,
    level = "debug",
    skip_all,
    fields(currency_code = %code),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(code): Path<String>) -> AppResult<StatusCode> {
    st.currencies.delete(&code).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/currencies", get(list).post(create))
        .route("/currencies/{code}", axum::routing::delete(delete))
}
