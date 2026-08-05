use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

// The domain types live in sure-core; re-export so the OpenAPI registration
// (`crate::routes::income::IncomeStream`, ...) and the handler annotations resolve.
pub use sure_core::{
    IncomeBasis, IncomeStream, IncomeStreamStep, PayFrequency, SaveIncomeStream,
    SaveIncomeStreamStep, TakeHomeSource,
};

// OTEL span names for this module's handlers.
const INCOME_LIST: &str = "income.list";
const INCOME_GET: &str = "income.get";
const INCOME_CREATE: &str = "income.create";
const INCOME_UPDATE: &str = "income.update";
const INCOME_DELETE: &str = "income.delete";

/// Every income stream in the household, with its dated pay-scale steps attached.
///
/// Flat and unfiltered: the income screen wants every person's streams at once, and one request
/// per person would be N round trips for a few rows.
#[utoipa::path(get, path = "/api/income-streams", tag = "income",
    responses((status = 200, body = [IncomeStream])))]
#[tracing::instrument(
    name = INCOME_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<IncomeStream>>> {
    Ok(Json(st.forecast.list_income_streams().await?))
}

/// One income stream.
#[utoipa::path(get, path = "/api/income-streams/{id}", tag = "income",
    params(("id" = i64, Path,)),
    responses((status = 200, body = IncomeStream), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_GET,
    level = "debug",
    skip_all,
    fields(stream_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn get_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<IncomeStream>> {
    Ok(Json(st.forecast.get_income_stream(id).await?))
}

/// Record income for someone in the household.
///
/// Nested under the person, and flat for every mutation below — the `valuations` arrangement. It
/// puts `person_id` in the path, where it cannot be omitted or contradicted by the body, and keeps
/// the mutation URLs stable.
#[utoipa::path(post, path = "/api/people/{person_id}/income-streams", tag = "income",
    params(("person_id" = i64, Path,)), request_body = SaveIncomeStream,
    responses((status = 201, body = IncomeStream), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_CREATE,
    level = "debug",
    skip_all,
    fields(person_id = %person_id),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Path(person_id): Path<i64>,
    Json(input): Json<SaveIncomeStream>,
) -> AppResult<(StatusCode, Json<IncomeStream>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.forecast.create_income_stream(person_id, input).await?),
    ))
}

/// Replace an income stream, its pay-scale schedule included.
///
/// The steps sent here *are* the schedule afterwards, so removing one is omitting it — the
/// full-replace contract `PUT /api/forecast/assumptions` already has.
#[utoipa::path(put, path = "/api/income-streams/{id}", tag = "income",
    params(("id" = i64, Path,)), request_body = SaveIncomeStream,
    responses((status = 200, body = IncomeStream), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_UPDATE,
    level = "debug",
    skip_all,
    fields(stream_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveIncomeStream>,
) -> AppResult<Json<IncomeStream>> {
    Ok(Json(st.forecast.update_income_stream(id, input).await?))
}

/// Remove an income stream. Refused with 409 while a forecast change still points at it — repoint
/// or remove those first, so a promotion cannot quietly become a no-op.
#[utoipa::path(delete, path = "/api/income-streams/{id}", tag = "income",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_DELETE,
    level = "debug",
    skip_all,
    fields(stream_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.forecast.delete_income_stream(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/income-streams", get(list))
        .route(
            "/income-streams/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route("/people/{person_id}/income-streams", post(create))
}
