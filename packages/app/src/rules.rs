//! Auto-classification rule engine: the `zen-expression` evaluation loop that decides,
//! for each transaction, whether a rule matches and what it would change, plus the
//! validation of a rule's expression. Persistence — rule CRUD, loading evaluation
//! contexts, and writing/undoing a run's audit trail — lives in `sure_dal::rules`; this
//! module owns only the evaluation and the orchestration that turns matches into the
//! changes the DAL then persists.

use serde_json::{json, Value};

use sure_core::{
    AppError, AppResult, PreviewMatch, PreviewRequest, Rule, RulePreview, RunResult, SaveRule,
};
use sure_dal::rules::{PlannedApplication, TxCtx};
use sure_dal::Db;

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

/// Evaluate `rules` over every transaction, then persist the decided changes. The
/// evaluation (which fields matched, what a rule would set) is done here; the DAL
/// writes the run, the transaction updates, and the audit rows in one transaction.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn run(
    db: &Db,
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

/// Preview which transactions an expression would match, without changing anything.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn preview(db: &Db, req: &PreviewRequest) -> AppResult<RulePreview> {
    validate_expression(&req.expression)?;
    let limit = req.limit.unwrap_or(25).clamp(1, 500) as usize;
    let rows = sure_dal::rules::load_contexts(db).await?;
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
    Ok(RulePreview { matched, sample })
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

pub fn validate_rule(input: &SaveRule) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("rule name is required"));
    }
    validate_expression(&input.expression)
}

/// Ensure the expression parses by evaluating it against a representative context.
pub fn validate_expression(expression: &str) -> AppResult<()> {
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
