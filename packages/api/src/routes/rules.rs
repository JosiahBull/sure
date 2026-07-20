//! Rule HTTP handlers. The `zen-expression` evaluation engine, orchestration, and CRUD
//! all go through `sure_app::rules::RuleService` (`st.rules`) now, backed by the
//! `RuleRepo` port. These handlers are thin glue.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_core::{
    PreviewMatch, PreviewRequest, Rule, RuleApplicationDetail, RulePreview, RuleRun, RunResult,
    SaveRule,
};

// OTEL span names for this module's handlers.
const RULES_LIST: &str = "rules.list";
const RULES_GET: &str = "rules.get";
const RULES_CREATE: &str = "rules.create";
const RULES_UPDATE: &str = "rules.update";
const RULES_DELETE: &str = "rules.delete";
const RULES_RUN_ONE: &str = "rules.run_one";
const RULES_RUN_ALL: &str = "rules.run_all";
const RULES_PREVIEW: &str = "rules.preview";
const RULES_LIST_RUNS: &str = "rules.list_runs";
const RULES_RUN_APPLICATIONS: &str = "rules.run_applications";
const RULES_UNDO_RUN: &str = "rules.undo_run";

// ---- handlers ------------------------------------------------------------

/// List rules in evaluation order.
#[utoipa::path(get, path = "/api/rules", tag = "rules", responses((status = 200, body = [Rule])))]
#[tracing::instrument(
    name = RULES_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Rule>>> {
    Ok(Json(st.rules.list().await?))
}

/// Fetch one rule.
#[utoipa::path(get, path = "/api/rules/{id}", tag = "rules", params(("id" = i64, Path,)),
    responses((status = 200, body = Rule), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_GET,
    level = "debug",
    skip_all,
    fields(rule_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_one(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<Rule>> {
    Ok(Json(st.rules.get(id).await?))
}

/// Create a rule (validates the expression).
#[utoipa::path(post, path = "/api/rules", tag = "rules", request_body = SaveRule,
    responses((status = 201, body = Rule), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveRule>,
) -> AppResult<(StatusCode, Json<Rule>)> {
    sure_app::rules::validate_rule(&input)?;
    Ok((StatusCode::CREATED, Json(st.rules.create(input).await?)))
}

/// Replace a rule.
#[utoipa::path(put, path = "/api/rules/{id}", tag = "rules", params(("id" = i64, Path,)),
    request_body = SaveRule,
    responses((status = 200, body = Rule), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_UPDATE,
    level = "debug",
    skip_all,
    fields(rule_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveRule>,
) -> AppResult<Json<Rule>> {
    sure_app::rules::validate_rule(&input)?;
    Ok(Json(st.rules.update(id, input).await?))
}

/// Delete a rule (audit history is retained; its rule_id becomes null).
#[utoipa::path(delete, path = "/api/rules/{id}", tag = "rules", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_DELETE,
    level = "debug",
    skip_all,
    fields(rule_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.rules.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Run a single rule over all transactions.
#[utoipa::path(post, path = "/api/rules/{id}/run", tag = "rules", params(("id" = i64, Path,)),
    responses((status = 200, body = RunResult), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_RUN_ONE,
    level = "debug",
    skip_all,
    fields(rule_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn run_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<RunResult>> {
    let rule = st.rules.get(id).await?;
    let result = st.rules.run(&[rule], Some(id), "single").await?;
    Ok(Json(result))
}

/// Run all enabled rules in priority order.
#[utoipa::path(post, path = "/api/rules/run", tag = "rules",
    responses((status = 200, body = RunResult)))]
#[tracing::instrument(
    name = RULES_RUN_ALL,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn run_all(State(st): State<AppState>) -> AppResult<Json<RunResult>> {
    let rules = st.rules.enabled_rules().await?;
    let result = st.rules.run(&rules, None, "all").await?;
    Ok(Json(result))
}

/// Preview which transactions an expression would match, without changing anything.
#[utoipa::path(post, path = "/api/rules/preview", tag = "rules", request_body = PreviewRequest,
    responses((status = 200, body = RulePreview), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_PREVIEW,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn preview(
    State(st): State<AppState>,
    Json(req): Json<PreviewRequest>,
) -> AppResult<Json<RulePreview>> {
    Ok(Json(st.rules.preview(&req).await?))
}

/// List rule runs (most recent first) — the audit trail.
#[utoipa::path(get, path = "/api/rules/runs", tag = "rules", responses((status = 200, body = [RuleRun])))]
#[tracing::instrument(
    name = RULES_LIST_RUNS,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_runs(State(st): State<AppState>) -> AppResult<Json<Vec<RuleRun>>> {
    Ok(Json(st.rules.list_runs().await?))
}

/// List the per-transaction changes made by a run (with transaction detail for display).
#[utoipa::path(get, path = "/api/rules/runs/{run_id}", tag = "rules", params(("run_id" = i64, Path,)),
    responses((status = 200, body = [RuleApplicationDetail])))]
#[tracing::instrument(
    name = RULES_RUN_APPLICATIONS,
    level = "debug",
    skip_all,
    fields(run_id = %run_id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn run_applications(
    State(st): State<AppState>,
    Path(run_id): Path<i64>,
) -> AppResult<Json<Vec<RuleApplicationDetail>>> {
    Ok(Json(st.rules.run_applications(run_id).await?))
}

/// Undo a run, reverting each changed transaction to its prior state (unless it was
/// changed again since).
#[utoipa::path(post, path = "/api/rules/runs/{run_id}/undo", tag = "rules",
    params(("run_id" = i64, Path,)),
    responses((status = 200, body = RunResult), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = RULES_UNDO_RUN,
    level = "debug",
    skip_all,
    fields(run_id = %run_id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn undo_run(
    State(st): State<AppState>,
    Path(run_id): Path<i64>,
) -> AppResult<Json<RunResult>> {
    Ok(Json(st.rules.undo_run(run_id).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/rules", get(list).post(create))
        .route("/rules/run", post(run_all))
        .route("/rules/preview", post(preview))
        .route("/rules/runs", get(list_runs))
        .route("/rules/runs/{run_id}", get(run_applications))
        .route("/rules/runs/{run_id}/undo", post(undo_run))
        .route("/rules/{id}", get(get_one).put(update).delete(delete))
        .route("/rules/{id}/run", post(run_one))
}
