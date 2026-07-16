use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;
pub use sure_dal::categories::{Category, CategoryNode, SaveCategory};

/// List all categories (flat).
#[utoipa::path(get, path = "/api/categories", tag = "categories",
    responses((status = 200, body = [Category])))]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Category>>> {
    Ok(Json(sure_dal::categories::list(&st.db).await?))
}

/// The category tree (roots with nested children).
#[utoipa::path(get, path = "/api/categories/tree", tag = "categories",
    responses((status = 200, body = [CategoryNode])))]
pub async fn tree(State(st): State<AppState>) -> AppResult<Json<Vec<CategoryNode>>> {
    Ok(Json(sure_dal::categories::tree(&st.db).await?))
}

/// Create a category.
#[utoipa::path(post, path = "/api/categories", tag = "categories",
    request_body = SaveCategory,
    responses((status = 201, body = Category), (status = 422, body = crate::error::ErrorBody)))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveCategory>,
) -> AppResult<(StatusCode, Json<Category>)> {
    let cat = sure_dal::categories::create(&st.db, input).await?;
    Ok((StatusCode::CREATED, Json(cat)))
}

/// Replace a category.
#[utoipa::path(put, path = "/api/categories/{id}", tag = "categories",
    params(("id" = i64, Path,)), request_body = SaveCategory,
    responses((status = 200, body = Category), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveCategory>,
) -> AppResult<Json<Category>> {
    Ok(Json(sure_dal::categories::update(&st.db, id, input).await?))
}

/// Delete a category. Child categories and transaction links cascade per schema
/// (`ON DELETE CASCADE` for children, `SET NULL` for transactions).
#[utoipa::path(delete, path = "/api/categories/{id}", tag = "categories",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::categories::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/categories", get(list).post(create))
        .route("/categories/tree", get(tree))
        .route(
            "/categories/{id}",
            axum::routing::put(update).delete(delete),
        )
}
