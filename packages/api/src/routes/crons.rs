use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_dal::crons::{Cron, CronRun, CronRunResult, SaveCron};

// OTEL span names for this module's handlers.
const CRONS_LIST: &str = "crons.list";
const CRONS_CREATE: &str = "crons.create";
const CRONS_UPDATE: &str = "crons.update";
const CRONS_DELETE: &str = "crons.delete";
const CRONS_RUN_ONE: &str = "crons.run_one";
const CRONS_RUN_ALL: &str = "crons.run_all";
const CRONS_LIST_RUNS: &str = "crons.list_runs";
const CRONS_UNDO_RUN: &str = "crons.undo_run";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct RunQuery {
    /// Apply all due periods up to this date (default: today).
    pub to: Option<String>,
}

#[utoipa::path(get, path = "/api/crons", tag = "crons", responses((status = 200, body = [Cron])))]
#[tracing::instrument(
    name = CRONS_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Cron>>> {
    Ok(Json(sure_dal::crons::list(&st.db).await?))
}

#[utoipa::path(post, path = "/api/crons", tag = "crons", request_body = SaveCron,
    responses((status = 201, body = Cron), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CRONS_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveCron>,
) -> AppResult<(StatusCode, Json<Cron>)> {
    Ok((StatusCode::CREATED, Json(sure_dal::crons::create(&st.db, input).await?)))
}

#[utoipa::path(put, path = "/api/crons/{id}", tag = "crons", params(("id" = i64, Path,)),
    request_body = SaveCron,
    responses((status = 200, body = Cron), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CRONS_UPDATE,
    level = "debug",
    skip_all,
    fields(cron_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveCron>,
) -> AppResult<Json<Cron>> {
    Ok(Json(sure_dal::crons::update(&st.db, id, input).await?))
}

#[utoipa::path(delete, path = "/api/crons/{id}", tag = "crons", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CRONS_DELETE,
    level = "debug",
    skip_all,
    fields(cron_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::crons::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Apply all due periods for one cron up to the target date.
#[utoipa::path(post, path = "/api/crons/{id}/run", tag = "crons", params(("id" = i64, Path,), RunQuery),
    responses((status = 200, body = CronRunResult), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CRONS_RUN_ONE,
    level = "debug",
    skip_all,
    fields(cron_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn run_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<RunQuery>,
) -> AppResult<Json<CronRunResult>> {
    Ok(Json(sure_dal::crons::run_one(&st.db, id, q.to.as_deref()).await?))
}

/// Apply all due periods for every enabled cron.
#[utoipa::path(post, path = "/api/crons/run", tag = "crons", params(RunQuery),
    responses((status = 200, body = CronRunResult)))]
#[tracing::instrument(
    name = CRONS_RUN_ALL,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn run_all(
    State(st): State<AppState>,
    Query(q): Query<RunQuery>,
) -> AppResult<Json<CronRunResult>> {
    Ok(Json(sure_dal::crons::run_all(&st.db, q.to.as_deref()).await?))
}

/// A cron's run history.
#[utoipa::path(get, path = "/api/crons/{id}/runs", tag = "crons", params(("id" = i64, Path,)),
    responses((status = 200, body = [CronRun])))]
#[tracing::instrument(
    name = CRONS_LIST_RUNS,
    level = "debug",
    skip_all,
    fields(cron_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_runs(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<CronRun>>> {
    Ok(Json(sure_dal::crons::list_runs(&st.db, id).await?))
}

/// Undo a single applied period: delete the artifact it produced and roll the
/// cron's watermark back to the previous applied period.
#[utoipa::path(post, path = "/api/crons/runs/{run_id}/undo", tag = "crons", params(("run_id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = CRONS_UNDO_RUN,
    level = "debug",
    skip_all,
    fields(run_id = %run_id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn undo_run(State(st): State<AppState>, Path(run_id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::crons::undo_run(&st.db, run_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/crons", get(list).post(create))
        .route("/crons/run", post(run_all))
        .route("/crons/runs/{run_id}/undo", post(undo_run))
        .route("/crons/{id}", axum::routing::put(update).delete(delete))
        .route("/crons/{id}/run", post(run_one))
        .route("/crons/{id}/runs", get(list_runs))
}
