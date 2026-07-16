//! Rule HTTP handlers plus the Zen-expression evaluation engine. Persistence — rule
//! CRUD, loading evaluation contexts, and writing/undoing a run's audit trail — lives
//! in `sure_dal::rules`; this module owns only the `zen-expression` evaluation and the
//! orchestration that turns matches into the changes the DAL then persists.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub use sure_dal::rules::{
    PreviewMatch, PreviewRequest, Rule, RuleApplicationDetail, RulePreview, RuleRun, RunResult,
    SaveRule,
};
use sure_dal::rules::{PlannedApplication, TxCtx};

/// Mutable per-transaction state, updated as successive rules apply within a run.
struct Current {
    category_id: Option<i64>,
    categorized_by_rule_id: Option<i64>,
    is_one_off: bool,
    merchant_id: Option<i64>,
}

impl Current {
    fn of(row: &TxCtx) -> Self {
        Current {
            category_id: row.category_id,
            categorized_by_rule_id: row.categorized_by_rule_id,
            is_one_off: row.is_one_off,
            merchant_id: row.merchant_id,
        }
    }
}

// ---- handlers ------------------------------------------------------------

/// List rules in evaluation order.
#[utoipa::path(get, path = "/api/rules", tag = "rules", responses((status = 200, body = [Rule])))]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Rule>>> {
    Ok(Json(sure_dal::rules::list(&st.db).await?))
}

/// Fetch one rule.
#[utoipa::path(get, path = "/api/rules/{id}", tag = "rules", params(("id" = i64, Path,)),
    responses((status = 200, body = Rule), (status = 404, body = crate::error::ErrorBody)))]
pub async fn get_one(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<Rule>> {
    Ok(Json(sure_dal::rules::get(&st.db, id).await?))
}

/// Create a rule (validates the expression).
#[utoipa::path(post, path = "/api/rules", tag = "rules", request_body = SaveRule,
    responses((status = 201, body = Rule), (status = 422, body = crate::error::ErrorBody)))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveRule>,
) -> AppResult<(StatusCode, Json<Rule>)> {
    validate_rule(&input)?;
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::rules::create(&st.db, input).await?),
    ))
}

/// Replace a rule.
#[utoipa::path(put, path = "/api/rules/{id}", tag = "rules", params(("id" = i64, Path,)),
    request_body = SaveRule,
    responses((status = 200, body = Rule), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveRule>,
) -> AppResult<Json<Rule>> {
    validate_rule(&input)?;
    Ok(Json(sure_dal::rules::update(&st.db, id, input).await?))
}

/// Delete a rule (audit history is retained; its rule_id becomes null).
#[utoipa::path(delete, path = "/api/rules/{id}", tag = "rules", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::rules::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Run a single rule over all transactions.
#[utoipa::path(post, path = "/api/rules/{id}/run", tag = "rules", params(("id" = i64, Path,)),
    responses((status = 200, body = RunResult), (status = 404, body = crate::error::ErrorBody)))]
pub async fn run_one(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<RunResult>> {
    let rule = sure_dal::rules::get(&st.db, id).await?;
    let result = run_rules(&st.db, &[rule], Some(id), "single").await?;
    Ok(Json(result))
}

/// Run all enabled rules in priority order.
#[utoipa::path(post, path = "/api/rules/run", tag = "rules",
    responses((status = 200, body = RunResult)))]
pub async fn run_all(State(st): State<AppState>) -> AppResult<Json<RunResult>> {
    let rules = sure_dal::rules::enabled_rules(&st.db).await?;
    let result = run_rules(&st.db, &rules, None, "all").await?;
    Ok(Json(result))
}

/// Preview which transactions an expression would match, without changing anything.
#[utoipa::path(post, path = "/api/rules/preview", tag = "rules", request_body = PreviewRequest,
    responses((status = 200, body = RulePreview), (status = 422, body = crate::error::ErrorBody)))]
pub async fn preview(
    State(st): State<AppState>,
    Json(req): Json<PreviewRequest>,
) -> AppResult<Json<RulePreview>> {
    validate_expression(&req.expression)?;
    let limit = req.limit.unwrap_or(25).clamp(1, 500) as usize;
    let rows = sure_dal::rules::load_contexts(&st.db).await?;
    let mut matched = 0i64;
    let mut sample = Vec::new();
    for row in &rows {
        let cur = Current::of(row);
        if expr_matches(&req.expression, &build_context(row, &cur)) {
            matched += 1;
            if sample.len() < limit {
                sample.push(PreviewMatch {
                    transaction_id: row.id,
                    posted_at: row.posted_at.clone(),
                    description: row.description.clone(),
                    amount_minor: row.amount_minor,
                    currency_code: row.currency_code.clone(),
                    category_id: row.category_id,
                });
            }
        }
    }
    Ok(Json(RulePreview { matched, sample }))
}

/// List rule runs (most recent first) — the audit trail.
#[utoipa::path(get, path = "/api/rules/runs", tag = "rules", responses((status = 200, body = [RuleRun])))]
pub async fn list_runs(State(st): State<AppState>) -> AppResult<Json<Vec<RuleRun>>> {
    Ok(Json(sure_dal::rules::list_runs(&st.db).await?))
}

/// List the per-transaction changes made by a run (with transaction detail for display).
#[utoipa::path(get, path = "/api/rules/runs/{run_id}", tag = "rules", params(("run_id" = i64, Path,)),
    responses((status = 200, body = [RuleApplicationDetail])))]
pub async fn run_applications(
    State(st): State<AppState>,
    Path(run_id): Path<i64>,
) -> AppResult<Json<Vec<RuleApplicationDetail>>> {
    Ok(Json(sure_dal::rules::run_applications(&st.db, run_id).await?))
}

/// Undo a run, reverting each changed transaction to its prior state (unless it was
/// changed again since).
#[utoipa::path(post, path = "/api/rules/runs/{run_id}/undo", tag = "rules",
    params(("run_id" = i64, Path,)),
    responses((status = 200, body = RunResult), (status = 404, body = crate::error::ErrorBody)))]
pub async fn undo_run(
    State(st): State<AppState>,
    Path(run_id): Path<i64>,
) -> AppResult<Json<RunResult>> {
    Ok(Json(sure_dal::rules::undo_run(&st.db, run_id).await?))
}

// ---- engine --------------------------------------------------------------

/// Evaluate `rules` over every transaction, then persist the decided changes. The
/// evaluation (which fields matched, what a rule would set) is done here; the DAL
/// writes the run, the transaction updates, and the audit rows in one transaction.
async fn run_rules(
    db: &sure_dal::Db,
    rules: &[Rule],
    rule_id: Option<i64>,
    kind: &str,
) -> AppResult<RunResult> {
    let rows = sure_dal::rules::load_contexts(db).await?;
    let mut matched = 0i64;
    let mut applications = Vec::new();

    for row in &rows {
        let mut cur = Current::of(row);
        for rule in rules {
            if !rule.enabled {
                continue;
            }
            if !expr_matches(&rule.expression, &build_context(row, &cur)) {
                continue;
            }
            matched += 1;

            // A category is "manual" when it's set but not by a rule.
            let manual = cur.categorized_by_rule_id.is_none() && cur.category_id.is_some();
            let mut new_category = cur.category_id;
            let mut cat_changed = false;
            if let Some(target) = rule.set_category_id {
                if (!manual || rule.overwrite_manual) && cur.category_id != Some(target) {
                    new_category = Some(target);
                    cat_changed = true;
                }
            }
            let mut new_one_off = cur.is_one_off;
            let mut one_off_changed = false;
            if let Some(v) = rule.set_one_off {
                if cur.is_one_off != v {
                    new_one_off = v;
                    one_off_changed = true;
                }
            }
            let mut new_merchant = cur.merchant_id;
            let mut merchant_changed = false;
            if let Some(m) = rule.set_merchant_id {
                if cur.merchant_id != Some(m) {
                    new_merchant = Some(m);
                    merchant_changed = true;
                }
            }

            if cat_changed || one_off_changed || merchant_changed {
                let new_cat_by_rule = if cat_changed {
                    Some(rule.id)
                } else {
                    cur.categorized_by_rule_id
                };
                applications.push(PlannedApplication {
                    rule_id: rule.id,
                    transaction_id: row.id,
                    prev_category_id: cur.category_id,
                    new_category_id: new_category,
                    prev_categorized_by_rule_id: cur.categorized_by_rule_id,
                    new_categorized_by_rule_id: new_cat_by_rule,
                    prev_one_off: cur.is_one_off,
                    new_one_off,
                    prev_merchant_id: cur.merchant_id,
                    new_merchant_id: new_merchant,
                });
                cur.category_id = new_category;
                cur.categorized_by_rule_id = new_cat_by_rule;
                cur.is_one_off = new_one_off;
                cur.merchant_id = new_merchant;
            }

            if rule.stop_on_match {
                break;
            }
        }
    }

    sure_dal::rules::persist_run(db, rule_id, kind, matched, applications).await
}

fn build_context(row: &TxCtx, cur: &Current) -> Value {
    let amount = row.amount_minor as f64 / 10f64.powi(row.decimal_places as i32);
    let mut obj = serde_json::Map::new();
    obj.insert("amount".into(), json!(amount));
    obj.insert("amount_minor".into(), json!(row.amount_minor));
    obj.insert("abs_amount".into(), json!(amount.abs()));
    obj.insert("is_income".into(), json!(row.amount_minor > 0));
    obj.insert("is_expense".into(), json!(row.amount_minor < 0));
    obj.insert("description".into(), json!(row.description));
    obj.insert(
        "merchant".into(),
        json!(row.merchant.clone().unwrap_or_default()),
    );
    obj.insert(
        "merchant_id".into(),
        cur.merchant_id.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    obj.insert("notes".into(), json!(row.notes.clone().unwrap_or_default()));
    obj.insert("currency".into(), json!(row.currency_code));
    obj.insert("account_id".into(), json!(row.account_id));
    obj.insert("account".into(), json!(row.account_name));
    obj.insert("account_kind".into(), json!(row.account_kind));
    obj.insert(
        "category_id".into(),
        cur.category_id.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    obj.insert("is_one_off".into(), json!(cur.is_one_off));
    obj.insert("date".into(), json!(row.posted_at));
    if let Some(y) = row.posted_at.get(0..4).and_then(|s| s.parse::<i64>().ok()) {
        obj.insert("year".into(), json!(y));
    }
    if let Some(m) = row.posted_at.get(5..7).and_then(|s| s.parse::<i64>().ok()) {
        obj.insert("month".into(), json!(m));
    }
    if let Some(d) = row.posted_at.get(8..10).and_then(|s| s.parse::<i64>().ok()) {
        obj.insert("day".into(), json!(d));
    }
    Value::Object(obj)
}

/// Evaluate an expression against a context, treating any error or non-boolean
/// result as "no match".
fn expr_matches(expression: &str, ctx: &Value) -> bool {
    zen_expression::evaluate_expression(expression, ctx.clone().into())
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn validate_rule(input: &SaveRule) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("rule name is required"));
    }
    validate_expression(&input.expression)
}

/// Ensure the expression parses by evaluating it against a representative context.
fn validate_expression(expression: &str) -> AppResult<()> {
    if expression.trim().is_empty() {
        return Err(AppError::validation("expression is required"));
    }
    let sample = json!({
        "amount": -12.5, "amount_minor": -1250, "abs_amount": 12.5,
        "is_income": false, "is_expense": true,
        "description": "sample", "merchant": "sample", "merchant_id": null, "notes": "",
        "currency": "NZD", "account_id": 1, "account": "sample", "account_kind": "bank",
        "category_id": null, "is_one_off": false,
        "date": "2026-01-01", "year": 2026, "month": 1, "day": 1
    });
    zen_expression::evaluate_expression(expression.trim(), sample.into())
        .map_err(|e| AppError::validation(format!("invalid expression: {e}")))?;
    Ok(())
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
