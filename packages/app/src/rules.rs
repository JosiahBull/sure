//! Auto-classification rule engine: the `zen-expression` evaluation loop that decides,
//! for each transaction, whether a rule matches and what it would change, plus the
//! validation of a rule's expression. Persistence — rule CRUD, loading evaluation
//! contexts, and writing/undoing a run's audit trail — lives behind the [`RuleRepo`]
//! port (`sure_dal::rules` is the real implementation); this module owns only the
//! evaluation and the orchestration that turns matches into the changes the DAL then
//! persists.

use std::sync::Arc;

use serde_json::{json, Value};

use sure_core::{
    AppError, AppResult, PreviewMatch, PreviewRequest, Rule, RulePreview, RuleRunKind, RunResult,
    SaveRule,
};

use crate::ports::{PlannedApplication, RuleRepo, TxCtx};

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

pub struct RuleService {
    rules: Arc<dyn RuleRepo>,
}

impl RuleService {
    pub fn new(rules: Arc<dyn RuleRepo>) -> Self {
        Self { rules }
    }

    // ---- thin CRUD passthrough — no orchestration, so it lives directly on the repo
    // port rather than duplicating logic here; kept on RuleService so routes/rules.rs
    // has one handle for everything rule-related. ----

    pub async fn list(&self) -> AppResult<Vec<Rule>> {
        self.rules.list().await
    }

    pub async fn enabled_rules(&self) -> AppResult<Vec<Rule>> {
        self.rules.enabled_rules().await
    }

    pub async fn get(&self, id: i64) -> AppResult<Rule> {
        self.rules.get(id).await
    }

    pub async fn create(&self, input: SaveRule) -> AppResult<Rule> {
        self.rules.create(input).await
    }

    pub async fn update(&self, id: i64, input: SaveRule) -> AppResult<Rule> {
        self.rules.update(id, input).await
    }

    pub async fn delete(&self, id: i64) -> AppResult<()> {
        self.rules.delete(id).await
    }

    pub async fn list_runs(&self) -> AppResult<Vec<sure_core::RuleRun>> {
        self.rules.list_runs().await
    }

    pub async fn run_applications(
        &self,
        run_id: i64,
    ) -> AppResult<Vec<sure_core::RuleApplicationDetail>> {
        self.rules.run_applications(run_id).await
    }

    pub async fn undo_run(&self, run_id: i64) -> AppResult<RunResult> {
        self.rules.undo_run(run_id).await
    }

    /// Evaluate `rules` over every transaction, then persist the decided changes. The
    /// evaluation (which fields matched, what a rule would set) is done here; the repo
    /// writes the run, the transaction updates, and the audit rows in one transaction.
    pub async fn run(
        &self,
        rules: &[Rule],
        rule_id: Option<i64>,
        kind: RuleRunKind,
    ) -> AppResult<RunResult> {
        let rows = self.rules.load_contexts().await?;
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

        self.rules
            .persist_run(rule_id, kind, matched, applications)
            .await
    }

    /// Preview which transactions an expression would match, without changing anything.
    pub async fn preview(&self, req: &PreviewRequest) -> AppResult<RulePreview> {
        validate_expression(&req.expression)?;
        let limit = req.limit.unwrap_or(25).clamp(1, 500) as usize;
        let rows = self.rules.load_contexts().await?;
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
    // The Zen engine only understands plain JSON values, so this is the deliberate
    // exception (CLAUDE.md rule 1) where `AccountKind` is rendered to its wire string
    // rather than staying the enum — an external expression-evaluator payload, not
    // domain storage, and it happens exactly once, right here.
    obj.insert("account_kind".into(), json!(row.account_kind.as_str()));
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

/// Hard ceiling on an expression's size. `zen-expression`'s parser recurses once per
/// nesting level and has no depth guard of its own, so a body that is trivially small by
/// HTTP standards can still bury a tokio worker's 2 MiB stack — and a Rust stack overflow
/// `abort()`s the process rather than panicking, so `CatchPanicLayer` never sees it: every
/// in-flight request dies, the WAL is left unchecked, and `sure-appbase`'s drain never
/// runs. 16 KiB is already two orders of magnitude past the longest honest rule over the
/// ~20 documented context fields, so nothing a person writes ever meets it.
const MAX_EXPRESSION_BYTES: usize = 16 * 1024;

/// Hard ceiling on bracket-nesting depth, measured in one non-recursive pass before the
/// parser is handed the string. [`MAX_EXPRESSION_BYTES`] alone does not close the hole:
/// `[[[[1]]]]` nested 40 000 deep is only 80 KB, 4% of the stack cap, and reliably
/// overflows a 2 MiB worker in a *release* build. A real rule nests a handful of levels —
/// a couple of `and`/`or` groups around a `contains(...)` call — so 64 leaves enormous
/// headroom while keeping the recursion depth the parser reaches trivially small.
const MAX_EXPRESSION_DEPTH: usize = 64;

/// Deepest run of unclosed `(`/`[`/`{` in `expression`, counted in a single pass over the
/// bytes — the point is to bound the parser's recursion *without* recursing ourselves.
///
/// Brackets inside string literals are counted too. That is a deliberate
/// over-approximation: tracking quoting would mean half a lexer, and no honest rule puts
/// 64 unclosed brackets inside a quoted merchant name.
fn max_bracket_depth(expression: &str) -> usize {
    let mut depth = 0usize;
    let mut deepest = 0usize;
    for &b in expression.as_bytes() {
        if matches!(b, b'(' | b'[' | b'{') {
            depth += 1;
            deepest = deepest.max(depth);
        } else if matches!(b, b')' | b']' | b'}') {
            // Saturating: an unbalanced closer is the parser's error to report, not ours.
            depth = depth.saturating_sub(1);
        }
    }
    deepest
}

/// Ensure the expression parses by evaluating it against a representative context.
///
/// The two structural limits are checked *first*, before `zen_expression` ever sees the
/// string: past them the parser does not return an error, it kills the process (see
/// [`MAX_EXPRESSION_BYTES`]). This is the only gate in front of the parser for all three
/// entry points — `POST /api/rules`, `PUT /api/rules/{id}` (both via [`validate_rule`])
/// and `POST /api/rules/preview` (via [`RuleService::preview`]) — so it has to hold for
/// unstored expressions too, not just ones on their way into the database.
pub fn validate_expression(expression: &str) -> AppResult<()> {
    if expression.trim().is_empty() {
        return Err(AppError::validation("expression is required"));
    }
    if expression.len() > MAX_EXPRESSION_BYTES {
        return Err(AppError::validation(format!(
            "expression is too long ({} bytes, limit {MAX_EXPRESSION_BYTES})",
            expression.len()
        )));
    }
    let depth = max_bracket_depth(expression);
    if depth > MAX_EXPRESSION_DEPTH {
        return Err(AppError::validation(format!(
            "expression nesting is too deep ({depth} levels, limit {MAX_EXPRESSION_DEPTH})"
        )));
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

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use sure_core::{AccountKind, RuleApplicationDetail, RuleRun};

    use super::*;

    fn ctx(id: i64, amount_minor: i64, category_id: Option<i64>) -> TxCtx {
        TxCtx {
            id,
            account_id: 1,
            posted_at: "2026-01-05".to_string(),
            amount_minor,
            currency_code: "NZD".to_string(),
            decimal_places: 2,
            description: "Flat White".to_string(),
            merchant: Some("The Roastery".to_string()),
            merchant_id: None,
            notes: None,
            category_id,
            is_one_off: false,
            categorized_by_rule_id: None,
            account_name: "Everyday".to_string(),
            account_kind: AccountKind::Bank,
        }
    }

    fn rule(id: i64, expression: &str, set_category_id: i64) -> Rule {
        Rule {
            id,
            name: "categorize coffee".to_string(),
            description: None,
            expression: expression.to_string(),
            set_category_id: Some(set_category_id),
            set_one_off: None,
            set_merchant_id: None,
            overwrite_manual: false,
            stop_on_match: true,
            priority: 0,
            enabled: true,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        }
    }

    /// Records exactly what `RuleService::run` decided to persist, without a database.
    #[derive(Default)]
    struct FakeRules {
        contexts: Vec<TxCtx>,
    }
    #[async_trait]
    impl RuleRepo for FakeRules {
        async fn load_contexts(&self) -> AppResult<Vec<TxCtx>> {
            Ok(self.contexts.clone())
        }
        async fn persist_run(
            &self,
            rule_id: Option<i64>,
            _kind: RuleRunKind,
            matched: i64,
            applications: Vec<PlannedApplication>,
        ) -> AppResult<RunResult> {
            Ok(RunResult {
                run_id: rule_id.unwrap_or(0),
                matched,
                changed: applications.len() as i64,
            })
        }
        async fn list(&self) -> AppResult<Vec<Rule>> {
            unreachable!("RuleService.run/preview never list rules")
        }
        async fn enabled_rules(&self) -> AppResult<Vec<Rule>> {
            unreachable!("RuleService.run/preview never list rules")
        }
        async fn get(&self, _id: i64) -> AppResult<Rule> {
            unreachable!("RuleService.run/preview never fetch a rule by id")
        }
        async fn create(&self, _input: SaveRule) -> AppResult<Rule> {
            unreachable!("RuleService.run/preview never create a rule")
        }
        async fn update(&self, _id: i64, _input: SaveRule) -> AppResult<Rule> {
            unreachable!("RuleService.run/preview never update a rule")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("RuleService.run/preview never delete a rule")
        }
        async fn list_runs(&self) -> AppResult<Vec<RuleRun>> {
            unreachable!("RuleService.run/preview never list runs")
        }
        async fn run_applications(&self, _run_id: i64) -> AppResult<Vec<RuleApplicationDetail>> {
            unreachable!("RuleService.run/preview never list run applications")
        }
        async fn undo_run(&self, _run_id: i64) -> AppResult<RunResult> {
            unreachable!("RuleService.run/preview never undo a run")
        }
    }

    #[tokio::test]
    async fn a_matching_rule_categorizes_an_uncategorized_transaction() {
        let repo = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, None), ctx(2, 500, None)],
        });
        let svc = RuleService::new(repo);
        let r = rule(1, "merchant == \"The Roastery\"", 42);

        let result = svc.run(&[r], Some(1), RuleRunKind::Single).await.unwrap();

        assert_eq!(result.matched, 2); // both rows share the same merchant
        assert_eq!(result.changed, 2); // both were uncategorized, so both changed
    }

    #[tokio::test]
    async fn a_manual_category_is_protected_unless_overwrite_is_set() {
        let repo = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, Some(7))], // manually categorized already
        });
        let svc = RuleService::new(repo);
        let mut r = rule(1, "merchant == \"The Roastery\"", 42);
        r.overwrite_manual = false;

        let result = svc.run(&[r], Some(1), RuleRunKind::Single).await.unwrap();

        assert_eq!(result.matched, 1);
        assert_eq!(result.changed, 0); // manual category left untouched
    }

    #[test]
    fn an_empty_expression_is_rejected() {
        assert!(validate_expression("").is_err());
    }

    #[test]
    fn a_syntactically_invalid_expression_is_rejected() {
        assert!(validate_expression("this is not zen-expression syntax +++").is_err());
    }

    /// Asserts the message, not just the `is_err()`, because the whole point is that the
    /// *structural* guard fired: had this reached `zen_expression`, the parser would have
    /// recursed 40 000 frames deep and aborted the process instead of returning at all.
    #[test]
    fn a_deeply_nested_expression_is_rejected_before_the_parser_sees_it() {
        let deep = format!("{}1{}", "[".repeat(40_000), "]".repeat(40_000));
        let err = validate_expression(&deep).expect_err("40 000 levels must be rejected");
        if let AppError::Validation(msg) = &err {
            assert!(
                msg.contains("too long") || msg.contains("too deep"),
                "expected a structural rejection, got: {msg}"
            );
            assert!(
                !msg.contains("invalid expression"),
                "the parser was reached: {msg}"
            );
        } else {
            panic!("expected a validation error, got: {err:?}");
        }
    }

    /// The depth guard has to stand on its own: this body is well under
    /// `MAX_EXPRESSION_BYTES`, so only the nesting check can catch it.
    #[test]
    fn nesting_alone_is_rejected_even_within_the_byte_ceiling() {
        let nested = "[".repeat(4_000);
        assert!(nested.len() < MAX_EXPRESSION_BYTES);
        let err = validate_expression(&nested).expect_err("4 000 levels must be rejected");
        if let AppError::Validation(msg) = &err {
            assert!(msg.contains("too deep"), "expected the depth guard: {msg}");
        } else {
            panic!("expected a validation error, got: {err:?}");
        }
    }

    #[test]
    fn an_expression_over_the_byte_ceiling_is_rejected() {
        // Flat, unnested, and syntactically fine — only the size limit can reject it.
        let long = format!("description == \"{}\"", "x".repeat(MAX_EXPRESSION_BYTES));
        let err = validate_expression(&long).expect_err("an oversize expression must be rejected");
        if let AppError::Validation(msg) = &err {
            assert!(msg.contains("too long"), "expected the size guard: {msg}");
        } else {
            panic!("expected a validation error, got: {err:?}");
        }
    }

    #[test]
    fn an_ordinary_rule_expression_still_validates() {
        validate_expression(
            "is_expense and (contains(lower(description),'countdown') or abs_amount > 40)",
        )
        .expect("a realistic rule must still be accepted");
    }

    #[test]
    fn a_modestly_nested_expression_still_validates() {
        // Depth 10 — deeper than any rule in the UI, comfortably inside the ceiling.
        let nested = format!("{}amount_minor < 0{}", "(".repeat(10), ")".repeat(10));
        assert_eq!(max_bracket_depth(&nested), 10);
        validate_expression(&nested).expect("ten levels of grouping must still be accepted");
    }
}
