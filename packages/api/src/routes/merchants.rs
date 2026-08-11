use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

// Types + queries live in the DAL; re-export so the OpenAPI registration
// (`crate::routes::merchants::Merchant`, ...) and handler annotations still resolve.
pub use sure_core::{Merchant, SaveMerchant};

// OTEL span names for this module's handlers.
const MERCHANTS_LIST: &str = "merchants.list";
const MERCHANTS_CREATE: &str = "merchants.create";
const MERCHANTS_UPDATE: &str = "merchants.update";
const MERCHANTS_DELETE: &str = "merchants.delete";

#[utoipa::path(get, path = "/api/merchants", tag = "merchants",
    responses((status = 200, body = [Merchant])))]
#[tracing::instrument(
    name = MERCHANTS_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Merchant>>> {
    Ok(Json(st.merchants.list().await?))
}

#[utoipa::path(post, path = "/api/merchants", tag = "merchants", request_body = SaveMerchant,
    responses((status = 201, body = Merchant), (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = MERCHANTS_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveMerchant>,
) -> AppResult<(StatusCode, Json<Merchant>)> {
    Ok((StatusCode::CREATED, Json(st.merchants.create(input).await?)))
}

#[utoipa::path(put, path = "/api/merchants/{id}", tag = "merchants", params(("id" = i64, Path,)),
    request_body = SaveMerchant,
    responses((status = 200, body = Merchant), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = MERCHANTS_UPDATE,
    level = "debug",
    skip_all,
    fields(merchant_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveMerchant>,
) -> AppResult<Json<Merchant>> {
    Ok(Json(st.merchants.update(id, input).await?))
}

#[utoipa::path(delete, path = "/api/merchants/{id}", tag = "merchants", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = MERCHANTS_DELETE,
    level = "debug",
    skip_all,
    fields(merchant_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.merchants.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/merchants", get(list).post(create))
        .route("/merchants/{id}", axum::routing::put(update).delete(delete))
}
