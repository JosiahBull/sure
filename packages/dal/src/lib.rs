//! Data-access layer: owns the SQLite connection pool, the embedded schema migrations,
//! and every SQL query in the app. Nothing above this crate touches `sqlx`: the
//! application core (`sure-app`) and the HTTP layer (`sure-api`) reach persistence only
//! through the repository ports in `sure_app::ports`, which [`store::SqliteStore`]
//! implements by delegating to the per-entity `pub` functions here. Each module keeps its
//! own `FromRow` row structs and maps them into the `sure-core` domain types, so no `sqlx`
//! type ever crosses the crate boundary.

// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `114_269_63` == $114,269.63); clippy's grouping lint fights that convention.
#![allow(clippy::inconsistent_digit_grouping)]

use std::path::PathBuf;
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
pub mod forecast;
pub mod merchants;
pub mod people;
pub mod providers;
pub mod reports;
pub mod rules;
pub mod scheduled_tasks;
pub mod settings;
pub mod snapshot;
pub mod stock_prices;
pub mod store;
pub mod transactions;
pub mod valuations;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::ConnectOptions;

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
        .busy_timeout(Duration::from_secs(5))
        // Log every executed statement at TRACE (sqlx defaults to INFO, which would
        // drown out the one-line-per-request summary); promote statements slower than a
        // second to WARN so genuinely slow queries surface without any RUST_LOG tuning.
        .log_statements(log::LevelFilter::Trace)
        .log_slow_statements(log::LevelFilter::Warn, Duration::from_secs(1));

    ensure_database_dir(database_url)?;

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Create the directory that will hold a file-backed database, and return it.
///
/// Returns `None` for `sqlite::memory:`, which has no directory to create. Separate from
/// [`connect`] (which calls it) because the sandbox has to run *before* the pool is
/// opened and needs to name that directory: SQLite writes far more than the database
/// file into it — `-wal`, `-shm`, and a `-journal` in rollback mode — so the directory,
/// not the file, is the unit a filesystem policy has to grant.
pub fn ensure_database_dir(database_url: &str) -> anyhow::Result<Option<PathBuf>> {
    let Some(file) = database_file(database_url)? else {
        return Ok(None);
    };
    // A bare `sqlite:sure.db` has an empty parent, meaning the working directory.
    let dir = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) | None => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)
        .map_err(|err| anyhow::anyhow!("could not create {}: {err}", dir.display()))?;
    Ok(Some(dir))
}

/// The on-disk path a `DATABASE_URL` resolves to, or `None` when the database is
/// in-memory and so has no directory for anything to grant.
///
/// `SqliteConnectOptions::get_filename` is not enough on its own. For a `file:` URL it
/// hands back the URI rather than a path, and for an in-memory database it synthesises
/// one — `file:sqlx-in-memory-3` — that names nothing at all. What distinguishes those
/// two, `mode=memory`, lives in the query string it drops. So in-memory is decided from
/// the URL, and the URI form is unwrapped here.
pub fn database_file(database_url: &str) -> anyhow::Result<Option<PathBuf>> {
    if is_in_memory(database_url) {
        return Ok(None);
    }
    let filename = SqliteConnectOptions::from_str(database_url)?
        .get_filename()
        .to_path_buf();
    if filename.as_os_str().is_empty() {
        return Ok(None);
    }
    let Some(uri) = filename.to_str().and_then(|f| f.strip_prefix("file:")) else {
        return Ok(Some(filename));
    };
    // Percent-escapes are legal in a SQLite URI, and decoding one by halves would create
    // a wrongly-named directory rather than fail. A plain `sqlite:/data/sure.db` avoids
    // the question entirely, so ask for that instead of guessing.
    anyhow::ensure!(
        !uri.contains('%'),
        "DATABASE_URL uses a percent-escaped file: URI ({uri}); use a plain path, or name \
         the directory in SURE_SANDBOX_WRITE_PATHS"
    );
    Ok(Some(PathBuf::from(uri)))
}

/// Whether a `DATABASE_URL` names an in-memory database: `sqlite::memory:`,
/// `sqlite://:memory:`, or any URL carrying `mode=memory`.
fn is_in_memory(database_url: &str) -> bool {
    let rest = database_url.strip_prefix("sqlite:").unwrap_or(database_url);
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    path == ":memory:" || query.split('&').any(|pair| pair == "mode=memory")
}

/// Run all pending migrations. Called on startup and by the test harness.
pub async fn migrate(pool: &Db) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_in_memory_spelling_has_no_file_on_disk() {
        // The last two matter most: sqlx reports a *filename* for both
        // (`file:sqlx-in-memory-N`, `file:memdb1`), so only the URL says they're memory.
        for url in [
            "sqlite::memory:",
            "sqlite://:memory:",
            "sqlite:file:memdb1?mode=memory&cache=shared",
        ] {
            assert_eq!(database_file(url).unwrap(), None, "{url}");
            assert_eq!(ensure_database_dir(url).unwrap(), None, "{url}");
        }
    }

    #[test]
    fn a_file_url_resolves_to_a_path() {
        for (url, expected) in [
            ("sqlite:data/sure.db", "data/sure.db"),
            ("sqlite:/data/sure.db", "/data/sure.db"),
            ("sqlite://data/sure.db", "data/sure.db"),
            // The URI form: on-disk despite the `file:` prefix sqlx echoes back.
            ("sqlite:file:/data/sure.db?mode=rwc", "/data/sure.db"),
        ] {
            assert_eq!(
                database_file(url).unwrap(),
                Some(PathBuf::from(expected)),
                "{url}"
            );
        }
    }

    #[test]
    fn a_bare_filename_resolves_to_the_working_directory() {
        // `parent()` of a bare filename is `""`, which is not a path anything can open.
        assert_eq!(
            ensure_database_dir("sqlite:sure-does-not-exist.db").unwrap(),
            Some(PathBuf::from("."))
        );
    }
}
