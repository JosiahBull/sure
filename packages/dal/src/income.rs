//! Per-person income streams: persistence. Which take-home rate wins is resolution logic and
//! lives in `sure-app`; this module stores and retrieves the rows.

use sure_core::{
    AppError, AppResult, IncomeBasis, IncomeStream, IncomeStreamStep, PayFrequency,
    SaveIncomeStream,
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
                  kiwisaver_account_id, student_loan_account_id, enabled AS "enabled!: bool",
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
                  kiwisaver_account_id, student_loan_account_id, enabled AS "enabled!: bool",
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
    if let Some(th) = input.take_home_bps {
        if !(0..=10_000).contains(&th) {
            problems.push(format!(
                "take_home_bps must be between 0 and 10000, got {th}"
            ));
        }
    }
    if let Some(end) = input.ends_on.as_ref() {
        if end.date() <= input.starts_on.date() {
            problems.push("ends_on must be after starts_on".into());
        }
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
            AppError::validation("person, currency or linked category does not exist")
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
    // Only the new id is wanted — the steps go in below and `get` re-reads the whole stream —
    // so this returns that rather than restating all two dozen columns.
    let id = sqlx::query_scalar!(
        r#"INSERT INTO income_streams
              (person_id, label, employer, currency_code, annual_amount_minor, basis,
               pay_frequency, first_payment_on, starts_on, ends_on, annual_increase_bps,
               kiwisaver_bps, student_loan, take_home_bps, linked_category_id, enabled,
               sort_order, notes, employer_kiwisaver_bps, kiwisaver_account_id,
               student_loan_account_id)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
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
        input.student_loan_account_id
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
    let updated = sqlx::query!(
        "UPDATE income_streams SET
            label=?2, employer=?3, currency_code=?4, annual_amount_minor=?5, basis=?6,
            pay_frequency=?7, first_payment_on=?8, starts_on=?9, ends_on=?10,
            annual_increase_bps=?11, kiwisaver_bps=?12, student_loan=?13, take_home_bps=?14,
            linked_category_id=?15, enabled=?16, sort_order=?17, notes=?18,
            employer_kiwisaver_bps=?19, kiwisaver_account_id=?20, student_loan_account_id=?21,
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
        input.student_loan_account_id
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
}
