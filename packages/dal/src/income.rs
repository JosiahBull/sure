//! Per-person income streams: persistence. Which take-home rate wins is resolution logic and
//! lives in `sure-app`; this module stores and retrieves the rows.

use sure_core::{
    AppError, AppResult, IncomeBasis, IncomePayment, IncomePaymentStatus, IncomeStream,
    IncomeStreamStep, MatchedBy, PayFrequency, PayTreatment, PayeBreakdown, SaveIncomeStream,
};

use crate::Db;

#[derive(Debug)]
struct IncomeStreamRow {
    id: i64,
    person_id: i64,
    label: String,
    employer: Option<String>,
    currency_code: String,
    annual_amount_minor: i64,
    basis: String,
    pay_frequency: String,
    first_payment_on: String,
    starts_on: String,
    ends_on: Option<String>,
    annual_increase_bps: i64,
    kiwisaver_bps: i64,
    employer_kiwisaver_bps: i64,
    student_loan: bool,
    take_home_bps: Option<i64>,
    linked_category_id: Option<i64>,
    kiwisaver_account_id: Option<i64>,
    student_loan_account_id: Option<i64>,
    match_account_id: Option<i64>,
    match_pattern: Option<String>,
    pay_treatment: String,
    enabled: bool,
    sort_order: i64,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl IncomeStreamRow {
    /// Steps are attached separately, so this takes them rather than querying — one query for
    /// every stream's steps beats one per stream.
    fn into_stream(self, steps: Vec<IncomeStreamStep>) -> AppResult<IncomeStream> {
        // Every writer goes through `as_str`, so a value that doesn't parse means the row was
        // written by something else entirely — surface it rather than coercing it into whichever
        // variant looks closest, which would silently reprice someone's salary.
        let bad = |e: String| AppError::Internal(anyhow::anyhow!(e));
        let basis: IncomeBasis = self.basis.parse().map_err(bad)?;
        let pay_frequency: PayFrequency = self.pay_frequency.parse().map_err(bad)?;
        let pay_treatment: PayTreatment = self.pay_treatment.parse().map_err(bad)?;
        Ok(IncomeStream {
            id: self.id,
            person_id: self.person_id,
            label: self.label,
            employer: self.employer,
            currency_code: self.currency_code,
            annual_amount_minor: self.annual_amount_minor,
            basis,
            pay_frequency,
            first_payment_on: self.first_payment_on,
            starts_on: self.starts_on,
            ends_on: self.ends_on,
            annual_increase_bps: self.annual_increase_bps,
            kiwisaver_bps: self.kiwisaver_bps,
            employer_kiwisaver_bps: self.employer_kiwisaver_bps,
            student_loan: self.student_loan,
            take_home_bps: self.take_home_bps,
            linked_category_id: self.linked_category_id,
            kiwisaver_account_id: self.kiwisaver_account_id,
            student_loan_account_id: self.student_loan_account_id,
            match_account_id: self.match_account_id,
            match_pattern: self.match_pattern,
            pay_treatment,
            enabled: self.enabled,
            sort_order: self.sort_order,
            notes: self.notes,
            steps,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Debug)]
struct IncomeStreamStepRow {
    id: i64,
    income_stream_id: i64,
    effective_on: String,
    annual_amount_minor: i64,
    label: Option<String>,
}

impl From<IncomeStreamStepRow> for IncomeStreamStep {
    fn from(r: IncomeStreamStepRow) -> Self {
        IncomeStreamStep {
            id: r.id,
            income_stream_id: r.income_stream_id,
            effective_on: r.effective_on,
            annual_amount_minor: r.annual_amount_minor,
            label: r.label,
        }
    }
}

/// Every stream with its dated steps attached, by person then sort order then label.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<IncomeStream>> {
    let rows = sqlx::query_as!(
        IncomeStreamRow,
        r#"SELECT id AS "id!", person_id, label, employer, currency_code, annual_amount_minor,
                  basis, pay_frequency, first_payment_on, starts_on, ends_on,
                  annual_increase_bps, kiwisaver_bps, employer_kiwisaver_bps,
                  student_loan AS "student_loan!: bool", take_home_bps, linked_category_id,
                  kiwisaver_account_id, student_loan_account_id, match_account_id, match_pattern,
                  pay_treatment, enabled AS "enabled!: bool",
                  sort_order, notes, created_at, updated_at
             FROM income_streams ORDER BY person_id, sort_order, label, id"#
    )
    .fetch_all(db)
    .await?;
    // One query for every step, grouped in memory: a per-stream query would be N+1 on a page that
    // always wants all of them.
    let steps = sqlx::query_as!(
        IncomeStreamStepRow,
        r#"SELECT id AS "id!", income_stream_id, effective_on, annual_amount_minor, label
             FROM income_stream_steps ORDER BY income_stream_id, effective_on"#
    )
    .fetch_all(db)
    .await?;
    let mut by_stream: std::collections::HashMap<i64, Vec<IncomeStreamStep>> =
        std::collections::HashMap::new();
    for s in steps {
        by_stream
            .entry(s.income_stream_id)
            .or_default()
            .push(s.into());
    }
    rows.into_iter()
        .map(|r| {
            let mine = by_stream.remove(&r.id).unwrap_or_default();
            r.into_stream(mine)
        })
        .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<IncomeStream> {
    let row = sqlx::query_as!(
        IncomeStreamRow,
        r#"SELECT id AS "id!", person_id, label, employer, currency_code, annual_amount_minor,
                  basis, pay_frequency, first_payment_on, starts_on, ends_on,
                  annual_increase_bps, kiwisaver_bps, employer_kiwisaver_bps,
                  student_loan AS "student_loan!: bool", take_home_bps, linked_category_id,
                  kiwisaver_account_id, student_loan_account_id, match_account_id, match_pattern,
                  pay_treatment, enabled AS "enabled!: bool",
                  sort_order, notes, created_at, updated_at
             FROM income_streams WHERE id=?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("income stream"))?;
    let steps = sqlx::query_as!(
        IncomeStreamStepRow,
        r#"SELECT id AS "id!", income_stream_id, effective_on, annual_amount_minor, label
             FROM income_stream_steps WHERE income_stream_id=?1 ORDER BY effective_on"#,
        id
    )
    .fetch_all(db)
    .await?;
    row.into_stream(steps.into_iter().map(Into::into).collect())
}

/// Shared validation for both writes. Collects **every** problem rather than failing on the first,
/// the `AccountMetadata::validate_for` contract — filling in a form should not be a game of
/// whack-a-mole.
fn validate(input: &SaveIncomeStream) -> AppResult<()> {
    let mut problems: Vec<String> = Vec::new();
    if input.label.trim().is_empty() {
        problems.push("label must not be empty".into());
    }
    if !(0..=10_000).contains(&input.kiwisaver_bps) {
        problems.push(format!(
            "kiwisaver_bps must be between 0 and 10000, got {}",
            input.kiwisaver_bps
        ));
    }
    if !(0..=10_000).contains(&input.employer_kiwisaver_bps) {
        problems.push(format!(
            "employer_kiwisaver_bps must be between 0 and 10000, got {}",
            input.employer_kiwisaver_bps
        ));
    }
    if let Some(th) = input.take_home_bps
        && !(0..=10_000).contains(&th)
    {
        problems.push(format!(
            "take_home_bps must be between 0 and 10000, got {th}"
        ));
    }
    if let Some(end) = input.ends_on.as_ref()
        && end.date() <= input.starts_on.date()
    {
        problems.push("ends_on must be after starts_on".into());
    }
    // Matching needs both halves: an account to look in and a token to look for. One without the
    // other is a matcher that silently never runs, which reads as a bug rather than a setting.
    let pattern_set = input
        .match_pattern
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    if input.match_account_id.is_some() && !pattern_set {
        problems.push(
            "match_account_id is set but match_pattern is empty — both are needed for matching"
                .into(),
        );
    }
    if input.match_account_id.is_none() && pattern_set {
        problems.push(
            "match_pattern is set but match_account_id is empty — both are needed for matching"
                .into(),
        );
    }
    // A schedule with two figures on the same date is a typo, and the unique index would report it
    // as an opaque constraint failure. Name it instead.
    let mut dates: Vec<chrono::NaiveDate> =
        input.steps.iter().map(|s| s.effective_on.date()).collect();
    dates.sort_unstable();
    if let Some(dup) = dates.windows(2).find(|w| w[0] == w[1]) {
        problems.push(format!("two steps share the date {}", dup[0]));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::validation(problems.join("; ")))
    }
}

/// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
/// (CLAUDE.md rule 2's escape hatch).
#[allow(clippy::wildcard_enum_match_arm)]
fn fk_error(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("person, currency, linked category or account does not exist")
        }
        other => AppError::from(other),
    }
}

async fn replace_steps(
    txn: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    stream_id: i64,
    input: &SaveIncomeStream,
) -> AppResult<()> {
    sqlx::query!(
        "DELETE FROM income_stream_steps WHERE income_stream_id=?1",
        stream_id
    )
    .execute(&mut **txn)
    .await?;
    for s in &input.steps {
        let effective_on = s.effective_on.to_string();
        let annual_amount_minor = s.annual_amount_minor.minor();
        let label = s.label.as_deref();
        sqlx::query!(
            "INSERT INTO income_stream_steps
                (income_stream_id, effective_on, annual_amount_minor, label)
             VALUES (?1,?2,?3,?4)",
            stream_id,
            effective_on,
            annual_amount_minor,
            label
        )
        .execute(&mut **txn)
        .await?;
    }
    Ok(())
}

/// Create the stream and its whole step schedule in one transaction.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, person_id: i64, input: SaveIncomeStream) -> AppResult<IncomeStream> {
    validate(&input)?;
    let mut txn = db.begin().await?;
    let label = input.label.trim();
    let employer = input.employer.as_deref();
    let currency_code = input.currency_code.to_uppercase();
    let annual_amount_minor = input.annual_amount_minor.minor();
    let basis = input.basis.as_str();
    let pay_frequency = input.pay_frequency.as_str();
    let first_payment_on = input.first_payment_on.to_string();
    let starts_on = input.starts_on.to_string();
    let ends_on = input.ends_on.as_ref().map(|d| d.to_string());
    let notes = input.notes.as_deref();
    // Stored trimmed, empty as NULL — matching is on iff both halves are set, and a
    // whitespace-only pattern must not read as "on".
    let match_pattern = input
        .match_pattern
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let pay_treatment = input.pay_treatment.as_str();
    // Only the new id is wanted — the steps go in below and `get` re-reads the whole stream —
    // so this returns that rather than restating all two dozen columns.
    let id = sqlx::query_scalar!(
        r#"INSERT INTO income_streams
              (person_id, label, employer, currency_code, annual_amount_minor, basis,
               pay_frequency, first_payment_on, starts_on, ends_on, annual_increase_bps,
               kiwisaver_bps, student_loan, take_home_bps, linked_category_id, enabled,
               sort_order, notes, employer_kiwisaver_bps, kiwisaver_account_id,
               student_loan_account_id, match_account_id, match_pattern, pay_treatment)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,
                   ?22,?23,?24)
           RETURNING id AS "id!""#,
        person_id,
        label,
        employer,
        currency_code,
        annual_amount_minor,
        basis,
        pay_frequency,
        first_payment_on,
        starts_on,
        ends_on,
        input.annual_increase_bps,
        input.kiwisaver_bps,
        input.student_loan,
        input.take_home_bps,
        input.linked_category_id,
        input.enabled,
        input.sort_order,
        notes,
        input.employer_kiwisaver_bps,
        input.kiwisaver_account_id,
        input.student_loan_account_id,
        input.match_account_id,
        match_pattern,
        pay_treatment
    )
    .fetch_one(&mut *txn)
    .await
    .map_err(fk_error)?;
    replace_steps(&mut txn, id, &input).await?;
    txn.commit().await?;
    get(db, id).await
}

/// Full replace, steps included — a step omitted from `input` is deleted.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveIncomeStream) -> AppResult<IncomeStream> {
    validate(&input)?;
    let mut txn = db.begin().await?;
    let label = input.label.trim();
    let employer = input.employer.as_deref();
    let currency_code = input.currency_code.to_uppercase();
    let annual_amount_minor = input.annual_amount_minor.minor();
    let basis = input.basis.as_str();
    let pay_frequency = input.pay_frequency.as_str();
    let first_payment_on = input.first_payment_on.to_string();
    let starts_on = input.starts_on.to_string();
    let ends_on = input.ends_on.as_ref().map(|d| d.to_string());
    let notes = input.notes.as_deref();
    let match_pattern = input
        .match_pattern
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    let pay_treatment = input.pay_treatment.as_str();
    let updated = sqlx::query!(
        "UPDATE income_streams SET
            label=?2, employer=?3, currency_code=?4, annual_amount_minor=?5, basis=?6,
            pay_frequency=?7, first_payment_on=?8, starts_on=?9, ends_on=?10,
            annual_increase_bps=?11, kiwisaver_bps=?12, student_loan=?13, take_home_bps=?14,
            linked_category_id=?15, enabled=?16, sort_order=?17, notes=?18,
            employer_kiwisaver_bps=?19, kiwisaver_account_id=?20, student_loan_account_id=?21,
            match_account_id=?22, match_pattern=?23, pay_treatment=?24,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1",
        id,
        label,
        employer,
        currency_code,
        annual_amount_minor,
        basis,
        pay_frequency,
        first_payment_on,
        starts_on,
        ends_on,
        input.annual_increase_bps,
        input.kiwisaver_bps,
        input.student_loan,
        input.take_home_bps,
        input.linked_category_id,
        input.enabled,
        input.sort_order,
        notes,
        input.employer_kiwisaver_bps,
        input.kiwisaver_account_id,
        input.student_loan_account_id,
        input.match_account_id,
        match_pattern,
        pay_treatment
    )
    .execute(&mut *txn)
    .await
    .map_err(fk_error)?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound("income stream"));
    }
    replace_steps(&mut txn, id, &input).await?;
    txn.commit().await?;
    get(db, id).await
}

/// Delete a stream.
///
/// Refused with a 409 naming the forecast events whose effects target it, rather than left to the
/// `ON DELETE RESTRICT`: a bare constraint failure tells the user nothing about which event to fix
/// first. This is `people::delete`'s pattern, and it fires here for the same reason — an effect
/// pointing at a deleted stream would become a silent no-op the forecast keeps pretending to model.
///
/// The lookup is guarded so this still works before 0022 creates that table: a stream can be
/// deleted in the release that ships streams alone.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    if let Some(blockers) = blocking_events(db, id).await? {
        return Err(AppError::conflict(format!(
            "Remove or repoint the forecast changes that use this income first: {blockers}"
        )));
    }
    let res = sqlx::query!("DELETE FROM income_streams WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("income stream"));
    }
    Ok(())
}

/// Labels of the forecast events whose effects target `stream_id`, or `None` if there are none.
async fn blocking_events(db: &Db, stream_id: i64) -> AppResult<Option<String>> {
    let exists = sqlx::query_scalar!(
        r#"SELECT 1 AS "one!" FROM sqlite_master
            WHERE type='table' AND name='forecast_event_effects'"#
    )
    .fetch_optional(db)
    .await?;
    if exists.is_none() {
        return Ok(None);
    }
    let labels = sqlx::query_scalar!(
        "SELECT DISTINCT e.label
           FROM forecast_event_effects f
           JOIN forecast_events e ON e.id = f.event_id
          WHERE f.income_stream_id = ?1
          ORDER BY e.label",
        stream_id
    )
    .fetch_all(db)
    .await?;
    if labels.is_empty() {
        return Ok(None);
    }
    Ok(Some(crate::people::summarise(&labels)))
}

// ---- income payments -------------------------------------------------------
//
// One row per expected payment per stream (`UNIQUE(income_stream_id, due_on)` — the `cron_runs`
// idempotence shape), claiming at most one transaction each. Several rows may share a
// transaction: a bonus paid inside the salary run is two streams landing in one deposit.

#[derive(Debug)]
struct IncomePaymentRow {
    id: i64,
    income_stream_id: i64,
    due_on: String,
    status: String,
    transaction_id: Option<i64>,
    matched_by: Option<String>,
    expected_net_minor: Option<i64>,
    observed_net_minor: Option<i64>,
    gross_minor: Option<i64>,
    income_tax_minor: Option<i64>,
    acc_levy_minor: Option<i64>,
    kiwisaver_minor: Option<i64>,
    student_loan_minor: Option<i64>,
    employer_kiwisaver_minor: Option<i64>,
    esct_minor: Option<i64>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<IncomePaymentRow> for IncomePayment {
    type Error = AppError;

    fn try_from(r: IncomePaymentRow) -> AppResult<IncomePayment> {
        // Same contract as `into_stream`: every writer goes through `as_str`, so an unparseable
        // value is a foreign write to surface, not a default to reach for.
        let bad = |e: String| AppError::Internal(anyhow::anyhow!(e));
        let status: IncomePaymentStatus = r.status.parse().map_err(bad)?;
        let matched_by: Option<MatchedBy> = match r.matched_by {
            Some(m) => Some(m.parse().map_err(bad)?),
            None => None,
        };
        Ok(IncomePayment {
            id: r.id,
            income_stream_id: r.income_stream_id,
            due_on: r.due_on,
            status,
            transaction_id: r.transaction_id,
            matched_by,
            expected_net_minor: r.expected_net_minor,
            observed_net_minor: r.observed_net_minor,
            gross_minor: r.gross_minor,
            income_tax_minor: r.income_tax_minor,
            acc_levy_minor: r.acc_levy_minor,
            kiwisaver_minor: r.kiwisaver_minor,
            student_loan_minor: r.student_loan_minor,
            employer_kiwisaver_minor: r.employer_kiwisaver_minor,
            esct_minor: r.esct_minor,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// Payments, filtered. Every filter optional and fixed-shape (`?N IS NULL OR …`), so the query
/// stays compile-time checked rather than joining the `QueryBuilder` holdouts.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_payments(
    db: &Db,
    from: Option<&str>,
    to: Option<&str>,
    person_id: Option<i64>,
    status: Option<IncomePaymentStatus>,
) -> AppResult<Vec<IncomePayment>> {
    let status = status.map(IncomePaymentStatus::as_str);
    let rows = sqlx::query_as!(
        IncomePaymentRow,
        r#"SELECT p.id AS "id!", p.income_stream_id, p.due_on, p.status, p.transaction_id,
                  p.matched_by, p.expected_net_minor, p.observed_net_minor, p.gross_minor,
                  p.income_tax_minor, p.acc_levy_minor, p.kiwisaver_minor, p.student_loan_minor,
                  p.employer_kiwisaver_minor, p.esct_minor, p.created_at, p.updated_at
             FROM income_payments p
             JOIN income_streams s ON s.id = p.income_stream_id
            WHERE (?1 IS NULL OR p.due_on >= ?1)
              AND (?2 IS NULL OR p.due_on <= ?2)
              AND (?3 IS NULL OR s.person_id = ?3)
              AND (?4 IS NULL OR p.status = ?4)
            ORDER BY p.due_on DESC, p.income_stream_id"#,
        from,
        to,
        person_id,
        status
    )
    .fetch_all(db)
    .await?;
    rows.into_iter().map(IncomePayment::try_from).collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get_payment(db: &Db, id: i64) -> AppResult<IncomePayment> {
    let row = sqlx::query_as!(
        IncomePaymentRow,
        r#"SELECT id AS "id!", income_stream_id, due_on, status, transaction_id, matched_by,
                  expected_net_minor, observed_net_minor, gross_minor, income_tax_minor,
                  acc_levy_minor, kiwisaver_minor, student_loan_minor, employer_kiwisaver_minor,
                  esct_minor, created_at, updated_at
             FROM income_payments WHERE id=?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("income payment"))?;
    row.try_into()
}

/// Ensure an `expected` row exists for `(stream, due_on)`, refreshing its predicted net.
///
/// The conflict arm updates only rows still `expected`: a matched, confirmed or dismissed row is
/// settled history, and regenerating the schedule must be able to run over it forever without
/// touching it — that is the whole idempotence contract.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert_expected(
    db: &Db,
    stream_id: i64,
    due_on: &str,
    expected_net_minor: i64,
) -> AppResult<()> {
    sqlx::query!(
        "INSERT INTO income_payments (income_stream_id, due_on, status, expected_net_minor)
         VALUES (?1, ?2, 'expected', ?3)
         ON CONFLICT(income_stream_id, due_on) DO UPDATE
            SET expected_net_minor = excluded.expected_net_minor,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE income_payments.status = 'expected'",
        stream_id,
        due_on,
        expected_net_minor
    )
    .execute(db)
    .await?;
    Ok(())
}

/// The `expected` rows of one stream — what a regeneration run diffs against the current
/// schedule to find strays.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn expected_due_ons(db: &Db, stream_id: i64) -> AppResult<Vec<String>> {
    Ok(sqlx::query_scalar!(
        "SELECT due_on FROM income_payments
          WHERE income_stream_id=?1 AND status='expected' ORDER BY due_on",
        stream_id
    )
    .fetch_all(db)
    .await?)
}

/// Delete one stray `expected` row — a date the (since edited) schedule no longer contains.
/// Guarded on status so a race with a match cannot delete settled history.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_expected(db: &Db, stream_id: i64, due_on: &str) -> AppResult<()> {
    sqlx::query!(
        "DELETE FROM income_payments
          WHERE income_stream_id=?1 AND due_on=?2 AND status='expected'",
        stream_id,
        due_on
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Claim a transaction for `(stream, due_on)`, recording the observed slice and its
/// reconstructed decomposition. The caller owns the arithmetic; this stores it.
#[tracing::instrument(level = "debug", skip_all)]
#[allow(clippy::too_many_arguments)] // one write, one row: bundling these into a struct would just move the field list
pub async fn record_match(
    db: &Db,
    stream_id: i64,
    due_on: &str,
    transaction_id: i64,
    matched_by: MatchedBy,
    status: IncomePaymentStatus,
    observed_net_minor: i64,
    breakdown: &PayeBreakdown,
) -> AppResult<IncomePayment> {
    let matched_by = matched_by.as_str();
    let status = status.as_str();
    let id = sqlx::query_scalar!(
        r#"UPDATE income_payments
              SET status=?3, transaction_id=?4, matched_by=?5, observed_net_minor=?6,
                  gross_minor=?7, income_tax_minor=?8, acc_levy_minor=?9, kiwisaver_minor=?10,
                  student_loan_minor=?11, employer_kiwisaver_minor=?12, esct_minor=?13,
                  updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
            WHERE income_stream_id=?1 AND due_on=?2
        RETURNING id AS "id!""#,
        stream_id,
        due_on,
        status,
        transaction_id,
        matched_by,
        observed_net_minor,
        breakdown.gross_minor,
        breakdown.income_tax_minor,
        breakdown.acc_levy_minor,
        breakdown.kiwisaver_minor,
        breakdown.student_loan_minor,
        breakdown.employer_kiwisaver_minor,
        breakdown.esct_minor
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("income payment"))?;
    get_payment(db, id).await
}

/// Undo a match: back to `expected`, decomposition cleared, the transaction released.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn unlink_payment(db: &Db, id: i64) -> AppResult<IncomePayment> {
    let res = sqlx::query!(
        "UPDATE income_payments
            SET status='expected', transaction_id=NULL, matched_by=NULL,
                observed_net_minor=NULL, gross_minor=NULL, income_tax_minor=NULL,
                acc_levy_minor=NULL, kiwisaver_minor=NULL, student_loan_minor=NULL,
                employer_kiwisaver_minor=NULL, esct_minor=NULL,
                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id=?1",
        id
    )
    .execute(db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("income payment"));
    }
    get_payment(db, id).await
}

/// Move a payment between the human-owned statuses (confirm a match, dismiss an expected pay,
/// re-open a dismissal). The matcher never calls this.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn set_payment_status(
    db: &Db,
    id: i64,
    status: IncomePaymentStatus,
) -> AppResult<IncomePayment> {
    let status = status.as_str();
    let res = sqlx::query!(
        "UPDATE income_payments
            SET status=?2, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE id=?1",
        id,
        status
    )
    .execute(db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("income payment"));
    }
    get_payment(db, id).await
}

/// Repair rows whose claimed transaction is gone — an undone import deletes by provider tag and
/// `ON DELETE SET NULL` leaves the match pointing at nothing. Back to `expected` with the
/// decomposition cleared, so neither a report nor the review UI shows income backed by no
/// deposit. Returns how many were repaired, for the matcher's log line.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn reset_orphaned_payments(db: &Db) -> AppResult<u64> {
    let res = sqlx::query!(
        "UPDATE income_payments
            SET status='expected', matched_by=NULL, observed_net_minor=NULL, gross_minor=NULL,
                income_tax_minor=NULL, acc_levy_minor=NULL, kiwisaver_minor=NULL,
                student_loan_minor=NULL, employer_kiwisaver_minor=NULL, esct_minor=NULL,
                updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
          WHERE transaction_id IS NULL AND status IN ('matched','confirmed')"
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

/// Transaction ids already claimed by any live match — the matcher's exclusion list, so one
/// deposit cannot satisfy two paydays (greedy one-to-one, as `import::routing` scores).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn claimed_transaction_ids(db: &Db) -> AppResult<Vec<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT transaction_id AS "transaction_id!" FROM income_payments
            WHERE transaction_id IS NOT NULL"#
    )
    .fetch_all(db)
    .await?)
}

/// The latest settled (non-`expected`) due date of a stream — where schedule regeneration
/// resumes from, so a stream matched for years does not re-enumerate its whole history.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn latest_settled_due_on(db: &Db, stream_id: i64) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT MAX(due_on) AS "due_on?" FROM income_payments
            WHERE income_stream_id=?1 AND status != 'expected'"#,
        stream_id
    )
    .fetch_one(db)
    .await?)
}

/// One matched payment as a report consumes it: the decomposition plus who earned it, joined
/// through the live transaction row — a payment whose transaction is gone is invisible here by
/// construction, never ghost income.
#[derive(Debug, Clone)]
pub struct MatchedPaymentRow {
    pub income_stream_id: i64,
    pub stream_label: String,
    pub person_id: i64,
    pub person_name: String,
    pub transaction_id: i64,
    pub observed_net_minor: i64,
    pub gross_minor: i64,
    pub income_tax_minor: i64,
    pub acc_levy_minor: i64,
    pub kiwisaver_minor: i64,
    pub student_loan_minor: i64,
}

/// Every matched/confirmed payment with a live transaction and a stored decomposition.
///
/// Unwindowed: the sankey already loads the window's transactions, and it filters these by the
/// transaction ids it actually kept — date, attribution and one-off rules then apply in exactly
/// one place instead of two that could disagree.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn matched_payments(db: &Db) -> AppResult<Vec<MatchedPaymentRow>> {
    Ok(sqlx::query_as!(
        MatchedPaymentRow,
        r#"SELECT p.income_stream_id, s.label AS stream_label, s.person_id,
                  pe.name AS person_name,
                  p.transaction_id AS "transaction_id!", p.observed_net_minor AS "observed_net_minor!",
                  p.gross_minor AS "gross_minor!", p.income_tax_minor AS "income_tax_minor!",
                  p.acc_levy_minor AS "acc_levy_minor!", p.kiwisaver_minor AS "kiwisaver_minor!",
                  p.student_loan_minor AS "student_loan_minor!"
             FROM income_payments p
             JOIN income_streams s ON s.id = p.income_stream_id
             JOIN people pe ON pe.id = s.person_id
             JOIN transactions t ON t.id = p.transaction_id
            WHERE p.status IN ('matched','confirmed')
              AND p.observed_net_minor IS NOT NULL
              AND p.gross_minor IS NOT NULL
            ORDER BY p.due_on, p.income_stream_id"#
    )
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::{IsoDate, Money, SaveIncomeStreamStep};

    async fn test_db() -> Db {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    async fn a_person(db: &Db) -> i64 {
        crate::people::create(
            db,
            sure_core::SavePerson {
                name: "Rua".into(),
                color: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn stream(label: &str) -> SaveIncomeStream {
        SaveIncomeStream {
            label: label.into(),
            employer: Some("Kaimahi Collective".into()),
            currency_code: "NZD".into(),
            annual_amount_minor: Money::new(88_000_00).unwrap(),
            basis: IncomeBasis::GrossNzPaye,
            pay_frequency: PayFrequency::Fortnightly,
            first_payment_on: IsoDate::parse("2026-04-03").unwrap(),
            starts_on: IsoDate::parse("2026-04-01").unwrap(),
            ends_on: None,
            annual_increase_bps: 0,
            kiwisaver_bps: 350,
            employer_kiwisaver_bps: 350,
            student_loan: true,
            take_home_bps: None,
            linked_category_id: None,
            kiwisaver_account_id: None,
            student_loan_account_id: None,
            match_account_id: None,
            match_pattern: None,
            pay_treatment: PayTreatment::Regular,
            enabled: true,
            sort_order: 0,
            notes: None,
            steps: vec![],
        }
    }

    #[tokio::test]
    async fn a_stream_round_trips_with_its_steps_in_date_order() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let mut input = stream("Teaching");
        // Deliberately out of order on the way in: a pay scale is read in date order however it
        // was entered.
        input.steps = vec![
            SaveIncomeStreamStep {
                effective_on: IsoDate::parse("2028-04-01").unwrap(),
                annual_amount_minor: Money::new(96_000_00).unwrap(),
                label: Some("Step 6".into()),
            },
            SaveIncomeStreamStep {
                effective_on: IsoDate::parse("2027-04-01").unwrap(),
                annual_amount_minor: Money::new(92_000_00).unwrap(),
                label: Some("Step 5".into()),
            },
        ];
        let created = create(&db, person, input).await.unwrap();
        assert_eq!(created.steps.len(), 2);
        assert_eq!(created.steps[0].effective_on, "2027-04-01");
        assert_eq!(created.steps[1].effective_on, "2028-04-01");
        assert_eq!(created.basis, IncomeBasis::GrossNzPaye);
        assert_eq!(created.pay_frequency, PayFrequency::Fortnightly);
        assert!(created.student_loan);

        let listed = list(&db).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].steps.len(), 2);
    }

    /// A full-replace update means an omitted step is deleted — otherwise removing a step from a
    /// scale would be impossible through the API.
    #[tokio::test]
    async fn updating_replaces_the_whole_schedule() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let mut input = stream("Teaching");
        input.steps = vec![SaveIncomeStreamStep {
            effective_on: IsoDate::parse("2027-04-01").unwrap(),
            annual_amount_minor: Money::new(92_000_00).unwrap(),
            label: None,
        }];
        let created = create(&db, person, input).await.unwrap();

        let mut next = stream("Teaching");
        next.steps = vec![];
        let updated = update(&db, created.id, next).await.unwrap();
        assert!(updated.steps.is_empty());
    }

    #[tokio::test]
    async fn two_steps_on_one_date_are_named_rather_than_left_to_the_index() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let mut input = stream("Teaching");
        let dup = SaveIncomeStreamStep {
            effective_on: IsoDate::parse("2027-04-01").unwrap(),
            annual_amount_minor: Money::new(92_000_00).unwrap(),
            label: None,
        };
        input.steps = vec![dup.clone(), dup];
        let err = create(&db, person, input).await.unwrap_err();
        assert!(
            format!("{err:?}").contains("2027-04-01"),
            "the error should name the clashing date, got {err:?}"
        );
    }

    /// Every problem in one response, not the first one found.
    #[tokio::test]
    async fn validation_collects_every_problem_at_once() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let mut input = stream("  ");
        input.kiwisaver_bps = 50_000;
        input.take_home_bps = Some(20_000);
        let err = create(&db, person, input).await.unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("label"), "{msg}");
        assert!(msg.contains("kiwisaver_bps"), "{msg}");
        assert!(msg.contains("take_home_bps"), "{msg}");
    }

    #[tokio::test]
    async fn an_unknown_person_is_a_validation_error_not_a_dangling_stream() {
        let db = test_db().await;
        let err = create(&db, 9_999, stream("Ghost")).await.unwrap_err();
        assert!(format!("{err:?}").contains("does not exist"), "{err:?}");
    }

    #[tokio::test]
    async fn deleting_a_missing_stream_is_a_not_found() {
        let db = test_db().await;
        assert!(delete(&db, 4_242).await.is_err());
    }

    #[tokio::test]
    async fn half_a_match_config_is_refused_with_both_halves_named() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let mut input = stream("Teaching");
        input.match_pattern = Some("KAIMAHI".into());
        let err = create(&db, person, input).await.unwrap_err();
        assert!(
            format!("{err:?}").contains("match_account_id"),
            "should name the missing half, got {err:?}"
        );
    }

    // ---- payments ----------------------------------------------------------------

    async fn an_account(db: &Db) -> i64 {
        crate::accounts::create(
            db,
            sure_core::SaveAccount {
                name: "Everyday".into(),
                kind: sure_core::AccountKind::Bank,
                institution: Some("ANZ".into()),
                currency_code: "NZD".into(),
                metadata: None,
                archived: false,
                sort_order: 0,
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".into()),
                ownership: sure_core::Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn a_deposit(db: &Db, account_id: i64, posted_at: &str, amount_minor: i64) -> i64 {
        crate::transactions::create(
            db,
            sure_core::SaveTransaction {
                account_id,
                posted_at: IsoDate::parse(posted_at).unwrap(),
                amount_minor: Money::new(amount_minor).unwrap(),
                currency_code: Some("NZD".into()),
                description: "KAIMAHI COLLECTIVE SALARY".into(),
                merchant: None,
                notes: None,
                category_id: None,
                is_one_off: false,
                merchant_id: None,
                ownership: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn breakdown(observed_net: i64) -> PayeBreakdown {
        // Any reconciling figures do for storage tests — the arithmetic has its own tests in
        // sure-core.
        PayeBreakdown {
            gross_minor: observed_net + 1_000_00,
            income_tax_minor: 800_00,
            acc_levy_minor: 50_00,
            kiwisaver_minor: 100_00,
            student_loan_minor: 50_00,
            net_minor: observed_net,
            employer_kiwisaver_minor: 100_00,
            esct_minor: 30_00,
            govt_contribution_minor: 0,
            kiwisaver_credited_minor: 170_00,
        }
    }

    #[tokio::test]
    async fn a_payment_walks_expected_matched_unlinked_and_back() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let account = an_account(&db).await;
        let mut input = stream("Salary");
        input.match_account_id = Some(account);
        input.match_pattern = Some("KAIMAHI".into());
        let s = create(&db, person, input).await.unwrap();

        upsert_expected(&db, s.id, "2026-05-14", 2_700_00)
            .await
            .unwrap();
        // Idempotent: a second run refreshes the prediction without duplicating the row.
        upsert_expected(&db, s.id, "2026-05-14", 2_710_00)
            .await
            .unwrap();
        let listed = list_payments(&db, None, None, Some(person), None)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, IncomePaymentStatus::Expected);
        assert_eq!(listed[0].expected_net_minor, Some(2_710_00));

        let tx = a_deposit(&db, account, "2026-05-13", 2_708_00).await;
        let matched = record_match(
            &db,
            s.id,
            "2026-05-14",
            tx,
            MatchedBy::Auto,
            IncomePaymentStatus::Matched,
            2_708_00,
            &breakdown(2_708_00),
        )
        .await
        .unwrap();
        assert_eq!(matched.status, IncomePaymentStatus::Matched);
        assert_eq!(matched.transaction_id, Some(tx));
        assert_eq!(matched.gross_minor, Some(2_708_00 + 1_000_00));
        // A settled row is invisible to regeneration…
        upsert_expected(&db, s.id, "2026-05-14", 9_999_99)
            .await
            .unwrap();
        let after = get_payment(&db, matched.id).await.unwrap();
        assert_eq!(after.status, IncomePaymentStatus::Matched);
        // …and to stray-deletion.
        delete_expected(&db, s.id, "2026-05-14").await.unwrap();
        assert!(get_payment(&db, matched.id).await.is_ok());

        assert_eq!(claimed_transaction_ids(&db).await.unwrap(), vec![tx]);
        assert_eq!(
            latest_settled_due_on(&db, s.id).await.unwrap(),
            Some("2026-05-14".into())
        );
        let for_reports = matched_payments(&db).await.unwrap();
        assert_eq!(for_reports.len(), 1);
        assert_eq!(for_reports[0].person_id, person);
        assert_eq!(for_reports[0].transaction_id, tx);

        let unlinked = unlink_payment(&db, matched.id).await.unwrap();
        assert_eq!(unlinked.status, IncomePaymentStatus::Expected);
        assert_eq!(unlinked.transaction_id, None);
        assert_eq!(unlinked.gross_minor, None);
        assert!(matched_payments(&db).await.unwrap().is_empty());
    }

    /// An undone import deletes transactions by provider tag; `ON DELETE SET NULL` must not
    /// leave a match claiming income that no longer landed.
    #[tokio::test]
    async fn an_orphaned_match_is_reset_to_expected() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let account = an_account(&db).await;
        let mut input = stream("Salary");
        input.match_account_id = Some(account);
        input.match_pattern = Some("KAIMAHI".into());
        let s = create(&db, person, input).await.unwrap();
        upsert_expected(&db, s.id, "2026-05-14", 2_700_00)
            .await
            .unwrap();
        let tx = a_deposit(&db, account, "2026-05-13", 2_700_00).await;
        let matched = record_match(
            &db,
            s.id,
            "2026-05-14",
            tx,
            MatchedBy::Auto,
            IncomePaymentStatus::Matched,
            2_700_00,
            &breakdown(2_700_00),
        )
        .await
        .unwrap();

        sqlx::query!("DELETE FROM transactions WHERE id=?1", tx)
            .execute(&db)
            .await
            .unwrap();
        assert!(matched_payments(&db).await.unwrap().is_empty());
        assert_eq!(reset_orphaned_payments(&db).await.unwrap(), 1);
        let repaired = get_payment(&db, matched.id).await.unwrap();
        assert_eq!(repaired.status, IncomePaymentStatus::Expected);
        assert_eq!(repaired.gross_minor, None);
        // Running the repair again finds nothing — it is idempotent too.
        assert_eq!(reset_orphaned_payments(&db).await.unwrap(), 0);
    }

    /// Two rows sharing one deposit: the salary-plus-bonus case the schema was shaped for.
    #[tokio::test]
    async fn two_streams_may_claim_one_transaction() {
        let db = test_db().await;
        let person = a_person(&db).await;
        let account = an_account(&db).await;
        let mut base = stream("Salary");
        base.match_account_id = Some(account);
        base.match_pattern = Some("KAIMAHI".into());
        let base = create(&db, person, base).await.unwrap();
        let mut bonus = stream("Bonus");
        bonus.pay_frequency = PayFrequency::Quarterly;
        bonus.pay_treatment = PayTreatment::ExtraPay;
        bonus.match_account_id = Some(account);
        bonus.match_pattern = Some("KAIMAHI".into());
        let bonus = create(&db, person, bonus).await.unwrap();

        upsert_expected(&db, base.id, "2026-05-14", 2_700_00)
            .await
            .unwrap();
        upsert_expected(&db, bonus.id, "2026-05-14", 1_500_00)
            .await
            .unwrap();
        let tx = a_deposit(&db, account, "2026-05-14", 4_208_00).await;
        for (sid, slice) in [(base.id, 2_708_00), (bonus.id, 1_500_00)] {
            record_match(
                &db,
                sid,
                "2026-05-14",
                tx,
                MatchedBy::Auto,
                IncomePaymentStatus::Matched,
                slice,
                &breakdown(slice),
            )
            .await
            .unwrap();
        }
        let rows = matched_payments(&db).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.transaction_id == tx));
        let total: i64 = rows.iter().map(|r| r.observed_net_minor).sum();
        assert_eq!(total, 4_208_00);
    }
}
