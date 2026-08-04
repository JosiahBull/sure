//! The one place FX SQL lives: `exchange_rates`, a dated series of "1 base = `rate` quote"
//! keyed `(base_code, quote_code, as_of)`. Both writers land here — the background poller
//! (`sure_app::tasks::exchange_rates`) via [`upsert`], and config-snapshot import via
//! `crate::snapshot` — and every conversion reads it through [`latest_per_pair`].
//!
//! It used to be two tables: the poller wrote a latest-only `exchange_rate_cache` that
//! nothing read, while conversions read `exchange_rates`, which only a restored snapshot
//! ever filled. A database that had never imported a snapshot therefore converted every
//! foreign-currency figure at parity. `0018_fx_rates_single_table.sql` folded the cache in;
//! keep the two paths joined — a rate written on one side and read on the other is the whole
//! point of this module.

use sqlx::FromRow;
use sure_core::AppResult;

use crate::Db;

/// A stored exchange rate. `rate` is kept as text (exact decimal) and parsed by the caller.
#[derive(Debug, Clone, FromRow)]
pub struct ExchangeRate {
    pub base_code: String,
    pub quote_code: String,
    /// Decimal text (exact), e.g. `"0.87207"`.
    pub rate: String,
    /// Upstream's reference date for the rate (ISO-8601 date).
    pub as_of: String,
}

/// Record the rate for one currency pair on one date.
///
/// Parameters mirror the table's column order, which is also the bind order below —
/// `as_of` before `rate`. Both are `&str`, so transposing them still compiles, stores the
/// date in `rate`, and `Fx::load`'s `parse::<f64>()` then silently drops the row, putting
/// that pair back at parity: exactly the bug consolidating these tables fixed. The tests
/// below assert on the stored *value*, not just the row count, to catch it.
///
/// Re-polling a date already on record corrects it in place rather than conflicting:
/// upstream publishes a day's reference rate before the day is out, so the first read of
/// "today" can legitimately be revised by a later one.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert(
    db: &Db,
    base_code: &str,
    quote_code: &str,
    as_of: &str,
    rate: &str,
) -> AppResult<ExchangeRate> {
    Ok(sqlx::query_as::<_, ExchangeRate>(
        "INSERT INTO exchange_rates (base_code, quote_code, as_of, rate)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(base_code, quote_code, as_of) DO UPDATE SET rate = excluded.rate
         RETURNING base_code, quote_code, rate, as_of",
    )
    .bind(base_code)
    .bind(quote_code)
    .bind(as_of)
    .bind(rate)
    .fetch_one(db)
    .await?)
}

/// The most recent rate for each currency pair.
///
/// Reduced to one row per pair in SQL rather than by loading the whole series and letting
/// the last row win: the poller writes one row per pair per day, so "select everything
/// ordered by date" would grow by ~1,460 rows a year — fetched and thrown away on every
/// report and forecast request — while yielding the same handful of rates.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn latest_per_pair(db: &Db) -> AppResult<Vec<ExchangeRate>> {
    Ok(sqlx::query_as::<_, ExchangeRate>(
        "SELECT base_code, quote_code, rate, as_of
           FROM (SELECT base_code, quote_code, rate, as_of,
                        ROW_NUMBER() OVER (
                            PARTITION BY base_code, quote_code ORDER BY as_of DESC
                        ) AS rn
                   FROM exchange_rates)
          WHERE rn = 1
          ORDER BY base_code, quote_code",
    )
    .fetch_all(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_app::ports::{ExchangeRateRepo, FxRatesRepo};

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

    /// The seam that was missing for the whole life of the poller: what the write port
    /// stores, the read port has to return. Deliberately crosses the two traits instead of
    /// calling this module's functions — neither query was wrong, they addressed different
    /// tables, and only a test that goes in one port and out the other can see that. The
    /// assertion on the rate's value also pins [`upsert`]'s `as_of`/`rate` bind order.
    #[tokio::test]
    async fn a_polled_rate_is_visible_to_conversions() {
        let store = crate::store::SqliteStore::new(test_db().await);

        ExchangeRateRepo::upsert_rate(&store, "NZD", "USD", "2026-01-01", "0.6")
            .await
            .unwrap();

        let rates = FxRatesRepo::exchange_rates(&store).await.unwrap();
        let usd = rates
            .iter()
            .find(|r| r.base_code == "NZD" && r.quote_code == "USD")
            .expect("a polled rate must be readable by Fx::load");
        assert_eq!(usd.rate, "0.6");
    }

    #[tokio::test]
    async fn keeps_the_series_but_reads_only_the_latest_date() {
        let db = test_db().await;
        assert!(latest_per_pair(&db).await.unwrap().is_empty());

        upsert(&db, "NZD", "USD", "2026-01-01", "0.6")
            .await
            .unwrap();
        upsert(&db, "NZD", "USD", "2026-01-02", "0.61")
            .await
            .unwrap();

        // Both dates stay on record — this is a series, not a cache.
        assert_eq!(row_count(&db).await, 2);

        let latest = latest_per_pair(&db).await.unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].as_of, "2026-01-02");
        assert_eq!(latest[0].rate, "0.61");
    }

    #[tokio::test]
    async fn re_polling_the_same_date_corrects_it_in_place() {
        let db = test_db().await;
        upsert(&db, "NZD", "AUD", "2026-01-01", "0.92")
            .await
            .unwrap();
        let revised = upsert(&db, "NZD", "AUD", "2026-01-01", "0.93")
            .await
            .unwrap();
        assert_eq!(revised.rate, "0.93");

        assert_eq!(row_count(&db).await, 1);
        assert_eq!(latest_per_pair(&db).await.unwrap()[0].rate, "0.93");
    }

    async fn row_count(db: &Db) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM exchange_rates")
            .fetch_one(db)
            .await
            .unwrap()
    }
}
