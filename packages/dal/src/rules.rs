//! Rules repository: rule CRUD, the transaction contexts a run evaluates against,
//! and the persistence/undo of a run's audit trail. The Zen-expression evaluation
//! itself lives in the API crate (it owns `zen-expression`); this layer loads the
//! contexts, then persists the changes the evaluator decided on.

use sure_core::{AccountKind, AppError, AppResult, RuleRunKind};
pub use sure_core::{
    PreviewMatch, PreviewRequest, Rule, RuleApplicationDetail, RulePreview, RuleRun, RunResult,
    SaveRule,
};

use crate::Db;

/// Parse a stored `kind` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `RuleRunKind::as_str`, so an unparseable value means the row
/// came from something else entirely and deserves a real error, not a silent default.
fn parse_kind(kind: String) -> AppResult<RuleRunKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

/// A rule application row as stored — internal to `undo_run`'s revert check. Never
/// serialised: the API-facing view is [`RuleApplicationDetail`].
#[derive(Debug)]
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

/// The raw row shape for [`TxCtx`] — `account_kind` as stored, before parsing.
#[derive(Debug, Clone)]
struct TxCtxRow {
    id: i64,
    account_id: i64,
    posted_at: String,
    amount_minor: i64,
    currency_code: String,
    decimal_places: i64,
    description: String,
    merchant: Option<String>,
    merchant_id: Option<i64>,
    notes: Option<String>,
    category_id: Option<i64>,
    is_one_off: bool,
    categorized_by_rule_id: Option<i64>,
    account_name: String,
    account_kind: String,
}

/// A transaction row denormalised for rule evaluation. The API crate reads these
/// fields to build the Zen context; the DAL only loads them.
#[derive(Debug, Clone)]
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
    pub account_kind: AccountKind,
}

impl TryFrom<TxCtxRow> for TxCtx {
    type Error = AppError;

    fn try_from(r: TxCtxRow) -> AppResult<Self> {
        // Same rule as `sure_dal::accounts::AccountRow`: every writer goes through
        // `AccountKind::as_str`, so an unparseable value is a real error, not a default.
        let account_kind: AccountKind = r
            .account_kind
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(TxCtx {
            account_kind,
            id: r.id,
            account_id: r.account_id,
            posted_at: r.posted_at,
            amount_minor: r.amount_minor,
            currency_code: r.currency_code,
            decimal_places: r.decimal_places,
            description: r.description,
            merchant: r.merchant,
            merchant_id: r.merchant_id,
            notes: r.notes,
            category_id: r.category_id,
            is_one_off: r.is_one_off,
            categorized_by_rule_id: r.categorized_by_rule_id,
            account_name: r.account_name,
        })
    }
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

#[derive(Debug)]
struct RuleRow {
    id: i64,
    name: String,
    description: Option<String>,
    expression: String,
    set_category_id: Option<i64>,
    set_one_off: Option<bool>,
    set_merchant_id: Option<i64>,
    overwrite_manual: bool,
    stop_on_match: bool,
    priority: i64,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl From<RuleRow> for Rule {
    fn from(r: RuleRow) -> Self {
        Rule {
            id: r.id,
            name: r.name,
            description: r.description,
            expression: r.expression,
            set_category_id: r.set_category_id,
            set_one_off: r.set_one_off,
            set_merchant_id: r.set_merchant_id,
            overwrite_manual: r.overwrite_manual,
            stop_on_match: r.stop_on_match,
            priority: r.priority,
            enabled: r.enabled,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

// ---- CRUD ----------------------------------------------------------------

/// List rules in evaluation order.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Rule>> {
    Ok(sqlx::query_as!(
        RuleRow,
        r#"SELECT id AS "id!", name, description, expression, set_category_id,
                  set_one_off AS "set_one_off: bool", set_merchant_id,
                  overwrite_manual AS "overwrite_manual!: bool",
                  stop_on_match AS "stop_on_match!: bool", priority,
                  enabled AS "enabled!: bool", created_at, updated_at
                 FROM rules ORDER BY priority, id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

/// Enabled rules in evaluation order (for a "run all").
#[tracing::instrument(level = "debug", skip_all)]
pub async fn enabled_rules(db: &Db) -> AppResult<Vec<Rule>> {
    Ok(sqlx::query_as!(
        RuleRow,
        r#"SELECT id AS "id!", name, description, expression, set_category_id,
                  set_one_off AS "set_one_off: bool", set_merchant_id,
                  overwrite_manual AS "overwrite_manual!: bool",
                  stop_on_match AS "stop_on_match!: bool", priority,
                  enabled AS "enabled!: bool", created_at, updated_at
                 FROM rules WHERE enabled=1 ORDER BY priority, id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<Rule> {
    Ok(sqlx::query_as!(
        RuleRow,
        r#"SELECT id AS "id!", name, description, expression, set_category_id,
                  set_one_off AS "set_one_off: bool", set_merchant_id,
                  overwrite_manual AS "overwrite_manual!: bool",
                  stop_on_match AS "stop_on_match!: bool", priority,
                  enabled AS "enabled!: bool", created_at, updated_at
                 FROM rules WHERE id=?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("rule"))?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveRule) -> AppResult<Rule> {
    let name = input.name.trim();
    let expression = input.expression.trim();
    Ok(sqlx::query_as!(
        RuleRow,
        r#"INSERT INTO rules
              (name, description, expression, set_category_id, set_one_off, overwrite_manual,
               stop_on_match, priority, enabled, set_merchant_id)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
           RETURNING id AS "id!", name, description, expression,
                     set_category_id AS "set_category_id?",
                     set_one_off AS "set_one_off: bool", set_merchant_id AS "set_merchant_id?",
                     overwrite_manual AS "overwrite_manual!: bool",
                     stop_on_match AS "stop_on_match!: bool", priority,
                     enabled AS "enabled!: bool", created_at, updated_at"#,
        name,
        input.description,
        expression,
        input.set_category_id,
        input.set_one_off,
        input.overwrite_manual,
        input.stop_on_match,
        input.priority,
        input.enabled,
        input.set_merchant_id
    )
    .fetch_one(db)
    .await?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveRule) -> AppResult<Rule> {
    let name = input.name.trim();
    let expression = input.expression.trim();
    Ok(sqlx::query_as!(
        RuleRow,
        r#"UPDATE rules SET name=?2, description=?3, expression=?4, set_category_id=?5,
              set_one_off=?6, overwrite_manual=?7, stop_on_match=?8, priority=?9, enabled=?10,
              set_merchant_id=?11, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id=?1
           RETURNING id AS "id!", name, description, expression,
                     set_category_id AS "set_category_id?",
                     set_one_off AS "set_one_off: bool", set_merchant_id AS "set_merchant_id?",
                     overwrite_manual AS "overwrite_manual!: bool",
                     stop_on_match AS "stop_on_match!: bool", priority,
                     enabled AS "enabled!: bool", created_at, updated_at"#,
        id,
        name,
        input.description,
        expression,
        input.set_category_id,
        input.set_one_off,
        input.overwrite_manual,
        input.stop_on_match,
        input.priority,
        input.enabled,
        input.set_merchant_id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("rule"))?
    .into())
}

/// Delete a rule (audit history is retained; its rule_id becomes null via ON DELETE SET NULL).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM rules WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("rule"));
    }
    Ok(())
}

// ---- evaluation contexts + persistence -----------------------------------

/// Load every transaction's evaluation context.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn load_contexts(db: &Db) -> AppResult<Vec<TxCtx>> {
    // Columns needed to evaluate a rule against a transaction, denormalised for speed, in
    // `TxCtxRow`'s field order (`query_as!` maps positionally).
    sqlx::query_as!(
        TxCtxRow,
        r#"SELECT t.id AS "id!", t.account_id, t.posted_at, t.amount_minor, t.currency_code,
                  cur.decimal_places, t.description, t.merchant, t.merchant_id, t.notes,
                  t.category_id, t.is_one_off AS "is_one_off!: bool", t.categorized_by_rule_id,
                  a.name AS account_name, a.kind AS account_kind
             FROM transactions t
             JOIN accounts a ON a.id = t.account_id
             JOIN currencies cur ON cur.code = t.currency_code"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(TxCtx::try_from)
    .collect()
}

/// Load the evaluation context of every transaction that has no category yet — the rows
/// the automatic post-import/post-sync pass considers.
///
/// Deliberately a second query rather than a `WHERE` bolted onto [`load_contexts`] with a
/// flag: `query_as!` checks its SQL as a literal, so a runtime-assembled predicate would
/// have to give that up, and the two callers want genuinely different row sets.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn load_uncategorized_contexts(db: &Db) -> AppResult<Vec<TxCtx>> {
    sqlx::query_as!(
        TxCtxRow,
        r#"SELECT t.id AS "id!", t.account_id, t.posted_at, t.amount_minor, t.currency_code,
                  cur.decimal_places, t.description, t.merchant, t.merchant_id, t.notes,
                  t.category_id, t.is_one_off AS "is_one_off!: bool", t.categorized_by_rule_id,
                  a.name AS account_name, a.kind AS account_kind
             FROM transactions t
             JOIN accounts a ON a.id = t.account_id
             JOIN currencies cur ON cur.code = t.currency_code
            WHERE t.category_id IS NULL"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(TxCtx::try_from)
    .collect()
}

/// Persist a run: insert the run row, apply each decided change (updating the
/// transaction and writing the audit row), and record the counts. `matched` is the
/// number of rule matches the evaluator saw (including no-ops); `changed` is derived
/// from the applications actually made.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn persist_run(
    db: &Db,
    rule_id: Option<i64>,
    kind: RuleRunKind,
    matched: i64,
    applications: Vec<PlannedApplication>,
) -> AppResult<RunResult> {
    let mut txn = db.begin().await?;
    let kind = kind.as_str();
    let run_id = sqlx::query!(
        "INSERT INTO rule_runs (rule_id, kind) VALUES (?1,?2)",
        rule_id,
        kind
    )
    .execute(&mut *txn)
    .await?
    .last_insert_rowid();

    let changed = applications.len() as i64;
    for a in &applications {
        sqlx::query!(
            "UPDATE transactions SET category_id=?2, categorized_by_rule_id=?3, is_one_off=?4,
                merchant_id=?5, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
            a.transaction_id,
            a.new_category_id,
            a.new_categorized_by_rule_id,
            a.new_one_off,
            a.new_merchant_id
        )
        .execute(&mut *txn)
        .await?;
        sqlx::query!(
            "INSERT INTO rule_applications
                (rule_run_id, rule_id, transaction_id, prev_category_id, new_category_id,
                 prev_categorized_by_rule_id, prev_one_off, new_one_off,
                 prev_merchant_id, new_merchant_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            run_id,
            a.rule_id,
            a.transaction_id,
            a.prev_category_id,
            a.new_category_id,
            a.prev_categorized_by_rule_id,
            a.prev_one_off,
            a.new_one_off,
            a.prev_merchant_id,
            a.new_merchant_id
        )
        .execute(&mut *txn)
        .await?;
    }

    sqlx::query!(
        "UPDATE rule_runs SET matched=?2, changed=?3 WHERE id=?1",
        run_id,
        matched,
        changed
    )
    .execute(&mut *txn)
    .await?;
    txn.commit().await?;

    Ok(RunResult {
        run_id,
        matched,
        changed,
    })
}

#[derive(Debug)]
struct RuleRunRow {
    id: i64,
    rule_id: Option<i64>,
    kind: String,
    matched: i64,
    changed: i64,
    undone: bool,
    created_at: String,
}

impl TryFrom<RuleRunRow> for RuleRun {
    type Error = AppError;

    fn try_from(r: RuleRunRow) -> AppResult<Self> {
        Ok(RuleRun {
            kind: parse_kind(r.kind)?,
            id: r.id,
            rule_id: r.rule_id,
            matched: r.matched,
            changed: r.changed,
            undone: r.undone,
            created_at: r.created_at,
        })
    }
}

#[derive(Debug)]
struct RuleApplicationDetailRow {
    id: i64,
    transaction_id: i64,
    posted_at: String,
    description: String,
    amount_minor: i64,
    currency_code: String,
    prev_category_id: Option<i64>,
    new_category_id: Option<i64>,
    prev_merchant_id: Option<i64>,
    new_merchant_id: Option<i64>,
    prev_one_off: Option<bool>,
    new_one_off: Option<bool>,
    reverted: bool,
}

impl From<RuleApplicationDetailRow> for RuleApplicationDetail {
    fn from(r: RuleApplicationDetailRow) -> Self {
        RuleApplicationDetail {
            id: r.id,
            transaction_id: r.transaction_id,
            posted_at: r.posted_at,
            description: r.description,
            amount_minor: r.amount_minor,
            currency_code: r.currency_code,
            prev_category_id: r.prev_category_id,
            new_category_id: r.new_category_id,
            prev_merchant_id: r.prev_merchant_id,
            new_merchant_id: r.new_merchant_id,
            prev_one_off: r.prev_one_off,
            new_one_off: r.new_one_off,
            reverted: r.reverted,
        }
    }
}

/// List rule runs (most recent first) — the audit trail.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_runs(db: &Db) -> AppResult<Vec<RuleRun>> {
    sqlx::query_as!(
        RuleRunRow,
        r#"SELECT id AS "id!", rule_id, kind, matched, changed, undone AS "undone!: bool",
                  created_at
             FROM rule_runs ORDER BY id DESC"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(RuleRun::try_from)
    .collect()
}

/// The per-transaction changes made by a run, each joined to its transaction's current
/// description/date/amount for display. `transaction_id` is `ON DELETE CASCADE`, so an
/// application row can't outlive its transaction and the inner join always matches.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn run_applications(db: &Db, run_id: i64) -> AppResult<Vec<RuleApplicationDetail>> {
    Ok(sqlx::query_as!(
        RuleApplicationDetailRow,
        r#"SELECT a.id AS "id!", a.transaction_id, t.posted_at, t.description, t.amount_minor,
                  t.currency_code, a.prev_category_id, a.new_category_id, a.prev_merchant_id,
                  a.new_merchant_id, a.prev_one_off AS "prev_one_off: bool",
                  a.new_one_off AS "new_one_off: bool", a.reverted AS "reverted!: bool"
             FROM rule_applications a
             JOIN transactions t ON t.id = a.transaction_id
            WHERE a.rule_run_id = ?1
            ORDER BY a.id"#,
        run_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

/// Undo a run, reverting each changed transaction to its prior state (unless it was
/// changed again since). Returns `matched` = applications considered, `changed` =
/// transactions actually reverted.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn undo_run(db: &Db, run_id: i64) -> AppResult<RunResult> {
    let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM rule_runs WHERE id=?1", run_id)
        .fetch_one(db)
        .await?;
    if exists == 0 {
        return Err(AppError::NotFound("rule run"));
    }
    let apps = sqlx::query_as!(
        RuleApplication,
        r#"SELECT id AS "id!", rule_run_id, rule_id, transaction_id, prev_category_id,
                  new_category_id, prev_categorized_by_rule_id,
                  prev_one_off AS "prev_one_off: bool", new_one_off AS "new_one_off: bool",
                  prev_merchant_id, new_merchant_id, reverted AS "reverted!: bool", created_at
             FROM rule_applications WHERE rule_run_id=?1 AND reverted=0"#,
        run_id
    )
    .fetch_all(db)
    .await?;

    let mut txn = db.begin().await?;
    let mut reverted = 0i64;
    for app in &apps {
        // Only revert if the transaction is still in the state this run left it in.
        let res = sqlx::query!(
            "UPDATE transactions
             SET category_id=?2, categorized_by_rule_id=?3, is_one_off=?4, merchant_id=?7,
                 updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id=?1 AND category_id IS ?5 AND is_one_off = ?6 AND merchant_id IS ?8",
            app.transaction_id,
            app.prev_category_id,
            app.prev_categorized_by_rule_id,
            app.prev_one_off,
            app.new_category_id,
            app.new_one_off,
            app.prev_merchant_id,
            app.new_merchant_id
        )
        .execute(&mut *txn)
        .await?;
        if res.rows_affected() > 0 {
            reverted += 1;
        }
        sqlx::query!(
            "UPDATE rule_applications SET reverted=1 WHERE id=?1",
            app.id
        )
        .execute(&mut *txn)
        .await?;
    }
    sqlx::query!("UPDATE rule_runs SET undone=1 WHERE id=?1", run_id)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;

    Ok(RunResult {
        run_id,
        matched: apps.len() as i64,
        changed: reverted,
    })
}
