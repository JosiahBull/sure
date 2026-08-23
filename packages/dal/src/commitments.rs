//! `expense_commitments` CRUD. See `0044_expense_commitments.sql` for what the table is for and
//! `sure_app::forecast`'s `CommitmentNetting` for the invariant the `merchant_id` column carries.

use sure_core::{AppError, AppResult, PayFrequency};
pub use sure_core::{ExpenseCommitment, SaveExpenseCommitment};

use crate::Db;

#[derive(Debug)]
struct CommitmentRow {
    id: i64,
    category_id: i64,
    label: String,
    amount_minor: i64,
    currency_code: String,
    cadence: String,
    first_due_on: String,
    ends_on: Option<String>,
    escalation_delta_bps: i64,
    merchant_id: Option<i64>,
    enabled: bool,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<CommitmentRow> for ExpenseCommitment {
    type Error = AppError;

    /// Fallible for one column. `cadence` is stored as `TEXT` — the one legal place a domain enum
    /// is a string — and is parsed here, on the way in. Failing is right: every writer goes
    /// through `as_str`, so an unparseable value means the row came from something that went around
    /// all of them, and coercing it to whichever variant looks closest would silently reprice a bill.
    fn try_from(r: CommitmentRow) -> AppResult<Self> {
        let cadence: PayFrequency = r
            .cadence
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(ExpenseCommitment {
            id: r.id,
            category_id: r.category_id,
            label: r.label,
            amount_minor: r.amount_minor,
            currency_code: r.currency_code,
            cadence,
            first_due_on: r.first_due_on,
            ends_on: r.ends_on,
            escalation_delta_bps: r.escalation_delta_bps,
            merchant_id: r.merchant_id,
            enabled: r.enabled,
            notes: r.notes,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<ExpenseCommitment>> {
    sqlx::query_as!(
        CommitmentRow,
        r#"SELECT id AS "id!", category_id, label, amount_minor, currency_code,
                  cadence, first_due_on, ends_on, escalation_delta_bps, merchant_id,
                  enabled AS "enabled!: bool", notes, created_at, updated_at
             FROM expense_commitments ORDER BY category_id, label, id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<ExpenseCommitment> {
    sqlx::query_as!(
        CommitmentRow,
        r#"SELECT id AS "id!", category_id, label, amount_minor, currency_code,
                  cadence, first_due_on, ends_on, escalation_delta_bps, merchant_id,
                  enabled AS "enabled!: bool", notes, created_at, updated_at
             FROM expense_commitments WHERE id = ?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("expense commitment"))?
    .try_into()
}

/// Shared validation. Both writers run it, because a rule enforced on create and not on update is
/// a rule an update can walk around.
fn validate(input: &SaveExpenseCommitment) -> AppResult<String> {
    let label = input.label.trim();
    if label.is_empty() {
        return Err(AppError::validation("a commitment needs a label"));
    }
    // Ordered, not just present. An `ends_on` before `first_due_on` describes a commitment that is
    // never active, which the projection would silently treat as costing nothing — the caller
    // almost certainly meant something else and should be told rather than obeyed.
    if let Some(ends) = &input.ends_on
        && ends.to_string() < input.first_due_on.to_string()
    {
        return Err(AppError::validation(
            "ends_on is before first_due_on, which is a commitment that is never active",
        ));
    }
    Ok(label.to_string())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveExpenseCommitment) -> AppResult<ExpenseCommitment> {
    let label = validate(&input)?;
    let cadence = input.cadence.as_str();
    let currency = input.currency_code.trim().to_uppercase();
    let first_due_on = input.first_due_on.to_string();
    let ends_on = input.ends_on.as_ref().map(ToString::to_string);
    let amount = input.amount_minor.minor();
    let notes = input
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let id = sqlx::query_scalar!(
        r#"INSERT INTO expense_commitments
              (category_id, label, amount_minor, currency_code, cadence, first_due_on, ends_on,
               escalation_delta_bps, merchant_id, enabled, notes)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
           RETURNING id AS "id!""#,
        input.category_id,
        label,
        amount,
        currency,
        cadence,
        first_due_on,
        ends_on,
        input.escalation_delta_bps,
        input.merchant_id,
        input.enabled,
        notes
    )
    .fetch_one(db)
    .await
    .map_err(map_fk)?;
    get(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(
    db: &Db,
    id: i64,
    input: SaveExpenseCommitment,
) -> AppResult<ExpenseCommitment> {
    let label = validate(&input)?;
    let cadence = input.cadence.as_str();
    let currency = input.currency_code.trim().to_uppercase();
    let first_due_on = input.first_due_on.to_string();
    let ends_on = input.ends_on.as_ref().map(ToString::to_string);
    let amount = input.amount_minor.minor();
    let notes = input
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let done = sqlx::query!(
        "UPDATE expense_commitments SET
            category_id=?2, label=?3, amount_minor=?4, currency_code=?5, cadence=?6,
            first_due_on=?7, ends_on=?8, escalation_delta_bps=?9, merchant_id=?10, enabled=?11,
            notes=?12, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1",
        id,
        input.category_id,
        label,
        amount,
        currency,
        cadence,
        first_due_on,
        ends_on,
        input.escalation_delta_bps,
        input.merchant_id,
        input.enabled,
        notes
    )
    .execute(db)
    .await
    .map_err(map_fk)?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("expense commitment"));
    }
    get(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let done = sqlx::query!("DELETE FROM expense_commitments WHERE id=?1", id)
        .execute(db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("expense commitment"));
    }
    Ok(())
}

/// A foreign-key violation named, rather than surfaced as a 500.
///
/// Three FKs can fail here and a caller can fix any of them, so a generic internal error would be
/// hiding a fixable mistake behind an unfixable-looking one.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_fk(e: sqlx::Error) -> AppError {
    // `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here.
    match &e {
        sqlx::Error::Database(db) if db.message().contains("FOREIGN KEY") => {
            AppError::validation("unknown category, currency or merchant for this commitment")
        }
        _ => AppError::from(e),
    }
}
