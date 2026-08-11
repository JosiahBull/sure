use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

// The domain types live in sure-core; re-export so the OpenAPI registration
// (`crate::routes::people::Person`, ...) and the handler annotations resolve.
pub use sure_core::{Person, SavePerson};

// OTEL span names for this module's handlers.
const PEOPLE_LIST: &str = "people.list";
const PEOPLE_GET: &str = "people.get";
const PEOPLE_CREATE: &str = "people.create";
const PEOPLE_UPDATE: &str = "people.update";
const PEOPLE_DELETE: &str = "people.delete";

/// List the household.
#[utoipa::path(get, path = "/api/people", tag = "people",
    responses((status = 200, body = [Person])))]
#[tracing::instrument(
    name = PEOPLE_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Person>>> {
    Ok(Json(st.people.list().await?))
}

/// Fetch one household member.
#[utoipa::path(get, path = "/api/people/{id}", tag = "people", params(("id" = i64, Path,)),
    responses((status = 200, body = Person), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PEOPLE_GET,
    level = "debug",
    skip_all,
    fields(person_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_one(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<Person>> {
    Ok(Json(st.people.get(id).await?))
}

/// Add someone to the household.
#[utoipa::path(post, path = "/api/people", tag = "people", request_body = SavePerson,
    responses((status = 201, body = Person), (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PEOPLE_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SavePerson>,
) -> AppResult<(StatusCode, Json<Person>)> {
    Ok((StatusCode::CREATED, Json(st.people.create(input).await?)))
}

/// Rename or restyle a household member.
#[utoipa::path(put, path = "/api/people/{id}", tag = "people", params(("id" = i64, Path,)),
    request_body = SavePerson,
    responses((status = 200, body = Person), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PEOPLE_UPDATE,
    level = "debug",
    skip_all,
    fields(person_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SavePerson>,
) -> AppResult<Json<Person>> {
    Ok(Json(st.people.update(id, input).await?))
}

/// Remove someone from the household. Refused with 409 while any account is still
/// attributed to them — re-attribute those accounts first.
#[utoipa::path(delete, path = "/api/people/{id}", tag = "people", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PEOPLE_DELETE,
    level = "debug",
    skip_all,
    fields(person_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.people.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/people", get(list).post(create))
        .route("/people/{id}", get(get_one).put(update).delete(delete))
}
