//! Investment-strategy CRUD. Thin, like `routes::commitments`: the projection's use of these lives
//! in `sure_app::forecast` (see `StrategySim` for how a month's sweep is applied) and the DAL owns
//! the queries, so these handlers extract, forward and convert.
//!
//! The repo directly rather than a service, matching `AppState::income` and
//! `AppState::commitments`: five delegating methods with no use-case logic between them.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

// Re-exported so the OpenAPI registration and the handler annotations resolve, matching
// `routes::income`.
pub use sure_core::{InvestmentStrategy, SaveInvestmentStrategy};

const STRATEGIES_LIST: &str = "strategies.list";
const STRATEGIES_GET: &str = "strategies.get";
const STRATEGIES_CREATE: &str = "strategies.create";
const STRATEGIES_UPDATE: &str = "strategies.update";
const STRATEGIES_DELETE: &str = "strategies.delete";

/// Every investment strategy, enabled or not.
#[utoipa::path(get, path = "/api/investment-strategies", tag = "forecast",
    responses((status = 200, body = Vec<InvestmentStrategy>)))]
#[tracing::instrument(name = STRATEGIES_LIST, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<InvestmentStrategy>>> {
    Ok(Json(st.strategies.list_strategies().await?))
}

#[utoipa::path(get, path = "/api/investment-strategies/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 200, body = InvestmentStrategy),
        (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = STRATEGIES_GET, level = "debug", skip_all, fields(id = %id), err(level = tracing::Level::WARN))]
pub async fn get_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<InvestmentStrategy>> {
    Ok(Json(st.strategies.get_strategy(id).await?))
}

#[utoipa::path(post, path = "/api/investment-strategies", tag = "forecast",
    request_body = SaveInvestmentStrategy,
    responses((status = 201, body = InvestmentStrategy),
        (status = 422, description = "unknown target account or income stream, or a window that ends before it starts",
            body = crate::error::ErrorBody)))]
#[tracing::instrument(name = STRATEGIES_CREATE, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveInvestmentStrategy>,
) -> AppResult<(StatusCode, Json<InvestmentStrategy>)> {
    let created = st.strategies.create_strategy(input).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(put, path = "/api/investment-strategies/{id}", tag = "forecast",
    params(("id" = i64, Path,)), request_body = SaveInvestmentStrategy,
    responses((status = 200, body = InvestmentStrategy),
        (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = STRATEGIES_UPDATE, level = "debug", skip_all, fields(id = %id), err(level = tracing::Level::WARN))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveInvestmentStrategy>,
) -> AppResult<Json<InvestmentStrategy>> {
    Ok(Json(st.strategies.update_strategy(id, input).await?))
}

#[utoipa::path(delete, path = "/api/investment-strategies/{id}", tag = "forecast",
    params(("id" = i64, Path,)),
    responses((status = 204, description = "deleted"),
        (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = STRATEGIES_DELETE, level = "debug", skip_all, fields(id = %id), err(level = tracing::Level::WARN))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.strategies.delete_strategy(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/investment-strategies", get(list).post(create))
        .route(
            "/investment-strategies/{id}",
            get(get_one).put(update).delete(delete),
        )
}
