//! Auto-classification rule engine: the `zen-expression` evaluation loop that decides,
//! for each transaction, whether a rule matches and what it would change, plus the
//! validation of a rule's expression. Persistence — rule CRUD, loading evaluation
//! contexts, and writing/undoing a run's audit trail — lives behind the [`RuleRepo`]
//! port (`sure_dal::rules` is the real implementation); this module owns only the
//! evaluation and the orchestration that turns matches into the changes the DAL then
//! persists.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use zen_expression::{Expression, expression::Standard, vm::VM};

use sure_core::{
    AppError, AppResult, PreviewMatch, PreviewRequest, Rule, RulePreview, RuleRunKind, RunResult,
    SaveRule,
};

use crate::ports::{PlannedApplication, RuleRepo, TxCtx};

/// Classifying whatever has just landed — the only thing an import or a provider sync asks
/// of the rule engine.
///
/// A trait rather than an `Arc<RuleService>` for the usual reason: [`crate::import::ImportService`]
/// and [`crate::sync::SyncService`] are tested against in-memory fakes, and taking the concrete
/// service would make every one of those tests build a twelve-method [`RuleRepo`] fake to
/// exercise a step they don't care about. The implementation that matters is
/// [`RuleService::categorize_new`].
#[async_trait]
pub trait AutoCategorize: Send + Sync {
    /// Apply the enabled rules to everything still uncategorised. `Ok(None)` means there was
    /// nothing to do and nothing was written.
    async fn categorize_new(&self) -> AppResult<Option<RunResult>>;
}

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

#[async_trait]
impl AutoCategorize for RuleService {
    async fn categorize_new(&self) -> AppResult<Option<RunResult>> {
        RuleService::categorize_new(self).await
    }
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
    /// evaluation (which fields matched, what a rule would set) is done by [`plan_run`];
    /// the repo writes the run, the transaction updates, and the audit rows in one
    /// transaction.
    pub async fn run(
        &self,
        rules: &[Rule],
        rule_id: Option<i64>,
        kind: RuleRunKind,
    ) -> AppResult<RunResult> {
        // `kind` is `RuleRunKind`, a closed enum, so the label set is fixed.
        let _timer = sure_telemetry::instruments::Timer::new(
            &sure_telemetry::instruments().rules_run_duration,
            vec![sure_telemetry::KeyValue::new("kind", kind.as_str())],
        );
        let rows = self.rules.load_contexts().await?;
        let (matched, applications) = plan_run(rules, &rows);

        let run = self
            .rules
            .persist_run(rule_id, kind, matched, applications)
            .await?;
        // `matched` is how many transactions the rule set claimed; `changed` how many it
        // actually rewrote. The gap between them is the interesting number — a rule set that
        // matches thousands and changes nothing is doing no work.
        let instruments = sure_telemetry::instruments();
        for (disposition, count) in [("matched", run.matched), ("changed", run.changed)] {
            if count > 0 {
                instruments.rules_run_rows.add(
                    u64::try_from(count).unwrap_or(0),
                    &[
                        sure_telemetry::KeyValue::new("kind", kind.as_str()),
                        sure_telemetry::KeyValue::new("disposition", disposition),
                    ],
                );
            }
        }
        Ok(run)
    }

    /// Apply every enabled rule to the transactions that have no category yet.
    ///
    /// The unattended counterpart to [`Self::run`], made after an import or a provider sync
    /// puts new rows on the ledger — rules used to apply only when someone pressed "run", so
    /// an untouched install accumulated uncategorised transactions indefinitely while a
    /// perfectly good rule set sat there matching them.
    ///
    /// Two things separate it from a plain `run` over the enabled set, and both exist because
    /// this one happens without anyone watching:
    ///
    /// - **It only sees uncategorised rows** ([`RuleRepo::load_uncategorized_contexts`]). That
    ///   makes it incapable of replacing a category — a provider's enrichment, a rule's own
    ///   earlier verdict, or a correction someone typed — rather than merely disinclined to.
    ///   `overwrite_manual` would not be enough on its own: it is a per-rule flag, and a rule
    ///   set anyone can edit should not be able to arrange for a background pass to overwrite
    ///   the ledger. It also keeps the cost proportional to the backlog instead of to the
    ///   whole history, which matters when a provider poll syncs a dozen feeds at a time.
    /// - **It writes nothing when it changes nothing.** A run row per sync per provider would
    ///   bury the audit log — the one place a person looks to see what a rule actually did —
    ///   under a standing stream of empty entries.
    ///
    /// Returns `None` in that second case, and otherwise the persisted run, which is undoable
    /// from the rules page like any other.
    pub async fn categorize_new(&self) -> AppResult<Option<RunResult>> {
        let rules = self.rules.enabled_rules().await?;
        if rules.is_empty() {
            return Ok(None);
        }
        let rows = self.rules.load_uncategorized_contexts().await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let (matched, applications) = plan_run(&rules, &rows);
        if applications.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            self.rules
                .persist_run(None, RuleRunKind::Auto, matched, applications)
                .await?,
        ))
    }

    /// Preview which transactions an expression would match, without changing anything.
    pub async fn preview(&self, req: &PreviewRequest) -> AppResult<RulePreview> {
        validate_expression(&req.expression)?;
        let limit = req.limit.unwrap_or(25).clamp(1, 500) as usize;
        let rows = self.rules.load_contexts().await?;
        // Compiled once for the whole scan rather than re-parsed per row: a preview walks
        // every transaction in the ledger, and the expression is the same string each time.
        // `validate_expression` above already lexed, parsed *and* ran this source, so a
        // failure here is unreachable; it is mapped to the identical 400 rather than
        // quietly reporting zero matches.
        let program = zen_expression::compile_expression(&req.expression)
            .map_err(|e| AppError::validation(format!("invalid expression: {e}")))?;
        let mut vm = VM::new();
        let mut matched = 0i64;
        let mut sample = Vec::new();
        for row in &rows {
            let cur = Current::of(row);
            if program_matches(&program, &build_context(row, &cur), &mut vm) {
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

/// A rule paired with its expression compiled to `zen-expression` bytecode exactly once.
///
/// Disabled rules, and rules whose expression will not compile, are absent from the list
/// [`compile_rules`] returns: both match nothing, which is precisely what the row loop used
/// to conclude for them once per transaction.
struct CompiledRule<'a> {
    rule: &'a Rule,
    program: Expression<Standard>,
}

/// Compile every enabled rule's expression up front, preserving the caller's order.
///
/// Order is load-bearing: [`RuleService::run`] is handed rules already sorted by priority,
/// and priority is the only thing deciding which of two matching rules gets to set a
/// category first and which one's `stop_on_match` ends the row — so this must not reorder,
/// dedupe, or compact anything but the rules that can never match.
fn compile_rules(rules: &[Rule]) -> Vec<CompiledRule<'_>> {
    rules
        .iter()
        .filter(|rule| rule.enabled)
        .filter_map(|rule| {
            match zen_expression::compile_expression(&rule.expression) {
                Ok(program) => Some(CompiledRule { rule, program }),
                Err(err) => {
                    // Same verdict as before — an expression that will not parse matched
                    // nothing then and matches nothing now — just reported once per run
                    // instead of silently discarded once per transaction.
                    // `validate_expression` guards every write path, so reaching this means
                    // a row predating that guard, or one written outside the API.
                    tracing::warn!(
                        rule_id = rule.id,
                        error = %err,
                        "rule expression does not compile; the rule will match nothing"
                    );
                    None
                }
            }
        })
        .collect()
}

/// Decide, for every transaction, which rules match and what each would change — the entire
/// synchronous half of a run.
///
/// Split out of [`RuleService::run`] rather than inlined because the reused [`VM`] cannot
/// otherwise exist: it holds `Rc`-backed `Variable`s, so were it in scope across
/// `persist_run`'s await it would land in `run`'s future and make it `!Send`, which no Axum
/// handler can hold. Being a plain function also makes the ordering and overwrite rules
/// testable without a repo fake.
///
/// The reason it compiles first: `POST /api/rules/run` is synchronous and unpaginated, and
/// this loop is R rules × T transactions. Passing expression *source* to the engine per pair
/// meant a household with 200 rules and 50 000 transactions paid 10M parse+compile cycles
/// for 200 distinct expressions; hoisting makes that 200 parses and 10M bytecode runs.
///
/// Two things this cost deliberately is *not*: `zen-expression` compiles to bytecode and has
/// no backtracking matcher, so a crafted expression cannot blow up super-linearly the way a
/// regex can; and rules cannot feed each other unboundedly, because a run makes exactly one
/// forward pass per transaction in priority order. Together with the size and nesting
/// ceilings in [`validate_expression`], what is left here is ordinary work, not a way to kill
/// the process.
fn plan_run(rules: &[Rule], rows: &[TxCtx]) -> (i64, Vec<PlannedApplication>) {
    let compiled = compile_rules(rules);
    // Reused across every evaluation in the run purely for its stack/scope allocations;
    // `VM::run` clears both before each program, so nothing carries over between rules.
    let mut vm = VM::new();
    let mut matched = 0i64;
    let mut applications = Vec::new();

    for row in rows {
        let mut cur = Current::of(row);
        // Built once per row and patched in place when a rule actually changes something,
        // instead of rebuilt (twenty-odd `json!` allocations, every string field cloned)
        // once per rule per row.
        let mut ctx = build_context(row, &cur);
        for CompiledRule { rule, program } in &compiled {
            if !program_matches(program, &ctx, &mut vm) {
                continue;
            }
            matched += 1;

            // A category is "manual" when it's set but not by a rule.
            let manual = cur.categorized_by_rule_id.is_none() && cur.category_id.is_some();
            let mut new_category = cur.category_id;
            let mut cat_changed = false;
            if let Some(target) = rule.set_category_id
                && (!manual || rule.overwrite_manual)
                && cur.category_id != Some(target)
            {
                new_category = Some(target);
                cat_changed = true;
            }
            let mut new_one_off = cur.is_one_off;
            let mut one_off_changed = false;
            if let Some(v) = rule.set_one_off
                && cur.is_one_off != v
            {
                new_one_off = v;
                one_off_changed = true;
            }
            let mut new_merchant = cur.merchant_id;
            let mut merchant_changed = false;
            if let Some(m) = rule.set_merchant_id
                && cur.merchant_id != Some(m)
            {
                new_merchant = Some(m);
                merchant_changed = true;
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
                // The row's context is no longer rebuilt from `cur` on the next iteration,
                // so the fields a rule can move have to be written back here — a later rule
                // keying off `category_id`/`is_one_off`/`merchant_id` must see what this one
                // just did, exactly as it did when every rule got a freshly built map.
                write_current(&mut ctx, &cur);
            }

            if rule.stop_on_match {
                break;
            }
        }
    }

    (matched, applications)
}

/// Build a row's evaluation context. Returns the bare map rather than a [`Value`] so
/// [`plan_run`] can patch the three mutable fields in place between rules; the wrapping
/// `Value::Object` happens once per evaluation, in [`program_matches`].
fn build_context(row: &TxCtx, cur: &Current) -> Map<String, Value> {
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
    obj.insert("notes".into(), json!(row.notes.clone().unwrap_or_default()));
    obj.insert("currency".into(), json!(row.currency_code));
    obj.insert("account_id".into(), json!(row.account_id));
    obj.insert("account".into(), json!(row.account_name));
    // The Zen engine only understands plain JSON values, so this is the deliberate
    // exception (CLAUDE.md rule 1) where `AccountKind` is rendered to its wire string
    // rather than staying the enum — an external expression-evaluator payload, not
    // domain storage, and it happens exactly once, right here.
    obj.insert("account_kind".into(), json!(row.account_kind.as_str()));
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
    write_current(&mut obj, cur);
    obj
}

/// Write the only three context fields a run can change while it is walking a row's rules.
///
/// The single place these keys are produced, called both when [`build_context`] first builds
/// the map and whenever [`plan_run`] patches it after a rule applies — so the initial value
/// and the patched value cannot drift into disagreeing about a key's name or its null shape.
fn write_current(obj: &mut Map<String, Value>, cur: &Current) {
    obj.insert(
        "merchant_id".into(),
        cur.merchant_id.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    obj.insert(
        "category_id".into(),
        cur.category_id.map(|v| json!(v)).unwrap_or(Value::Null),
    );
    obj.insert("is_one_off".into(), json!(cur.is_one_off));
}

/// Evaluate a compiled rule against a context, treating any error or non-boolean result as
/// "no match" — the verdict this engine has always given, kept deliberately: a rule set that
/// quietly starts matching different rows is worse than a run that is slow.
///
/// `vm` is threaded in only to reuse its stack and scope allocations; `VM::run` clears both
/// before each program, so no state crosses between evaluations. The context is converted to
/// a `Variable` afresh every time for the same reason — a `Variable` object is an
/// `Rc<RefCell<..>>` inside, so handing one to two evaluations would share interior-mutable
/// state between them, which is exactly the coupling a per-row-per-rule rebuild never had.
fn program_matches(program: &Expression<Standard>, ctx: &Map<String, Value>, vm: &mut VM) -> bool {
    program
        .evaluate_with(Value::Object(ctx.clone()).into(), vm)
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
        /// What `enabled_rules` answers — only `categorize_new` asks, since `run` is handed
        /// its rules by the caller.
        enabled: Vec<Rule>,
        /// Every `persist_run` this fake was asked to make, so a test can assert that one
        /// did *not* happen as readily as that one did.
        persisted: std::sync::Mutex<Vec<(RuleRunKind, i64)>>,
    }
    #[async_trait]
    impl RuleRepo for FakeRules {
        async fn load_contexts(&self) -> AppResult<Vec<TxCtx>> {
            Ok(self.contexts.clone())
        }
        async fn load_uncategorized_contexts(&self) -> AppResult<Vec<TxCtx>> {
            // The real query's `WHERE category_id IS NULL`, in memory.
            Ok(self
                .contexts
                .iter()
                .filter(|c| c.category_id.is_none())
                .cloned()
                .collect())
        }
        async fn persist_run(
            &self,
            rule_id: Option<i64>,
            kind: RuleRunKind,
            matched: i64,
            applications: Vec<PlannedApplication>,
        ) -> AppResult<RunResult> {
            let changed = applications.len() as i64;
            self.persisted.lock().unwrap().push((kind, changed));
            Ok(RunResult {
                run_id: rule_id.unwrap_or(0),
                matched,
                changed,
            })
        }
        async fn list(&self) -> AppResult<Vec<Rule>> {
            unreachable!("RuleService.run/preview never list rules")
        }
        async fn enabled_rules(&self) -> AppResult<Vec<Rule>> {
            Ok(self.enabled.clone())
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
            ..Default::default()
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
            ..Default::default()
        });
        let svc = RuleService::new(repo);
        let mut r = rule(1, "merchant == \"The Roastery\"", 42);
        r.overwrite_manual = false;

        let result = svc.run(&[r], Some(1), RuleRunKind::Single).await.unwrap();

        assert_eq!(result.matched, 1);
        assert_eq!(result.changed, 0); // manual category left untouched
    }

    /// The narrow claim hoisting compilation rests on: a program compiled once and run
    /// against a context returns the same verdict `evaluate_expression` returned when it was
    /// handed the source per row — including for the three ways a rule reaches "no match"
    /// (a non-boolean result, a runtime error, and source that never compiles at all).
    #[test]
    fn a_compiled_program_agrees_with_evaluating_the_source() {
        let row = ctx(1, -450, Some(7));
        let cur = Current::of(&row);
        let context = build_context(&row, &cur);
        let mut vm = VM::new();

        for expression in [
            "merchant == \"The Roastery\"",
            "is_expense and abs_amount > 4",
            "is_income",
            "contains(lower(description), 'flat')",
            "category_id == 7",
            "merchant_id == null",
            "amount_minor",                          // non-boolean result → no match
            "len(amount_minor)",                     // runtime type error → no match
            "this is not zen-expression syntax +++", // never compiles → no match
        ] {
            let per_row = zen_expression::evaluate_expression(
                expression,
                Value::Object(context.clone()).into(),
            )
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
            let hoisted = zen_expression::compile_expression(expression)
                .ok()
                .map(|program| program_matches(&program, &context, &mut vm))
                .unwrap_or(false);
            assert_eq!(per_row, hoisted, "verdict changed for `{expression}`");
        }
    }

    /// Several rules over several transactions, asserting the planned changes field by field:
    /// priority order decides who sets the category first, `stop_on_match` ends the row where
    /// it always did, and a disabled rule is neither counted nor applied.
    #[test]
    fn a_rule_set_over_several_rows_plans_exactly_what_it_did_before() {
        let rows = vec![ctx(1, -450, None), ctx(2, 500, None)];

        let mut broad = rule(10, "is_expense", 10);
        broad.stop_on_match = false; // lets the next rule have a turn
        let stopper = rule(11, "abs_amount > 4", 11); // matches both rows, stop_on_match
        let unreached = rule(12, "is_income", 12); // row 2 already stopped at rule 11
        let mut off = rule(13, "is_expense", 13);
        off.enabled = false;

        let (matched, apps) = plan_run(&[broad, stopper, unreached, off], &rows);

        // Row 1: rules 10 then 11. Row 2: rule 11 only. The disabled rule never evaluates.
        assert_eq!(matched, 3);
        assert_eq!(apps.len(), 3);

        assert_eq!((apps[0].rule_id, apps[0].transaction_id), (10, 1));
        assert_eq!(apps[0].prev_category_id, None);
        assert_eq!(apps[0].new_category_id, Some(10));
        assert_eq!(apps[0].prev_categorized_by_rule_id, None);
        assert_eq!(apps[0].new_categorized_by_rule_id, Some(10));

        assert_eq!((apps[1].rule_id, apps[1].transaction_id), (11, 1));
        assert_eq!(apps[1].prev_category_id, Some(10));
        assert_eq!(apps[1].new_category_id, Some(11));
        assert_eq!(apps[1].prev_categorized_by_rule_id, Some(10));
        assert_eq!(apps[1].new_categorized_by_rule_id, Some(11));

        assert_eq!((apps[2].rule_id, apps[2].transaction_id), (11, 2));
        assert_eq!(apps[2].prev_category_id, None);
        assert_eq!(apps[2].new_category_id, Some(11));
    }

    /// Priority order is the only tie-breaker between two matching rules, and compiling up
    /// front must not disturb it: the same three rules in the reverse order settle on the
    /// other category and stop earlier.
    #[test]
    fn reversing_priority_order_changes_the_outcome() {
        let rows = vec![ctx(1, -450, None)];
        let mut broad = rule(10, "is_expense", 10);
        broad.stop_on_match = false;
        let stopper = rule(11, "abs_amount > 4", 11);

        let (_, forward) = plan_run(&[broad.clone(), stopper.clone()], &rows);
        let (matched_back, backward) = plan_run(&[stopper, broad], &rows);

        assert_eq!(forward.last().unwrap().new_category_id, Some(11));
        // Reversed, rule 11 matches first and stops the row before rule 10 is reached.
        assert_eq!(matched_back, 1);
        assert_eq!(backward.len(), 1);
        assert_eq!(backward[0].rule_id, 11);
    }

    /// A rule whose source never compiles is dropped by `compile_rules` instead of failing
    /// per row — it must still count as no match, and must not take the rest of the set with
    /// it (the pre-hoist loop simply evaluated it to `false` on every transaction).
    #[test]
    fn an_uncompilable_expression_matches_nothing_and_leaves_the_rest_of_the_set_running() {
        let rows = vec![ctx(1, -450, None)];
        let broken = rule(10, "this is not zen-expression syntax +++", 10);
        let good = rule(11, "is_expense", 11);

        let (matched, apps) = plan_run(&[broken, good], &rows);

        assert_eq!(matched, 1);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].rule_id, 11);
        assert_eq!(apps[0].new_category_id, Some(11));
    }

    /// The context is built once per row now, so the write-back after a rule applies is what
    /// keeps successive rules seeing each other's work. All three mutable fields are covered,
    /// because each is patched by the same helper and a missing one would be silent.
    #[test]
    fn a_later_rule_sees_what_an_earlier_rule_changed() {
        let rows = vec![ctx(1, -450, None)];

        let mut categorize = rule(10, "is_expense", 10);
        categorize.stop_on_match = false;

        let mut flag = rule(11, "category_id == 10", 0);
        flag.set_category_id = None;
        flag.set_one_off = Some(true);
        flag.set_merchant_id = Some(5);
        flag.stop_on_match = false;

        // Only reachable if both of rule 11's writes are visible in the patched context.
        let mut confirm = rule(12, "is_one_off and merchant_id == 5", 99);
        confirm.stop_on_match = true;

        let (matched, apps) = plan_run(&[categorize, flag, confirm], &rows);

        assert_eq!(
            matched, 3,
            "each rule must observe the previous rule's change"
        );
        assert_eq!(apps.len(), 3);
        assert!(apps[1].new_one_off);
        assert_eq!(apps[1].new_merchant_id, Some(5));
        assert_eq!(apps[2].new_category_id, Some(99));
    }

    // ---- the unattended pass ------------------------------------------------------

    /// The property the whole design rests on: the automatic pass cannot reach a transaction
    /// that already has a category, however the rule is configured. `overwrite_manual` is set
    /// here precisely because it is the flag that *would* let a deliberate `run` replace it.
    #[tokio::test]
    async fn categorize_new_never_touches_an_already_categorized_transaction() {
        let mut overwriting = rule(1, "merchant == \"The Roastery\"", 42);
        overwriting.overwrite_manual = true;
        let repo = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, Some(7)), ctx(2, -450, None)],
            enabled: vec![overwriting],
            ..Default::default()
        });
        let svc = RuleService::new(repo.clone());

        let result = svc.categorize_new().await.unwrap().expect("row 2 changed");

        assert_eq!(result.matched, 1, "only the uncategorised row is evaluated");
        assert_eq!(result.changed, 1);
        let persisted = repo.persisted.lock().unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].0, RuleRunKind::Auto, "recorded as automatic");
    }

    /// Running after every sync of every provider means the common case is "nothing new
    /// matched" — and that case must not write an audit row, or the log fills with entries
    /// describing no change.
    #[tokio::test]
    async fn categorize_new_writes_nothing_when_it_changes_nothing() {
        let repo = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, None)],
            enabled: vec![rule(1, "merchant == \"Somewhere Else\"", 42)],
            ..Default::default()
        });
        let svc = RuleService::new(repo.clone());

        assert!(svc.categorize_new().await.unwrap().is_none());
        assert!(
            repo.persisted.lock().unwrap().is_empty(),
            "an empty run must not reach the audit log"
        );
    }

    /// Second run over the same ledger: the rows the first pass categorised are now excluded
    /// by the loader, so there is nothing left to do and nothing left to record. This is what
    /// makes it safe on a schedule.
    #[tokio::test]
    async fn categorize_new_is_idempotent() {
        let repo = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, None)],
            enabled: vec![rule(1, "merchant == \"The Roastery\"", 42)],
            ..Default::default()
        });
        let svc = RuleService::new(repo.clone());
        assert!(svc.categorize_new().await.unwrap().is_some());

        // The fake's contexts are fixed, so re-running would find the same row again. Stand in
        // the post-run ledger — the row now carries the category the run gave it.
        let after = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, Some(42))],
            enabled: vec![rule(1, "merchant == \"The Roastery\"", 42)],
            ..Default::default()
        });
        let svc = RuleService::new(after.clone());

        assert!(svc.categorize_new().await.unwrap().is_none());
        assert!(after.persisted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn categorize_new_does_nothing_with_no_enabled_rules() {
        let repo = Arc::new(FakeRules {
            contexts: vec![ctx(1, -450, None)],
            ..Default::default()
        });
        let svc = RuleService::new(repo.clone());

        assert!(svc.categorize_new().await.unwrap().is_none());
        assert!(repo.persisted.lock().unwrap().is_empty());
    }

    /// The expression migration 0030 ships, pinned here because a `mortgage`'s own feed posts
    /// its monthly interest as one row reading "Interest of $1083.51 Principal" — wording the
    /// two loan rules in 0026 both miss, since neither the words "loan repayment" nor a
    /// separate principal row ever appear. Amounts are invented; the shape is what matters.
    #[test]
    fn the_shipped_loan_interest_rule_matches_a_loan_feeds_own_wording() {
        const SHIPPED: &str = "account_kind in ['mortgage', 'loan'] and startsWith(lower(description), 'interest of')";
        validate_expression(SHIPPED).expect("the shipped expression must be valid");

        let matches = |kind: AccountKind, description: &str| {
            let mut row = ctx(1, 108_351, None);
            row.account_kind = kind;
            row.description = description.to_string();
            let cur = Current::of(&row);
            zen_expression::evaluate_expression(
                SHIPPED,
                Value::Object(build_context(&row, &cur)).into(),
            )
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        };

        assert!(matches(
            AccountKind::Mortgage,
            "Interest of $1083.51 Principal"
        ));
        assert!(matches(AccountKind::Loan, "Interest of $6.59 Principal"));
        // The same words on a savings account are interest *earned*, and belong to the rule
        // that already covers that — this one must not claim them.
        assert!(!matches(
            AccountKind::Savings,
            "Interest of $12.00 Principal"
        ));
        // And it must not widen into every mention of interest on a loan.
        assert!(!matches(AccountKind::Mortgage, "IRD:TAX ON INTEREST"));
    }

    /// The expression migration 0031 ships. 0026 already means to cover StudyLink, but looks
    /// for the scheme's wording ("living costs", "course related costs"); ASB writes the
    /// initials, so every payment fell through. The reference numbers here are invented — the
    /// abbreviation and the run-together "PAYMENTREF" are the shape that matters.
    #[test]
    fn the_shipped_studylink_rule_matches_the_abbreviations_a_statement_uses() {
        const SHIPPED: &str = "contains(lower(description), 'studylink') and \
                               (contains(lower(description), 'lc paym') \
                               or contains(lower(description), 'crc paym'))";
        validate_expression(SHIPPED).expect("the shipped expression must be valid");

        let matches = |description: &str| {
            let mut row = ctx(1, 30_000, None);
            row.description = description.to_string();
            let cur = Current::of(&row);
            zen_expression::evaluate_expression(
                SHIPPED,
                Value::Object(build_context(&row, &cur)).into(),
            )
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        };

        assert!(matches(
            "D/C FROM STUDYLINK (MSD) LC PAYMENT REF:00000 SSV000000000"
        ));
        assert!(matches("D/C FROM STUDYLINK (MSD) CRC PAYMENT REF:00000"));
        // One real variant runs the fields together, which is why the token stops at "paym".
        assert!(matches("STUDYLINK (MSD) LC PAYMENTREF:000000SSV000000000"));
        // A Student Allowance is a grant, not a drawdown — genuine income, and not this rule's.
        // This is what the `and contains('studylink')` half cannot do on its own.
        assert!(!matches("D/C FROM STUDYLINK (MSD) ALLOWANCE REF:00000"));
        // Neither half is safe alone: the abbreviation is short enough to appear in ordinary
        // text, and must not classify anything on its own.
        assert!(!matches("BLC PAYMENTS LTD"));
    }

    /// The expression migration 0032 ships, and the one property that makes the pair safe:
    /// 0026 shipped only the credit half, and a bank writes the charge identically with one
    /// letter changed, so the two rules must not be able to claim each other's rows.
    #[test]
    fn the_shipped_debit_interest_rule_cannot_claim_the_credit_half() {
        const CHARGED: &str =
            "(contains(lower(description), 'dr.int') or contains(lower(description), 'debit int'))";
        // The sibling from 0026, quoted so the disjointness below is asserted about the real
        // pair rather than about this rule alone.
        const EARNED: &str = "(contains(lower(description), 'cr.int') \
                              or contains(lower(description), 'credit int') \
                              or contains(lower(description), 'reward interest') \
                              or contains(lower(description), 'interest earned'))";
        validate_expression(CHARGED).expect("the shipped expression must be valid");

        let matches = |expression: &str, description: &str| {
            let mut row = ctx(1, -12_499, None);
            row.description = description.to_string();
            let cur = Current::of(&row);
            zen_expression::evaluate_expression(
                expression,
                Value::Object(build_context(&row, &cur)).into(),
            )
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        };

        for charge in ["DR.INT TO 01/02/2026", "ASB BANK - DEBIT INT TO 01/02/2026"] {
            assert!(matches(CHARGED, charge), "must claim {charge}");
            assert!(!matches(EARNED, charge), "earned must not claim {charge}");
        }
        for credit in [
            "CR.INT TO 01/02/2026",
            "ASB BANK - INTEREST CR.INT TO 01/02",
        ] {
            assert!(!matches(CHARGED, credit), "must not claim {credit}");
            assert!(matches(EARNED, credit), "earned must still claim {credit}");
        }
    }

    /// The expression migration 0033 leaves behind. A university is an employer as readily as
    /// it is somewhere you pay fees, so 0026's rule — which looked only at the wording — filed
    /// three real "UNI OF AUCKLAND SALARY" credits as tertiary-education *expenses*, taking a
    /// bite out of income and adding one to spending from the same rows.
    #[test]
    fn the_shipped_university_rule_ignores_money_coming_in() {
        const SHIPPED: &str = "(contains(lower(description), 'uni of auckl') \
                               or contains(lower(description), 'university o') \
                               or contains(lower(description), 'ak uni') \
                               or contains(lower(description), 'academic dre') \
                               or contains(lower(description), 'uoa') \
                               or contains(lower(description), 'canterbury u') \
                               or contains(lower(description), 'language tra')) and is_expense";
        validate_expression(SHIPPED).expect("the shipped expression must be valid");

        let matches = |amount_minor: i64, description: &str| {
            let mut row = ctx(1, amount_minor, None);
            row.description = description.to_string();
            let cur = Current::of(&row);
            zen_expression::evaluate_expression(
                SHIPPED,
                Value::Object(build_context(&row, &cur)).into(),
            )
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        };

        // Paying the university is still education spending — the whole rule must survive.
        assert!(matches(-1_250_00, "UNI OF AUCKLAND TUITION"));
        assert!(matches(-450_00, "UNIVERSITY OF CANTERBURY"));
        // Being paid *by* one is not.
        assert!(!matches(1_487_20, "UNI OF AUCKLAND SALARY"));
        assert!(!matches(612_35, "UNI OF AUCKLAND SALARY"));
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
