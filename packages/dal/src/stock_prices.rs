//! Historical daily stock price cache, refreshed by the background poller in
//! `sure-api` and backfilled on demand for a date not yet cached. One row per
//! (ticker, exchange, as_of) — this table itself is the historical series, since a
//! point-in-time lookup already needs to query by date. `exchange_rates` is the same shape
//! for the same reason (it once had a latest-only cache beside it; that ended badly — see
//! `crate::exchange_rates`).

pub use sure_core::StockPrice;
use sure_core::{AppError, AppResult};

use crate::Db;

#[derive(Debug)]
struct StockPriceRow {
    ticker: String,
    exchange: String,
    as_of: String,
    close: String,
    currency_code: String,
    fetched_at: String,
}

impl From<StockPriceRow> for StockPrice {
    fn from(r: StockPriceRow) -> Self {
        StockPrice {
            ticker: r.ticker,
            exchange: r.exchange,
            as_of: r.as_of,
            close: r.close,
            currency_code: r.currency_code,
            fetched_at: r.fetched_at,
        }
    }
}

/// Upsert one day's close for a ticker.
///
/// `currency_code` arrives from an upstream quote, so it is whatever the feed said — it is
/// canonicalised (trim + upper-case, the form `currencies.code` stores) and checked against
/// `currencies` *before* the insert, exactly as `accounts::validate`, `brokerage::create_lot`
/// and `settings::update` do. Two failures this prevents: a feed answering `usd` for a table
/// keyed on `USD` would violate the FK for no reason at all, and an unknown code (a feed's
/// pence/cents pseudo-currency like `GBX`, or a crypto ticker) would come back as an opaque
/// `AppError::Database` FK violation naming neither the field nor the value. Callers that
/// sweep many quotes need to tell "this one quote is unusable, skip it" from "the database is
/// unhappy, stop" — a [`AppError::Validation`] says the former, and it is *only* ever the
/// former, since `currency_code` is this table's one and only foreign key. Without that
/// distinction one un-seeded currency aborted the whole poll (`sure_app::stock_prices`),
/// leaving every later ticker unpriced and — the run never being recorded — retried on every
/// check tick.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert(
    db: &Db,
    ticker: &str,
    exchange: &str,
    as_of: &str,
    close: &str,
    currency_code: &str,
) -> AppResult<StockPrice> {
    let currency = currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &currency).await? {
        return Err(unknown_currency(&currency));
    }
    Ok(sqlx::query_as!(
        StockPriceRow,
        "INSERT INTO stock_prices (ticker, exchange, as_of, close, currency_code)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(ticker, exchange, as_of) DO UPDATE SET
            close = excluded.close, currency_code = excluded.currency_code,
            fetched_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         RETURNING ticker, exchange, as_of, close, currency_code, fetched_at",
        ticker,
        exchange,
        as_of,
        close,
        currency
    )
    .fetch_one(db)
    .await
    .map_err(|e| map_unknown_currency(e, &currency))?
    .into())
}

/// The one error an unusable quote currency produces, in one place so the poll's
/// "skip this quote" classification and the check above cannot drift apart.
fn unknown_currency(currency: &str) -> AppError {
    AppError::validation(format!("unknown currency '{currency}'"))
}

/// FK backstop for the check in [`upsert`]: `stock_prices` has exactly one foreign key
/// (`currency_code -> currencies(code)`), so a violation here can only mean the currency
/// went away between the check and the insert. Reporting it as the same
/// [`AppError::Validation`] keeps a racing `DELETE /api/currencies/{code}` a skipped quote
/// rather than an aborted sweep.
// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
// (CLAUDE.md rule 2's escape hatch) — the arm above is exhaustive over our own types.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_unknown_currency(e: sqlx::Error, currency: &str) -> AppError {
    match e {
        sqlx::Error::Database(ref d) if d.is_foreign_key_violation() => unknown_currency(currency),
        other => AppError::from(other),
    }
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
    Ok(sqlx::query_as!(
        StockPriceRow,
        "SELECT ticker, exchange, as_of, close, currency_code, fetched_at
           FROM stock_prices WHERE ticker = ?1 AND exchange = ?2 AND as_of <= ?3
          ORDER BY as_of DESC LIMIT 1",
        ticker,
        exchange,
        as_of
    )
    .fetch_optional(db)
    .await?
    .map(Into::into))
}

/// Every cached close for a ticker, oldest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_history(db: &Db, ticker: &str, exchange: &str) -> AppResult<Vec<StockPrice>> {
    Ok(sqlx::query_as!(
        StockPriceRow,
        "SELECT ticker, exchange, as_of, close, currency_code, fetched_at
           FROM stock_prices WHERE ticker = ?1 AND exchange = ?2 ORDER BY as_of",
        ticker,
        exchange
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
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
    async fn upsert_rejects_an_unknown_currency_by_name_without_writing_anything() {
        let db = test_db().await;

        // `GBX` (pence) is a real thing for a feed to answer and is deliberately not a
        // `currencies` row: decimal places there drive money rendering, so the code refuses
        // the quote rather than inventing a currency definition for it.
        let err = upsert(&db, "VOD", "LSE", "2026-07-14", "72.30", "GBX")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, AppError::Validation(msg) if msg.contains("GBX")),
            "expected a validation error naming the code, got {err:?}"
        );
        // A rejected quote is not half-written.
        assert!(list_history(&db, "VOD", "LSE").await.unwrap().is_empty());

        // …and the next ticker in the same sweep still writes.
        upsert(&db, "MEL", "NZX", "2026-07-14", "5.60", "NZD")
            .await
            .unwrap();
        assert_eq!(list_history(&db, "MEL", "NZX").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn upsert_canonicalises_the_quote_currency_before_the_foreign_key_sees_it() {
        let db = test_db().await;

        // A feed answering lower case is not an unknown currency — `currencies.code` is
        // upper case, so binding the raw value would violate the FK for nothing.
        let row = upsert(&db, "AAPL", "", "2026-07-14", "210.11", " usd ")
            .await
            .unwrap();
        assert_eq!(row.currency_code, "USD");
    }

    /// The backstop for the check in `upsert`: FK enforcement is on (sqlx sets
    /// `PRAGMA foreign_keys = ON`, as does `crate::connect`), and a violation on this table
    /// can only be the currency — so it is reported as the same skippable validation error
    /// rather than an opaque database failure that would abort a caller's sweep.
    #[tokio::test]
    async fn a_currency_foreign_key_violation_is_reported_as_an_unknown_currency() {
        let db = test_db().await;
        let raw = sqlx::query!(
            "INSERT INTO stock_prices (ticker, exchange, as_of, close, currency_code)
             VALUES ('MEL', 'NZX', '2026-07-14', '5.60', 'ZZZ')"
        )
        .execute(&db)
        .await
        .expect_err("foreign keys must be enforced for the backstop to mean anything");

        let mapped = map_unknown_currency(raw, "ZZZ");
        assert!(
            matches!(&mapped, AppError::Validation(msg) if msg.contains("ZZZ")),
            "expected a validation error naming the code, got {mapped:?}"
        );
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
        assert!(
            get_at(&db, "MEL", "NZX", "2026-07-01")
                .await
                .unwrap()
                .is_none()
        );
        // A different ticker has nothing cached.
        assert!(
            get_at(&db, "AAPL", "", "2026-07-13")
                .await
                .unwrap()
                .is_none()
        );
    }
}
