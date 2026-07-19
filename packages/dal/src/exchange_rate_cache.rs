//! Cache of the most recently polled exchange rate per currency pair, refreshed by the
//! background poller in `sure-api`. One row per pair, continuously overwritten — unlike
//! `exchange_rates` (see `reports.rs`), which keeps dated historical snapshots for
//! reports.

use sqlx::FromRow;
use sure_core::AppResult;

use crate::Db;

#[derive(Debug, Clone, FromRow)]
pub struct CachedExchangeRate {
    pub base_code: String,
    pub quote_code: String,
    /// Decimal text (exact), e.g. `"0.87207"`.
    pub rate: String,
    /// Upstream's reference date for the rate (ISO-8601 date).
    pub as_of: String,
    /// When this row was last refreshed (ISO-8601 timestamp, UTC).
    pub fetched_at: String,
}

/// Upsert the latest rate for one currency pair.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert(
    db: &Db,
    base_code: &str,
    quote_code: &str,
    rate: &str,
    as_of: &str,
) -> AppResult<CachedExchangeRate> {
    Ok(sqlx::query_as::<_, CachedExchangeRate>(
        "INSERT INTO exchange_rate_cache (base_code, quote_code, rate, as_of, fetched_at)
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         ON CONFLICT(base_code, quote_code) DO UPDATE SET
            rate = excluded.rate, as_of = excluded.as_of, fetched_at = excluded.fetched_at
         RETURNING *",
    )
    .bind(base_code)
    .bind(quote_code)
    .bind(rate)
    .bind(as_of)
    .fetch_one(db)
    .await?)
}

/// Every cached rate quoted against `base_code`.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_for_base(db: &Db, base_code: &str) -> AppResult<Vec<CachedExchangeRate>> {
    Ok(sqlx::query_as::<_, CachedExchangeRate>(
        "SELECT * FROM exchange_rate_cache WHERE base_code = ?1 ORDER BY quote_code",
    )
    .bind(base_code)
    .fetch_all(db)
    .await?)
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
    async fn upserts_and_lists_by_base() {
        let db = test_db().await;
        assert_eq!(list_for_base(&db, "NZD").await.unwrap().len(), 0);

        upsert(&db, "NZD", "USD", "0.6", "2026-07-16")
            .await
            .unwrap();
        upsert(&db, "NZD", "AUD", "0.92", "2026-07-16")
            .await
            .unwrap();

        let rates = list_for_base(&db, "NZD").await.unwrap();
        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].quote_code, "AUD");
        assert_eq!(rates[0].rate, "0.92");

        // Re-polling the same pair updates in place rather than duplicating the row.
        let updated = upsert(&db, "NZD", "USD", "0.61", "2026-07-17")
            .await
            .unwrap();
        assert_eq!(updated.rate, "0.61");
        assert_eq!(updated.as_of, "2026-07-17");
        assert_eq!(list_for_base(&db, "NZD").await.unwrap().len(), 2);
    }
}
