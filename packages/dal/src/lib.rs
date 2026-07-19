//! Data-access layer: owns the SQLite connection pool, the embedded schema migrations,
//! and every SQL query in the app. Higher layers (the HTTP crate) call the repository
//! functions here and never touch `sqlx` directly — the persisted data types live here
//! too (they derive `FromRow`), and are re-exported by the API crate for its handlers
//! and OpenAPI document.

use std::str::FromStr;
use std::time::Duration;

// Per-entity repositories: types (request/response/rows) + queries.
pub mod accounts;
pub mod brokerage;
pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod exchange_rate_cache;
pub mod merchants;
pub mod providers;
pub mod reports;
pub mod rules;
pub mod scheduled_tasks;
pub mod settings;
pub mod snapshot;
pub mod stock_prices;
pub mod transactions;
pub mod valuations;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

/// The handle every layer above passes around. A thin alias today; a natural place to
/// grow into a wrapper type carrying repositories if the app ever needs it.
pub type Db = SqlitePool;

// Re-export the concrete pool/connection types so callers don't need a direct `sqlx`
// dependency just to name them.
pub use sqlx::sqlite::SqlitePool as Pool;
pub use sqlx::SqliteConnection;

/// Migrations are embedded at compile time from `packages/dal/migrations`, so the
/// binary and the test harness run the exact same schema with no external files.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Open (creating if necessary) a SQLite pool tuned for a low-concurrency,
/// single-family workload: WAL for concurrent reads, foreign keys enforced.
pub async fn connect(database_url: &str) -> anyhow::Result<Db> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));

    // Ensure the parent directory exists for file-backed databases.
    if let Some(parent) = options.get_filename().parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Run all pending migrations. Called on startup and by the test harness.
pub async fn migrate(pool: &Db) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}
