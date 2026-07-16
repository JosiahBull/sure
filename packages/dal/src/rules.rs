//! Rules repository: rule CRUD, the transaction contexts a run evaluates against,
//! and the persistence/undo of a run's audit trail. The Zen-expression evaluation
//! itself lives in the API crate (it owns `zen-expression`); this layer loads the
//! contexts, then persists the changes the evaluator decided on.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

/// Columns needed to evaluate a rule against a transaction, denormalised for speed.
pub const CTX_QUERY: &str = "SELECT t.id, t.account_id, t.posted_at, t.amount_minor, t.currency_code,
        cur.decimal_places AS decimal_places, t.description, t.merchant, t.merchant_id, t.notes,
        t.category_id, t.is_one_off, t.categorized_by_rule_id,
        a.name AS account_name, a.kind AS account_kind
    FROM transactions t
    JOIN accounts a ON a.id = t.account_id
    JOIN currencies cur ON cur.code = t.currency_code";

#[derive(Serialize, FromRow, ToSchema, Clone)]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    /// Zen expression evaluated against a transaction context; truthy => match.
    /// Fields available: `amount`, `amount_minor`, `abs_amount`, `is_income`,
    /// `is_expense`, `description`, `merchant`, `merchant_id`, `notes`, `currency`,
    /// `account`, `account_kind`, `account_id`, `category_id`, `is_one_off`, `date`,
    /// `year`, `month`, `day`.
    pub expression: String,
    pub set_category_id: Option<i64>,
    pub set_one_off: Option<bool>,
    /// Action: assign this custom merchant on match.
    pub set_merchant_id: Option<i64>,
    pub overwrite_manual: bool,
    pub stop_on_match: bool,
    pub priority: i64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveRule {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub expression: String,
    #[serde(default)]
    pub set_category_id: Option<i64>,
    #[serde(default)]
    pub set_one_off: Option<bool>,
    #[serde(default)]
    pub set_merchant_id: Option<i64>,
    #[serde(default)]
    pub overwrite_manual: bool,
    #[serde(default)]
    pub stop_on_match: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct RuleRun {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub kind: String,
    pub matched: i64,
    pub changed: i64,
    pub undone: bool,
    pub created_at: String,
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct RuleApplication {
    pub id: i64,
    pub rule_run_id: i64,
    pub rule_id: Option<i64>,
    pub transaction_id: i64,
    pub prev_category_id: Option<i64>,
    pub new_category_id: Option<i64>,
    pub prev_categorized_by_rule_id: Option<i64>,
    pub prev_one_off: Option<bool>,
    pub new_one_off: Option<bool>,
    pub prev_merchant_id: Option<i64>,
    pub new_merchant_id: Option<i64>,
    pub reverted: bool,
    pub created_at: String,
}

/// One change from a run, enriched with the transaction it touched, for the audit log's
/// expandable diff. Category/merchant ids are resolved to names by the client; the
/// before/after ids are enough to render "Groceries → Dining" style changes.
#[derive(Serialize, FromRow, ToSchema)]
pub struct RuleApplicationDetail {
    pub id: i64,
    pub transaction_id: i64,
    pub posted_at: String,
    pub description: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub prev_category_id: Option<i64>,
    pub new_category_id: Option<i64>,
    pub prev_merchant_id: Option<i64>,
    pub new_merchant_id: Option<i64>,
    pub prev_one_off: Option<bool>,
    pub new_one_off: Option<bool>,
    pub reverted: bool,
}

#[derive(Serialize, ToSchema)]
pub struct RunResult {
    pub run_id: i64,
    /// Transactions the rule(s) matched.
    pub matched: i64,
    /// Transactions actually changed (recorded in the audit log).
    pub changed: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct PreviewRequest {
    pub expression: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct RulePreview {
    pub matched: i64,
    pub sample: Vec<PreviewMatch>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewMatch {
    pub transaction_id: i64,
    pub posted_at: String,
    pub description: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
}

/// A transaction row denormalised for rule evaluation. The API crate reads these
/// fields to build the Zen context; the DAL only loads them.
#[derive(FromRow, Clone)]
pub struct TxCtx {
    pub id: i64,
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub decimal_places: i64,
    pub description: String,
    pub merchant: Option<String>,
    pub merchant_id: Option<i64>,
    pub notes: Option<String>,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub categorized_by_rule_id: Option<i64>,
    pub account_name: String,
    pub account_kind: String,
}

/// One decided change from an evaluation, ready to be persisted by [`persist_run`].
/// The evaluator (API crate) builds these; the DAL writes the transaction update and
/// the matching audit row.
pub struct PlannedApplication {
    pub rule_id: i64,
    pub transaction_id: i64,
    pub prev_category_id: Option<i64>,
    pub new_category_id: Option<i64>,
    pub prev_categorized_by_rule_id: Option<i64>,
    pub new_categorized_by_rule_id: Option<i64>,
    pub prev_one_off: bool,
    pub new_one_off: bool,
    pub prev_merchant_id: Option<i64>,
    pub new_merchant_id: Option<i64>,
}

// ---- CRUD ----------------------------------------------------------------

/// List rules in evaluation order.
pub async fn list(db: &Db) -> AppResult<Vec<Rule>> {
    Ok(sqlx::query_as::<_, Rule>("SELECT * FROM rules ORDER BY priority, id")
        .fetch_all(db)
        .await?)
}

/// Enabled rules in evaluation order (for a "run all").
pub async fn enabled_rules(db: &Db) -> AppResult<Vec<Rule>> {
    Ok(
        sqlx::query_as::<_, Rule>("SELECT * FROM rules WHERE enabled=1 ORDER BY priority, id")
            .fetch_all(db)
            .await?,
    )
}

pub async fn get(db: &Db, id: i64) -> AppResult<Rule> {
    sqlx::query_as::<_, Rule>("SELECT * FROM rules WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("rule"))
}

pub async fn create(db: &Db, input: SaveRule) -> AppResult<Rule> {
    Ok(sqlx::query_as::<_, Rule>(
        "INSERT INTO rules
            (name, description, expression, set_category_id, set_one_off, overwrite_manual,
             stop_on_match, priority, enabled, set_merchant_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(&input.description)
    .bind(input.expression.trim())
    .bind(input.set_category_id)
    .bind(input.set_one_off)
    .bind(input.overwrite_manual)
    .bind(input.stop_on_match)
    .bind(input.priority)
    .bind(input.enabled)
    .bind(input.set_merchant_id)
    .fetch_one(db)
    .await?)
}

pub async fn update(db: &Db, id: i64, input: SaveRule) -> AppResult<Rule> {
    sqlx::query_as::<_, Rule>(
        "UPDATE rules SET name=?2, description=?3, expression=?4, set_category_id=?5, set_one_off=?6,
            overwrite_manual=?7, stop_on_match=?8, priority=?9, enabled=?10, set_merchant_id=?11,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(&input.description)
    .bind(input.expression.trim())
    .bind(input.set_category_id)
    .bind(input.set_one_off)
    .bind(input.overwrite_manual)
    .bind(input.stop_on_match)
    .bind(input.priority)
    .bind(input.enabled)
    .bind(input.set_merchant_id)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("rule"))
}

/// Delete a rule (audit history is retained; its rule_id becomes null via ON DELETE SET NULL).
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM rules WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("rule"));
    }
    Ok(())
}

// ---- evaluation contexts + persistence -----------------------------------

/// Load every transaction's evaluation context.
pub async fn load_contexts(db: &Db) -> AppResult<Vec<TxCtx>> {
    Ok(sqlx::query_as::<_, TxCtx>(CTX_QUERY).fetch_all(db).await?)
}

/// Persist a run: insert the run row, apply each decided change (updating the
/// transaction and writing the audit row), and record the counts. `matched` is the
/// number of rule matches the evaluator saw (including no-ops); `changed` is derived
/// from the applications actually made.
pub async fn persist_run(
    db: &Db,
    rule_id: Option<i64>,
    kind: &str,
    matched: i64,
    applications: Vec<PlannedApplication>,
) -> AppResult<RunResult> {
    let mut txn = db.begin().await?;
    let run_id = sqlx::query("INSERT INTO rule_runs (rule_id, kind) VALUES (?1,?2)")
        .bind(rule_id)
        .bind(kind)
        .execute(&mut *txn)
        .await?
        .last_insert_rowid();

    let changed = applications.len() as i64;
    for a in &applications {
        sqlx::query(
            "UPDATE transactions SET category_id=?2, categorized_by_rule_id=?3, is_one_off=?4,
                merchant_id=?5, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
        )
        .bind(a.transaction_id)
        .bind(a.new_category_id)
        .bind(a.new_categorized_by_rule_id)
        .bind(a.new_one_off)
        .bind(a.new_merchant_id)
        .execute(&mut *txn)
        .await?;
        sqlx::query(
            "INSERT INTO rule_applications
                (rule_run_id, rule_id, transaction_id, prev_category_id, new_category_id,
                 prev_categorized_by_rule_id, prev_one_off, new_one_off,
                 prev_merchant_id, new_merchant_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )
        .bind(run_id)
        .bind(a.rule_id)
        .bind(a.transaction_id)
        .bind(a.prev_category_id)
        .bind(a.new_category_id)
        .bind(a.prev_categorized_by_rule_id)
        .bind(a.prev_one_off)
        .bind(a.new_one_off)
        .bind(a.prev_merchant_id)
        .bind(a.new_merchant_id)
        .execute(&mut *txn)
        .await?;
    }

    sqlx::query("UPDATE rule_runs SET matched=?2, changed=?3 WHERE id=?1")
        .bind(run_id)
        .bind(matched)
        .bind(changed)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    Ok(RunResult {
        run_id,
        matched,
        changed,
    })
}

/// List rule runs (most recent first) — the audit trail.
pub async fn list_runs(db: &Db) -> AppResult<Vec<RuleRun>> {
    Ok(
        sqlx::query_as::<_, RuleRun>("SELECT * FROM rule_runs ORDER BY id DESC")
            .fetch_all(db)
            .await?,
    )
}

/// The per-transaction changes made by a run, each joined to its transaction's current
/// description/date/amount for display. `transaction_id` is `ON DELETE CASCADE`, so an
/// application row can't outlive its transaction and the inner join always matches.
pub async fn run_applications(db: &Db, run_id: i64) -> AppResult<Vec<RuleApplicationDetail>> {
    Ok(sqlx::query_as::<_, RuleApplicationDetail>(
        "SELECT a.id, a.transaction_id, t.posted_at, t.description, t.amount_minor, t.currency_code,
                a.prev_category_id, a.new_category_id, a.prev_merchant_id, a.new_merchant_id,
                a.prev_one_off, a.new_one_off, a.reverted
         FROM rule_applications a
         JOIN transactions t ON t.id = a.transaction_id
         WHERE a.rule_run_id = ?1
         ORDER BY a.id",
    )
    .bind(run_id)
    .fetch_all(db)
    .await?)
}

/// Undo a run, reverting each changed transaction to its prior state (unless it was
/// changed again since). Returns `matched` = applications considered, `changed` =
/// transactions actually reverted.
pub async fn undo_run(db: &Db, run_id: i64) -> AppResult<RunResult> {
    let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rule_runs WHERE id=?1")
        .bind(run_id)
        .fetch_one(db)
        .await?;
    if exists == 0 {
        return Err(AppError::NotFound("rule run"));
    }
    let apps = sqlx::query_as::<_, RuleApplication>(
        "SELECT * FROM rule_applications WHERE rule_run_id=?1 AND reverted=0",
    )
    .bind(run_id)
    .fetch_all(db)
    .await?;

    let mut txn = db.begin().await?;
    let mut reverted = 0i64;
    for app in &apps {
        // Only revert if the transaction is still in the state this run left it in.
        let res = sqlx::query(
            "UPDATE transactions
             SET category_id=?2, categorized_by_rule_id=?3, is_one_off=?4, merchant_id=?7,
                 updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?1 AND category_id IS ?5 AND is_one_off = ?6 AND merchant_id IS ?8",
        )
        .bind(app.transaction_id)
        .bind(app.prev_category_id)
        .bind(app.prev_categorized_by_rule_id)
        .bind(app.prev_one_off)
        .bind(app.new_category_id)
        .bind(app.new_one_off)
        .bind(app.prev_merchant_id)
        .bind(app.new_merchant_id)
        .execute(&mut *txn)
        .await?;
        if res.rows_affected() > 0 {
            reverted += 1;
        }
        sqlx::query("UPDATE rule_applications SET reverted=1 WHERE id=?1")
            .bind(app.id)
            .execute(&mut *txn)
            .await?;
    }
    sqlx::query("UPDATE rule_runs SET undone=1 WHERE id=?1")
        .bind(run_id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    Ok(RunResult {
        run_id,
        matched: apps.len() as i64,
        changed: reverted,
    })
}
