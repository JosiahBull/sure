use crate::{error::AppResult, extract::Json, state::AppState};
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
pub use sure_core::{CategoryMapping, SaveCategoryMapping};

#[utoipa::path(get, path = "/api/category-mappings", tag = "categories",
    responses((status = 200, body = [CategoryMapping])))]
#[tracing::instrument(name = "category_mappings.list", level = "debug", skip_all)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<CategoryMapping>>> {
    Ok(Json(st.categories.list_mappings().await?))
}

#[utoipa::path(post, path = "/api/category-mappings", tag = "categories", request_body = SaveCategoryMapping,
    responses((status = 201, body = CategoryMapping), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = "category_mappings.create", level = "debug", skip_all)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveCategoryMapping>,
) -> AppResult<(StatusCode, Json<CategoryMapping>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.categories.create_mapping(input).await?),
    ))
}

#[utoipa::path(put, path = "/api/category-mappings/{id}", tag = "categories", params(("id" = i64, Path,)), request_body = SaveCategoryMapping,
    responses((status = 200, body = CategoryMapping), (status = 404, body = crate::error::ErrorBody), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = "category_mappings.update", level = "debug", skip_all)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveCategoryMapping>,
) -> AppResult<Json<CategoryMapping>> {
    Ok(Json(st.categories.update_mapping(id, input).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/category-mappings", get(list).post(create))
        .route("/category-mappings/{id}", axum::routing::put(update))
}
