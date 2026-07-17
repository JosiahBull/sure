//! SQLite-backed [`sure_scheduler::TaskStateStore`]: the durable "when did this named
//! background task last run" state, so the scheduler survives process restarts without
//! redoing work ahead of schedule. Distinct from `crons`/`cron_runs`, which is a
//! user-facing recurring-adjustment ledger, not developer-defined background jobs.

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sure_scheduler::TaskStateStore;

use crate::Db;

pub struct SqliteTaskStateStore {
    db: Db,
}

impl SqliteTaskStateStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl TaskStateStore for SqliteTaskStateStore {
    async fn last_run_at(&self, task_name: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT last_run_at FROM scheduled_task_runs WHERE task_name = ?1")
                .bind(task_name)
                .fetch_optional(&self.db)
                .await?;
        let Some(text) = row else {
            return Ok(None);
        };
        Ok(Some(
            DateTime::parse_from_rfc3339(&text)?.with_timezone(&Utc),
        ))
    }

    async fn record_run(&self, task_name: &str, at: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO scheduled_task_runs (task_name, last_run_at, updated_at)
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(task_name) DO UPDATE SET
                last_run_at = excluded.last_run_at, updated_at = excluded.updated_at",
        )
        .bind(task_name)
        .bind(at.to_rfc3339_opts(SecondsFormat::Millis, true))
        .execute(&self.db)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> Db {
        // A single connection so all queries hit the same in-memory database — a pool
        // with >1 connection would give each connection its own empty :memory: db.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn round_trips_a_run_timestamp() {
        let db = test_db().await;
        let store = SqliteTaskStateStore::new(db);

        assert_eq!(store.last_run_at("exchange_rate_poll").await.unwrap(), None);

        let at = Utc::now();
        store.record_run("exchange_rate_poll", at).await.unwrap();
        let read_back = store
            .last_run_at("exchange_rate_poll")
            .await
            .unwrap()
            .unwrap();
        // Millisecond precision round-trip (see `to_rfc3339_opts` above).
        assert_eq!(read_back.timestamp_millis(), at.timestamp_millis());

        // Re-recording updates in place rather than erroring or duplicating.
        let later = at + chrono::Duration::seconds(60);
        store.record_run("exchange_rate_poll", later).await.unwrap();
        let read_back = store
            .last_run_at("exchange_rate_poll")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read_back.timestamp_millis(), later.timestamp_millis());
    }
}
