//! The `imports` log: what a file upload did to one account, and when.
//!
//! `provider_syncs` for uploads (see `crate::providers::record_sync`), and written for the same
//! reason: the alternative to recording it is reading the transactions back and inferring it,
//! which two UI panels used to do by fetching ten thousand rows and filtering on the provider
//! tag. Append-only — nothing updates or deletes a row here except an account going away, and a
//! snapshot import clearing the log along with every other audit table.

use serde::Deserialize;

use sure_core::{AppResult, ImportRecord, ImportSource};

use crate::Db;

/// One import to record. Mirrors `sure_app::ports::NewImport`; the adapter maps between them.
#[derive(Debug, Clone)]
pub struct NewImport {
    pub account_id: i64,
    pub source: ImportSource,
    pub provider_tag: String,
    pub source_account: Option<String>,
    pub filenames: Vec<String>,
    pub imported: i64,
    pub skipped: i64,
    pub held_back: i64,
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    pub cutover: Option<String>,
}

#[derive(Debug)]
struct ImportRow {
    id: i64,
    account_id: i64,
    source: String,
    source_account: Option<String>,
    filenames: String,
    imported: i64,
    skipped: i64,
    held_back: i64,
    covered_from: Option<String>,
    covered_to: Option<String>,
    cutover: Option<String>,
    created_at: String,
}

impl TryFrom<ImportRow> for ImportRecord {
    type Error = sure_core::AppError;

    fn try_from(r: ImportRow) -> Result<Self, Self::Error> {
        // Parsed into the enum the moment it leaves the TEXT column it was stored in, so nothing
        // above this line ever handles the source as a string (CLAUDE.md rule 1).
        let source: ImportSource = r
            .source
            .parse()
            .map_err(|e: String| sure_core::AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(ImportRecord {
            id: r.id,
            account_id: r.account_id,
            source,
            source_account: r.source_account,
            // Display-only, and written by this module, so a row that somehow holds something
            // else reads as "no filenames" rather than failing the whole list.
            filenames: Vec::<String>::deserialize(
                &serde_json::from_str::<serde_json::Value>(&r.filenames).unwrap_or_default(),
            )
            .unwrap_or_default(),
            imported: r.imported,
            skipped: r.skipped,
            held_back: r.held_back,
            covered_from: r.covered_from,
            covered_to: r.covered_to,
            cutover: r.cutover,
            created_at: r.created_at,
        })
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn record(db: &Db, entry: NewImport) -> AppResult<()> {
    let source = entry.source.as_str();
    let filenames = serde_json::to_string(&entry.filenames).unwrap_or_else(|_| "[]".to_string());
    sqlx::query!(
        "INSERT INTO imports
           (account_id, source, provider_tag, source_account, filenames,
            imported, skipped, held_back, covered_from, covered_to, cutover)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        entry.account_id,
        source,
        entry.provider_tag,
        entry.source_account,
        filenames,
        entry.imported,
        entry.skipped,
        entry.held_back,
        entry.covered_from,
        entry.covered_to,
        entry.cutover
    )
    .execute(db)
    .await?;
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db, account_id: Option<i64>) -> AppResult<Vec<ImportRecord>> {
    // One query rather than the two the `SELECT *` version needed: `?1 IS NULL` makes the
    // unscoped case a bind value instead of a second statement, and the macro has to see a
    // literal string either way.
    let rows = sqlx::query_as!(
        ImportRow,
        "SELECT id, account_id, source, source_account, filenames,
                imported, skipped, held_back, covered_from, covered_to, cutover, created_at
           FROM imports
          WHERE ?1 IS NULL OR account_id = ?1
          ORDER BY created_at DESC, id DESC",
        account_id
    )
    .fetch_all(db)
    .await?;
    rows.into_iter().map(ImportRecord::try_from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{AccountKind, SaveAccount};

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    async fn account(db: &Db, name: &str) -> i64 {
        crate::accounts::create(
            db,
            SaveAccount {
                name: name.to_string(),
                kind: AccountKind::Bank,
                currency_code: "NZD".to_string(),
                institution: Some("ASB".to_string()),
                metadata: None,
                archived: false,
                sort_order: 0,
                // A bank account is asked for both, so give it both — the figures don't matter
                // here, only that the account exists to hang a log entry off.
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".parse().expect("a valid date")),
                ownership: sure_core::Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn entry(account_id: i64, source: ImportSource) -> NewImport {
        NewImport {
            account_id,
            source,
            provider_tag: source.provider_tag(account_id),
            source_account: Some("12-3456-0000123-50".to_string()),
            filenames: vec!["chequing.csv".to_string(), "savings.csv".to_string()],
            imported: 12,
            skipped: 3,
            held_back: 1,
            covered_from: Some("2019-01-01".to_string()),
            covered_to: Some("2026-08-03".to_string()),
            cutover: Some("2024-03-01".to_string()),
        }
    }

    #[tokio::test]
    async fn a_recorded_import_reads_back_with_its_source_typed_and_its_filenames_intact() {
        let db = test_db().await;
        let id = account(&db, "Everyday").await;
        record(&db, entry(id, ImportSource::AsbCsv)).await.unwrap();

        let all = list(&db, Some(id)).await.unwrap();
        assert_eq!(all.len(), 1);
        let got = &all[0];
        assert_eq!(got.source, ImportSource::AsbCsv);
        assert_eq!(got.source_account.as_deref(), Some("12-3456-0000123-50"));
        assert_eq!(got.filenames, ["chequing.csv", "savings.csv"]);
        assert_eq!((got.imported, got.skipped, got.held_back), (12, 3, 1));
        assert_eq!(got.cutover.as_deref(), Some("2024-03-01"));
        assert!(!got.created_at.is_empty());
    }

    #[tokio::test]
    async fn the_log_is_newest_first_and_scoped_to_one_account() {
        let db = test_db().await;
        let mine = account(&db, "Everyday").await;
        let theirs = account(&db, "Savings").await;
        record(&db, entry(mine, ImportSource::AsbCsv))
            .await
            .unwrap();
        record(&db, entry(theirs, ImportSource::AsbCsv))
            .await
            .unwrap();
        let mut second = entry(mine, ImportSource::CsvUpload);
        second.imported = 99;
        record(&db, second).await.unwrap();

        let for_mine = list(&db, Some(mine)).await.unwrap();
        assert_eq!(for_mine.len(), 2, "the other account's row is not here");
        assert_eq!(
            for_mine[0].imported, 99,
            "the most recent import comes first"
        );
        assert_eq!(list(&db, None).await.unwrap().len(), 3);
    }

    /// The log is per-account, so deleting the account takes its history with it rather than
    /// leaving rows pointing at nothing.
    #[tokio::test]
    async fn deleting_an_account_takes_its_import_log_with_it() {
        let db = test_db().await;
        let id = account(&db, "Everyday").await;
        record(&db, entry(id, ImportSource::AsbCsv)).await.unwrap();
        crate::accounts::delete(&db, id).await.unwrap();
        assert!(list(&db, None).await.unwrap().is_empty());
    }
}
