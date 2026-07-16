use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

// Types + queries live in the DAL; re-export so the OpenAPI registration
// (`crate::routes::merchants::Merchant`, ...) and handler annotations still resolve.
pub use sure_dal::merchants::{Merchant, SaveMerchant};

#[utoipa::path(get, path = "/api/merchants", tag = "merchants",
    responses((status = 200, body = [Merchant])))]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Merchant>>> {
    Ok(Json(sure_dal::merchants::list(&st.db).await?))
}

#[utoipa::path(post, path = "/api/merchants", tag = "merchants", request_body = SaveMerchant,
    responses((status = 201, body = Merchant), (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveMerchant>,
) -> AppResult<(StatusCode, Json<Merchant>)> {
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::merchants::create(&st.db, input).await?),
    ))
}

#[utoipa::path(put, path = "/api/merchants/{id}", tag = "merchants", params(("id" = i64, Path,)),
    request_body = SaveMerchant,
    responses((status = 200, body = Merchant), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveMerchant>,
) -> AppResult<Json<Merchant>> {
    Ok(Json(sure_dal::merchants::update(&st.db, id, input).await?))
}

#[utoipa::path(delete, path = "/api/merchants/{id}", tag = "merchants", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::merchants::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/merchants", get(list).post(create))
        .route("/merchants/{id}", axum::routing::put(update).delete(delete))
}
