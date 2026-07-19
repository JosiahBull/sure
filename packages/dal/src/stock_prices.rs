//! Historical daily stock price cache, refreshed by the background poller in
//! `sure-api` and backfilled on demand for a date not yet cached. One row per
//! (ticker, exchange, as_of) — unlike `exchange_rate_cache`, this table itself is the
//! historical series, since a point-in-time lookup already needs to query by date.

use sure_core::AppResult;
pub use sure_core::StockPrice;

use crate::Db;

/// Upsert one day's close for a ticker.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert(
    db: &Db,
    ticker: &str,
    exchange: &str,
    as_of: &str,
    close: &str,
    currency_code: &str,
) -> AppResult<StockPrice> {
    Ok(sqlx::query_as::<_, StockPrice>(
        "INSERT INTO stock_prices (ticker, exchange, as_of, close, currency_code)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(ticker, exchange, as_of) DO UPDATE SET
            close = excluded.close, currency_code = excluded.currency_code,
            fetched_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         RETURNING *",
    )
    .bind(ticker)
    .bind(exchange)
    .bind(as_of)
    .bind(close)
    .bind(currency_code)
    .fetch_one(db)
    .await?)
}

/// The closest cached close on or before `as_of` (the nearest preceding trading day —
/// so a weekend/holiday date still resolves to something sensible).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn get_at(
    db: &Db,
    ticker: &str,
    exchange: &str,
    as_of: &str,
) -> AppResult<Option<StockPrice>> {
    Ok(sqlx::query_as::<_, StockPrice>(
        "SELECT * FROM stock_prices WHERE ticker = ?1 AND exchange = ?2 AND as_of <= ?3
         ORDER BY as_of DESC LIMIT 1",
    )
    .bind(ticker)
    .bind(exchange)
    .bind(as_of)
    .fetch_optional(db)
    .await?)
}

/// Every cached close for a ticker, oldest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_history(db: &Db, ticker: &str, exchange: &str) -> AppResult<Vec<StockPrice>> {
    Ok(sqlx::query_as::<_, StockPrice>(
        "SELECT * FROM stock_prices WHERE ticker = ?1 AND exchange = ?2 ORDER BY as_of",
    )
    .bind(ticker)
    .bind(exchange)
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
    async fn upserts_and_lists_history() {
        let db = test_db().await;
        assert!(list_history(&db, "MEL", "NZX").await.unwrap().is_empty());

        upsert(&db, "MEL", "NZX", "2026-07-13", "5.60", "NZD")
            .await
            .unwrap();
        upsert(&db, "MEL", "NZX", "2026-07-14", "5.55", "NZD")
            .await
            .unwrap();

        let history = list_history(&db, "MEL", "NZX").await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].as_of, "2026-07-13");
        assert_eq!(history[0].close, "5.60");

        // Re-upserting the same day updates in place rather than duplicating the row.
        let updated = upsert(&db, "MEL", "NZX", "2026-07-14", "5.70", "NZD")
            .await
            .unwrap();
        assert_eq!(updated.close, "5.70");
        assert_eq!(list_history(&db, "MEL", "NZX").await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_at_resolves_to_the_nearest_preceding_trading_day() {
        let db = test_db().await;
        upsert(&db, "MEL", "NZX", "2026-07-10", "5.50", "NZD")
            .await
            .unwrap();
        upsert(&db, "MEL", "NZX", "2026-07-13", "5.60", "NZD")
            .await
            .unwrap();

        // Exact match.
        assert_eq!(
            get_at(&db, "MEL", "NZX", "2026-07-13")
                .await
                .unwrap()
                .unwrap()
                .close,
            "5.60"
        );
        // A weekend date (2026-07-11 is a Saturday) falls back to the prior trading day.
        assert_eq!(
            get_at(&db, "MEL", "NZX", "2026-07-12")
                .await
                .unwrap()
                .unwrap()
                .close,
            "5.50"
        );
        // Before any cached data.
        assert!(get_at(&db, "MEL", "NZX", "2026-07-01")
            .await
            .unwrap()
            .is_none());
        // A different ticker has nothing cached.
        assert!(get_at(&db, "AAPL", "", "2026-07-13")
            .await
            .unwrap()
            .is_none());
    }
}
