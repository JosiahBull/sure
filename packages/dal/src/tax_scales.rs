//! Editable tax scales: persistence, plus the seeding that keeps `sure_core::tax`'s constants the
//! single place the figures are written down.

use sure_core::tax::{builtin_scales, OwnedTaxScale, SaveTaxScale, StoredTaxScale, TaxScaleId};
use sure_core::{AppError, AppResult};

use crate::Db;

#[derive(Debug)]
struct TaxScaleRow {
    id: i64,
    scale_id: String,
    effective_from: String,
    brackets: String,
    acc_levy_bps: i64,
    acc_income_cap_minor: i64,
    student_loan_threshold_minor: i64,
    student_loan_rate_bps: i64,
    esct_brackets: String,
    kiwisaver_govt_match_bps: i64,
    kiwisaver_govt_max_minor: i64,
    kiwisaver_govt_income_cap_minor: Option<i64>,
    source_note: Option<String>,
    created_at: String,
    updated_at: String,
}

fn parse_bands(json: &str, field: &str) -> AppResult<Vec<(Option<i64>, i64)>> {
    serde_json::from_str(json).map_err(|e| {
        // The column is only ever written by `save` below, which validates first — so a parse
        // failure means the row was hand-edited. Reported rather than defaulted: silently falling
        // back to the built-in table would tax someone at rates they did not choose and never see.
        AppError::Internal(anyhow::anyhow!("tax scale {field} is not readable: {e}"))
    })
}

impl TryFrom<TaxScaleRow> for StoredTaxScale {
    type Error = AppError;

    fn try_from(r: TaxScaleRow) -> AppResult<Self> {
        let scale_id: TaxScaleId = r
            .scale_id
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(StoredTaxScale {
            id: r.id,
            scale_id,
            source_note: r.source_note,
            created_at: r.created_at,
            updated_at: r.updated_at,
            scale: OwnedTaxScale {
                effective_from: r.effective_from,
                brackets: parse_bands(&r.brackets, "brackets")?,
                acc_levy_bps: r.acc_levy_bps,
                acc_income_cap_minor: r.acc_income_cap_minor,
                student_loan_threshold_minor: r.student_loan_threshold_minor,
                student_loan_rate_bps: r.student_loan_rate_bps,
                esct_brackets: parse_bands(&r.esct_brackets, "esct_brackets")?,
                kiwisaver_govt_match_bps: r.kiwisaver_govt_match_bps,
                kiwisaver_govt_max_minor: r.kiwisaver_govt_max_minor,
                kiwisaver_govt_income_cap_minor: r.kiwisaver_govt_income_cap_minor,
            },
        })
    }
}

/// Every stored scale, oldest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<StoredTaxScale>> {
    sqlx::query_as!(
        TaxScaleRow,
        // Listed rather than `SELECT *`, and in `TaxScaleRow`'s field order: `query_as!` maps
        // columns to fields positionally, and this struct groups the JSON bands differently
        // from the table's column order.
        r#"SELECT id AS "id!", scale_id, effective_from, brackets, acc_levy_bps,
                  acc_income_cap_minor, student_loan_threshold_minor, student_loan_rate_bps,
                  esct_brackets, kiwisaver_govt_match_bps, kiwisaver_govt_max_minor,
                  kiwisaver_govt_income_cap_minor, source_note, created_at, updated_at
             FROM tax_scales ORDER BY scale_id, effective_from, id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

/// Copy the built-in scales in, if and only if the table is empty.
///
/// Run once after migration. The emptiness check is what makes it idempotent *and* non-destructive:
/// once someone has edited a rate, this never touches it again — including after an upgrade that
/// changes the constants, because a figure the user typed outranks one shipped in a binary.
///
/// Seeding here rather than with INSERTs in the migration is the point: the constants stay the only
/// place the numbers are written down, so there is no second copy to drift.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn seed(db: &Db) -> AppResult<()> {
    let existing = sqlx::query_scalar!("SELECT COUNT(*) FROM tax_scales")
        .fetch_one(db)
        .await?;
    if existing > 0 {
        return Ok(());
    }
    for scale in builtin_scales(TaxScaleId::NzPaye) {
        insert(
            db,
            TaxScaleId::NzPaye,
            &scale,
            Some(
                "Seeded from the built-in New Zealand figures: income tax and student loan from \
                 ird.govt.nz, ACC levy and cap and ESCT thresholds from published 2026/27 tables, \
                 all read 2026-08-05.",
            ),
        )
        .await?;
    }
    tracing::info!("seeded built-in tax scales");
    Ok(())
}

async fn insert(
    db: &Db,
    scale_id: TaxScaleId,
    scale: &OwnedTaxScale,
    source_note: Option<&str>,
) -> AppResult<StoredTaxScale> {
    let scale_id = scale_id.as_str();
    let brackets =
        serde_json::to_string(&scale.brackets).map_err(|e| AppError::Internal(e.into()))?;
    let esct_brackets =
        serde_json::to_string(&scale.esct_brackets).map_err(|e| AppError::Internal(e.into()))?;
    let row = sqlx::query_as!(
        TaxScaleRow,
        r#"INSERT INTO tax_scales
              (scale_id, effective_from, brackets, acc_levy_bps, acc_income_cap_minor,
               student_loan_threshold_minor, student_loan_rate_bps, esct_brackets, source_note,
               kiwisaver_govt_match_bps, kiwisaver_govt_max_minor, kiwisaver_govt_income_cap_minor)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
           RETURNING id AS "id!", scale_id, effective_from, brackets, acc_levy_bps,
                     acc_income_cap_minor, student_loan_threshold_minor, student_loan_rate_bps,
                     esct_brackets, kiwisaver_govt_match_bps, kiwisaver_govt_max_minor,
                     kiwisaver_govt_income_cap_minor, source_note, created_at, updated_at"#,
        scale_id,
        scale.effective_from,
        brackets,
        scale.acc_levy_bps,
        scale.acc_income_cap_minor,
        scale.student_loan_threshold_minor,
        scale.student_loan_rate_bps,
        esct_brackets,
        source_note,
        scale.kiwisaver_govt_match_bps,
        scale.kiwisaver_govt_max_minor,
        scale.kiwisaver_govt_income_cap_minor
    )
    .fetch_one(db)
    .await
    .map_err(unique_or_other)?;
    row.try_into()
}

/// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
/// (CLAUDE.md rule 2's escape hatch).
#[allow(clippy::wildcard_enum_match_arm)]
fn unique_or_other(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_unique_violation() => AppError::conflict(
            "a tax scale already starts on that date — edit it, or pick another date",
        ),
        other => AppError::from(other),
    }
}

/// Add a scale.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(
    db: &Db,
    scale_id: TaxScaleId,
    input: SaveTaxScale,
) -> AppResult<StoredTaxScale> {
    validate(&input)?;
    insert(db, scale_id, &input.scale, input.source_note.as_deref()).await
}

/// Replace a scale.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveTaxScale) -> AppResult<StoredTaxScale> {
    validate(&input)?;
    let s = &input.scale;
    let brackets = serde_json::to_string(&s.brackets).map_err(|e| AppError::Internal(e.into()))?;
    let esct_brackets =
        serde_json::to_string(&s.esct_brackets).map_err(|e| AppError::Internal(e.into()))?;
    let source_note = input.source_note.as_deref();
    let row = sqlx::query_as!(
        TaxScaleRow,
        r#"UPDATE tax_scales SET
              effective_from=?2, brackets=?3, acc_levy_bps=?4, acc_income_cap_minor=?5,
              student_loan_threshold_minor=?6, student_loan_rate_bps=?7, esct_brackets=?8,
              source_note=?9, kiwisaver_govt_match_bps=?10, kiwisaver_govt_max_minor=?11,
              kiwisaver_govt_income_cap_minor=?12,
              updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id=?1
           RETURNING id AS "id!", scale_id, effective_from, brackets, acc_levy_bps,
                     acc_income_cap_minor, student_loan_threshold_minor, student_loan_rate_bps,
                     esct_brackets, kiwisaver_govt_match_bps, kiwisaver_govt_max_minor,
                     kiwisaver_govt_income_cap_minor, source_note, created_at, updated_at"#,
        id,
        s.effective_from,
        brackets,
        s.acc_levy_bps,
        s.acc_income_cap_minor,
        s.student_loan_threshold_minor,
        s.student_loan_rate_bps,
        esct_brackets,
        source_note,
        s.kiwisaver_govt_match_bps,
        s.kiwisaver_govt_max_minor,
        s.kiwisaver_govt_income_cap_minor
    )
    .fetch_optional(db)
    .await
    .map_err(unique_or_other)?
    .ok_or(AppError::NotFound("tax scale"))?;
    row.try_into()
}

/// Remove a scale.
///
/// Refused when it is the last one for its jurisdiction: an empty table means every gross salary
/// silently becomes untaxed, which looks like a windfall rather than a mistake. Deleting them all to
/// get the built-ins back is a reasonable instinct, so the message says how to actually do that.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let scale_id = sqlx::query_scalar!("SELECT scale_id FROM tax_scales WHERE id=?1", id)
        .fetch_optional(db)
        .await?;
    let Some(scale_id) = scale_id else {
        return Err(AppError::NotFound("tax scale"));
    };
    let remaining = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM tax_scales WHERE scale_id=?1 AND id<>?2",
        scale_id,
        id
    )
    .fetch_one(db)
    .await?;
    if remaining == 0 {
        return Err(AppError::conflict(
            "This is the only tax scale left, and removing it would tax every gross salary at \
             nothing. Add its replacement first, or use Restore defaults.",
        ));
    }
    sqlx::query!("DELETE FROM tax_scales WHERE id=?1", id)
        .execute(db)
        .await?;
    Ok(())
}

/// Throw away every stored scale and re-seed from the built-in figures.
///
/// The way back from an edit that went wrong, and the reason `seed` can be conservative about never
/// overwriting: there is an explicit, obvious button for "I want the shipped numbers again".
#[tracing::instrument(level = "debug", skip_all)]
pub async fn restore_defaults(db: &Db) -> AppResult<Vec<StoredTaxScale>> {
    let mut txn = db.begin().await?;
    sqlx::query!("DELETE FROM tax_scales")
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;
    seed(db).await?;
    list(db).await
}

fn validate(input: &SaveTaxScale) -> AppResult<()> {
    input
        .scale
        .validate()
        .map_err(|problems| AppError::validation(problems.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> Db {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    /// Seeding is idempotent and never overwrites — a figure the user typed outranks one shipped in
    /// a binary, including across an upgrade that changes the constants.
    #[tokio::test]
    async fn seeding_fills_an_empty_table_once_and_never_again() {
        let db = test_db().await;
        // `migrate` already seeds, so the table is populated before this test does anything.
        let first = list(&db).await.unwrap();
        assert!(!first.is_empty(), "migration should have seeded");

        let edited = update(
            &db,
            first[0].id,
            SaveTaxScale {
                scale: OwnedTaxScale {
                    acc_levy_bps: 999,
                    ..builtin_scales(TaxScaleId::NzPaye)[0].clone()
                },
                source_note: Some("mine".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(edited.scale.acc_levy_bps, 999);

        seed(&db).await.unwrap();
        let after = list(&db).await.unwrap();
        assert_eq!(after.len(), first.len(), "seeding must not duplicate");
        assert_eq!(
            after[0].scale.acc_levy_bps, 999,
            "seeding must not overwrite"
        );
    }

    #[tokio::test]
    async fn the_seeded_scales_match_the_built_in_figures() {
        let db = test_db().await;
        let stored = list(&db).await.unwrap();
        let builtin = builtin_scales(TaxScaleId::NzPaye);
        assert_eq!(stored.len(), builtin.len());
        for (s, b) in stored.iter().zip(builtin.iter()) {
            assert_eq!(&s.scale, b, "seeded scale differs from the constant");
        }
        // The top band is open-ended, which is what stops income above it being untaxed.
        assert!(stored[0].scale.brackets.last().unwrap().0.is_none());
    }

    #[tokio::test]
    async fn an_unusable_scale_is_refused_with_every_problem_named() {
        let db = test_db().await;
        let err = create(
            &db,
            TaxScaleId::NzPaye,
            SaveTaxScale {
                scale: OwnedTaxScale {
                    effective_from: "not-a-date".into(),
                    // Descending, and closed at the top.
                    brackets: vec![(Some(50_000_00), 1_050), (Some(10_000_00), 1_750)],
                    acc_levy_bps: 50_000,
                    acc_income_cap_minor: 1,
                    student_loan_threshold_minor: 1,
                    student_loan_rate_bps: 1_200,
                    esct_brackets: vec![(None, 3_300)],
                    kiwisaver_govt_match_bps: 2_500,
                    kiwisaver_govt_max_minor: 260_72,
                    kiwisaver_govt_income_cap_minor: Some(180_000_00),
                },
                source_note: None,
            },
        )
        .await
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("ascending"), "{msg}");
        assert!(msg.contains("open-ended"), "{msg}");
        assert!(msg.contains("acc_levy_bps"), "{msg}");
        assert!(msg.contains("effective_from"), "{msg}");
    }

    #[tokio::test]
    async fn two_scales_cannot_start_on_the_same_day() {
        let db = test_db().await;
        let existing = list(&db).await.unwrap();
        let err = create(
            &db,
            TaxScaleId::NzPaye,
            SaveTaxScale {
                scale: existing[0].scale.clone(),
                source_note: None,
            },
        )
        .await
        .unwrap_err();
        assert!(format!("{err:?}").contains("already starts"), "{err:?}");
    }

    /// An empty table would tax every gross salary at nothing, which reads as a windfall rather
    /// than a mistake.
    #[tokio::test]
    async fn the_last_scale_cannot_be_deleted() {
        let db = test_db().await;
        let all = list(&db).await.unwrap();
        for s in all.iter().take(all.len() - 1) {
            delete(&db, s.id).await.unwrap();
        }
        let last = list(&db).await.unwrap();
        assert_eq!(last.len(), 1);
        assert!(delete(&db, last[0].id).await.is_err());
    }

    #[tokio::test]
    async fn restoring_defaults_undoes_an_edit() {
        let db = test_db().await;
        let first = list(&db).await.unwrap();
        update(
            &db,
            first[0].id,
            SaveTaxScale {
                scale: OwnedTaxScale {
                    acc_levy_bps: 1,
                    ..first[0].scale.clone()
                },
                source_note: None,
            },
        )
        .await
        .unwrap();
        let restored = restore_defaults(&db).await.unwrap();
        assert_eq!(restored.len(), builtin_scales(TaxScaleId::NzPaye).len());
        assert_eq!(restored[0].scale, builtin_scales(TaxScaleId::NzPaye)[0]);
    }
}
