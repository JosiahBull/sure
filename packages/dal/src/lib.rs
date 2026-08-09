//! Data-access layer: owns the SQLite connection pool, the embedded schema migrations,
//! and every SQL query in the app. Nothing above this crate touches `sqlx`: the
//! application core (`sure-app`) and the HTTP layer (`sure-api`) reach persistence only
//! through the repository ports in `sure_app::ports`, which [`store::SqliteStore`]
//! implements by delegating to the per-entity `pub` functions here. Each module keeps its
//! own row structs and maps them into the `sure-core` domain types, so no `sqlx` type ever
//! crosses the crate boundary.
//!
//! # Queries are compile-time checked
//!
//! Every static query here uses the `sqlx::query!` / `query_as!` / `query_scalar!` macros,
//! which check the SQL against the real schema at compile time: a column that does not
//! exist, a bind count that doesn't match, or a row struct whose field types disagree with
//! the table is a build error rather than a runtime `ColumnNotFound` on whichever request
//! first reaches it. The schema they check against is the one
//! `packages/dal/migrations` produces — see `scripts/sqlx-prepare.mjs`, which applies the
//! migrations to a throwaway database and caches the results in the committed `.sqlx/`
//! directory. **Change a query or a migration and you must re-run `pnpm sqlx:prepare`**;
//! `.githooks/pre-commit` runs `pnpm sqlx:check` and fails if you didn't.
//!
//! Three things the macros need help with, so they recur throughout this crate:
//!
//! * **`col AS "col!"`** forces a column non-null. SQLite's `describe` is conservative and
//!   reports a nullable type wherever it cannot prove otherwise — most often an
//!   `INTEGER PRIMARY KEY` (a rowid alias, which the type system considers nullable even
//!   though it never is), and any column read back out of a subquery, CTE, `GROUP BY`, or
//!   window function, which do not carry the base table's `NOT NULL` through.
//! * **`col AS "col?"`** forces a column nullable, for the mirror case: the outer side of a
//!   `LEFT JOIN`, where the column is `NOT NULL` in its own table but absent when the join
//!   misses.
//!
//! * **`col AS "col: Type"`** names the Rust type to decode into. Needed wherever SQLite
//!   reports no type at all — an aggregate over an empty table (`SUM(x)` describes as
//!   `NULL`), or `CASE`/`COALESCE` over mixed inputs — and for a `bool`, since a SQLite
//!   `INTEGER` column otherwise decodes as `i64`. It composes with the nullability forcers:
//!   `col AS "col!: bool"`.
//!
//! What that last override is deliberately *not* used for is the domain enums. Decoding a
//! `kind` column straight into `AccountKind` would mean deriving `sqlx::Type` on it, and
//! `sure-core`'s types stay free of `sqlx` on purpose (see its `Cargo.toml`): a `TEXT` column
//! is read into a `String` on the row struct and parsed into the enum by that struct's
//! `TryFrom`, which is the one place CLAUDE.md rule 1 wants the conversion to happen.
//!
//! Queries whose *shape* is decided at runtime — the transaction list's optional filters,
//! the chunked bulk inserts — cannot be macro-checked (the macro needs a literal string)
//! and use `sqlx::QueryBuilder` instead. Each such site says so; they are the only
//! unchecked SQL left in the crate.

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
pub mod exchange_rates;
pub mod forecast;
pub mod imports;
pub mod income;
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
pub mod tax_scales;
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

/// Connections the pool will open. SQLite serialises writers regardless, so this is sized
/// for concurrent *readers* plus the one writer.
const MAX_CONNECTIONS: u32 = 8;

/// How long a request may wait for a pooled connection before giving up.
///
/// Well under `sure_api::ApiConfig::request_timeout` (30s) on purpose, and the gap is the
/// whole point. `max_in_flight` (64) is deliberately larger than [`MAX_CONNECTIONS`], so a
/// burst *will* queue on acquire — and sqlx's own default acquire timeout is 30s, exactly
/// equal to the request deadline. Two deadlines expiring at the same instant is a race, and
/// whichever won decided what the client saw: `PoolTimedOut` (a scrubbed 500, which reads
/// as a bug and gives a client no reason to retry) or the deadline (a 408). Neither is the
/// 503 `overloaded` + `Retry-After` that `sure_api::limits` exists to emit. At 5s pool
/// exhaustion is instead a distinguishable, *fast* failure that always wins the race, and
/// [`AppError::is_overloaded`](sure_core::AppError::is_overloaded) turns it into that 503.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long SQLite itself waits for another writer to release the write lock before
/// returning `SQLITE_BUSY`. Its own internal retry, and the first of two: past it,
/// [`with_busy_retry`] retries the whole transaction.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Attempts (not retries) a busy write gets from [`with_busy_retry`].
const BUSY_RETRY_ATTEMPTS: u32 = 4;
/// First backoff after a `SQLITE_BUSY`, doubled each attempt: 25ms, 50ms, 100ms. With
/// [`BUSY_TIMEOUT`] already spent inside SQLite before the error is even raised, the point
/// of these is only to break the tie between two writers, not to wait out a long one.
const BUSY_RETRY_BASE_BACKOFF: Duration = Duration::from_millis(25);

/// Open (creating if necessary) a SQLite pool tuned for a low-concurrency,
/// single-family workload: WAL for concurrent reads, foreign keys enforced.
pub async fn connect(database_url: &str) -> anyhow::Result<Db> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
        // Log every executed statement at TRACE (sqlx defaults to INFO, which would
        // drown out the one-line-per-request summary); promote statements slower than a
        // second to WARN so genuinely slow queries surface without any RUST_LOG tuning.
        .log_statements(log::LevelFilter::Trace)
        .log_slow_statements(log::LevelFilter::Warn, Duration::from_secs(1));

    ensure_database_dir(database_url)?;

    let pool = SqlitePoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        // Not sqlx's default (30s), which is the request deadline to the second — see
        // POOL_ACQUIRE_TIMEOUT for why an equal deadline is a coin toss between a 500 and
        // a 408 when what the caller needs is a 503 it can retry.
        .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Run a database write, retrying it while SQLite reports that another writer holds the
/// lock.
///
/// SQLite serialises writers, so two concurrent imports (a scheduled sync and an upload,
/// say) *will* collide. [`BUSY_TIMEOUT`] is SQLite's own internal wait and used to be the
/// only retry anywhere in the process: past it the `SQLITE_BUSY` travelled all the way up
/// as `AppError::Database` and became a scrubbed 500 — an internal-error alert for the most
/// ordinary, transient condition the database has.
///
/// `op` must be a whole transaction, not a fragment of one: the retry replays it from the
/// start, which is only correct because a busy transaction committed nothing before it was
/// refused. Backoff is jittered so two writers that collided once do not line up on every
/// subsequent attempt. If the attempts run out the error is returned unchanged — and
/// [`AppError::is_overloaded`](sure_core::AppError::is_overloaded) renders it as a 503
/// `overloaded` with `Retry-After`, so the client retries rather than reading a defect.
pub async fn with_busy_retry<T, F, Fut>(what: &'static str, mut op: F) -> sure_core::AppResult<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = sure_core::AppResult<T>>,
{
    let mut backoff = BUSY_RETRY_BASE_BACKOFF;
    let mut attempt = 1u32;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= BUSY_RETRY_ATTEMPTS || !err.is_overloaded() {
                    return Err(err);
                }
                let wait = jittered(backoff);
                tracing::warn!(
                    operation = what,
                    attempt,
                    backoff_ms = wait.as_millis(),
                    error = %err,
                    "database busy; retrying the transaction"
                );
                tokio::time::sleep(wait).await;
                backoff *= 2;
                attempt += 1;
            }
        }
    }
}

/// Spread a backoff over `[d, 1.5d)`.
///
/// Deliberately not `rand`: this needs no statistical quality, only that two processes that
/// collided do not wake together, and the DAL should not grow an RNG dependency to say so.
/// The clock's sub-millisecond digits differ between two callers that are, by definition,
/// not synchronised.
fn jittered(base: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as u64);
    base + Duration::from_nanos((base.as_nanos() as u64 / 2).saturating_mul(nanos % 1000) / 1000)
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
///
/// Each migration that actually runs is logged by version and name, because until it was, a
/// schema change was completely silent: the only way to tell whether a start had applied one
/// was to read `_sqlx_migrations` afterwards. That matters most in development, where the
/// server restarts on every edit and a migration is applied within seconds of the file
/// appearing — long before its author has finished settling its contents. Editing it after
/// that point makes the *next* start fail with sqlx's checksum error, and the log is what
/// connects that failure to the start that caused it.
pub async fn migrate(pool: &Db) -> anyhow::Result<()> {
    log_pending(pool).await;
    MIGRATOR.run(pool).await?;
    // Seeding lives here rather than in an INSERT inside the migration so that
    // `sure_core::tax`'s constants stay the only place those figures are written down — a second
    // copy in SQL would be free to drift from the first, silently, forever. It fills an empty table
    // and never overwrites, so an edited rate survives every future startup.
    tax_scales::seed(pool).await?;
    Ok(())
}

/// Log which migrations this start is about to apply, before applying them.
///
/// Read *before* the run rather than diffed after it, so a migration that fails halfway is
/// still named in the log — an error that says only "migration 30 was previously applied but
/// has been modified" is far easier to place when the line above it says what 30 is.
///
/// Deliberately infallible: this is reporting, and a database that cannot answer the question
/// is about to fail the migration run itself, with a real error. Swallowing the problem here
/// means the caller sees that error rather than one about a logging query. The first-ever
/// start is the ordinary case for the query failing — `_sqlx_migrations` does not exist until
/// `run` creates it — which is why a missing table is treated as "nothing applied yet" rather
/// than as a fault.
async fn log_pending(pool: &Db) {
    let applied: std::collections::HashSet<i64> =
        match sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
            .fetch_all(pool)
            .await
        {
            Ok(versions) => versions.into_iter().collect(),
            // No table yet: a fresh database, so every migration is pending.
            Err(_) => std::collections::HashSet::new(),
        };

    let pending: Vec<_> = MIGRATOR
        .iter()
        .filter(|m| !applied.contains(&m.version))
        .collect();

    if pending.is_empty() {
        tracing::debug!(applied = applied.len(), "database schema is up to date");
        return;
    }
    tracing::info!(
        pending = pending.len(),
        applied = applied.len(),
        "applying database migrations"
    );
    for m in pending {
        tracing::info!(version = m.version, name = %m.description, "applying migration");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// [`log_pending`] swallows its query error on purpose, and the cost of that is a silent
    /// failure mode: if the row sqlx keeps its bookkeeping in ever stops answering to
    /// `SELECT version FROM _sqlx_migrations`, the `Err` arm reads it as "nothing applied yet"
    /// and every start would announce all 32 migrations as pending while applying none.
    /// Nobody would notice from the log, which is the one thing the log exists to prevent.
    ///
    /// So assert the round trip on a database that has just been fully migrated: what the
    /// migrator knows about and what the table records must be the same set, leaving nothing
    /// pending.
    #[tokio::test]
    async fn a_migrated_database_reports_nothing_pending() {
        let pool = connect("sqlite::memory:").await.unwrap();
        MIGRATOR.run(&pool).await.unwrap();

        let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
            .fetch_all(&pool)
            .await
            .expect("the version column log_pending reads must exist and decode as i64");

        let known: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert!(
            !known.is_empty(),
            "the migrator must know of some migrations"
        );
        assert_eq!(
            applied.len(),
            known.len(),
            "every migration the migrator knows about must be recorded as applied"
        );
        let applied: std::collections::HashSet<i64> = applied.into_iter().collect();
        let pending: Vec<i64> = known
            .iter()
            .copied()
            .filter(|v| !applied.contains(v))
            .collect();
        assert!(
            pending.is_empty(),
            "a fully migrated database must report nothing pending, got {pending:?}"
        );
    }

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

    /// W-18: the pool's acquire deadline must be *distinguishably* shorter than the request
    /// deadline. sqlx's default is 30s, which is `ApiConfig::request_timeout` to the second:
    /// two deadlines expiring together made the client's answer a coin toss between a
    /// scrubbed 500 and a 408, when the useful answer is a 503 it can retry.
    #[tokio::test]
    async fn the_pool_gives_up_on_acquire_well_before_the_request_deadline() {
        // The API's request deadline. Named here as a literal because the DAL sits below
        // `sure-api` and cannot read its config; `sure_api::config` is the source of truth.
        const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

        let pool = connect("sqlite::memory:").await.unwrap();
        assert_eq!(pool.options().get_acquire_timeout(), POOL_ACQUIRE_TIMEOUT);
        assert!(
            POOL_ACQUIRE_TIMEOUT * 4 <= API_REQUEST_TIMEOUT,
            "pool acquire ({POOL_ACQUIRE_TIMEOUT:?}) must lose the race to the request \
             deadline ({API_REQUEST_TIMEOUT:?}) by a wide margin, not by a hair"
        );
        pool.close().await;
    }

    /// The other half of W-18: once the pool does give up, the error must be recognisable as
    /// load. `AppError::is_overloaded` is what turns it into a 503 `overloaded` +
    /// `Retry-After` instead of the 500 it used to be.
    #[tokio::test]
    async fn pool_exhaustion_is_an_overload_not_an_internal_error() {
        // A hand-built pool, not `connect`, so the wait is 50ms instead of 5s.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_millis(50))
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let held = pool.acquire().await.unwrap();

        let err: sure_core::AppError = pool
            .acquire()
            .await
            .expect_err("the only connection is checked out")
            .into();
        assert!(err.is_overloaded(), "{err}");
        assert_eq!(err.code(), sure_core::error::OVERLOADED_CODE);

        drop(held);
        pool.close().await;
    }

    /// A throwaway file-backed database, deleted on drop.
    ///
    /// A *file*, because `SQLITE_BUSY` needs two connections contending for one database's
    /// write lock and `sqlite::memory:` gives every connection a private database of its own.
    /// In the OS temp directory, never `DATABASE_URL`: `data/sure.db` is real financial
    /// history, not a fixture.
    struct TempDb(PathBuf);

    impl TempDb {
        fn new(name: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let dir = std::env::temp_dir().join(format!("sure-dal-{name}-{unique}"));
            std::fs::create_dir_all(&dir).expect("temp dir should be creatable");
            Self(dir)
        }

        fn url(&self) -> String {
            format!("sqlite:{}/probe.db", self.0.display())
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            // Best effort: a leftover temp directory is not worth failing a test over.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A pool that reports `SQLITE_BUSY` immediately instead of waiting out
    /// [`BUSY_TIMEOUT`] — the same error the real pool raises, just without the five-second
    /// pause that would make this test unbearable.
    async fn impatient_pool(url: &str) -> Db {
        let options = SqliteConnectOptions::from_str(url)
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::ZERO);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap()
    }

    /// Deliberately not `sqlx::query!`: `busy_probe` is created by each test below at
    /// runtime, so it is not in `migrations/` and the compile-time checker has no schema to
    /// verify it against. The lock contention these tests are about needs a table nothing
    /// else writes to, which is worth more here than checking a two-column insert. Every
    /// query against the real schema uses the macros (see this module's docs).
    async fn insert_probe(pool: &Db) -> sure_core::AppResult<()> {
        sqlx::query("INSERT INTO busy_probe (id) VALUES (NULL)")
            .execute(pool)
            .await?;
        Ok(())
    }

    /// W-30: a write refused because another writer held the lock is transient by
    /// definition. Before this it was nobody's job to retry — `busy_timeout` was the only
    /// retry in the system, and past it the error became a 500 for a condition that clears
    /// on its own.
    #[tokio::test]
    async fn a_busy_write_is_retried_and_then_succeeds() {
        let temp = TempDb::new("busy-retry");
        let writer = impatient_pool(&temp.url()).await;
        sqlx::query("CREATE TABLE busy_probe (id INTEGER PRIMARY KEY)")
            .execute(&writer)
            .await
            .unwrap();
        let contender = impatient_pool(&temp.url()).await;

        // Hold the write lock.
        let mut blocking = writer.begin().await.unwrap();
        sqlx::query("INSERT INTO busy_probe (id) VALUES (NULL)")
            .execute(&mut *blocking)
            .await
            .unwrap();

        // Release it once the retry has actually been refused once, so the test proves a
        // retry happened rather than racing a timer.
        let attempts = Arc::new(AtomicUsize::new(0));
        let watched = Arc::clone(&attempts);
        let releaser = tokio::spawn(async move {
            while watched.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            blocking.commit().await.unwrap();
        });

        let counter = &attempts;
        let pool = &contender;
        with_busy_retry("test", || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            insert_probe(pool).await
        })
        .await
        .expect("the write should land once the other writer commits");

        releaser.await.unwrap();
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "the first attempt must have been refused, or this proves nothing"
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM busy_probe")
            .fetch_one(&contender)
            .await
            .unwrap();
        assert_eq!(
            rows, 2,
            "both the blocking write and the retried one landed"
        );
    }

    /// When the lock is never released the attempts run out — and the error that comes back
    /// still classifies as overload, so the API answers 503 `overloaded` (retryable by the
    /// client) rather than 500 (a bug to investigate).
    #[tokio::test]
    async fn a_write_that_stays_busy_gives_up_as_an_overload() {
        let temp = TempDb::new("busy-exhausted");
        let writer = impatient_pool(&temp.url()).await;
        sqlx::query("CREATE TABLE busy_probe (id INTEGER PRIMARY KEY)")
            .execute(&writer)
            .await
            .unwrap();
        let contender = impatient_pool(&temp.url()).await;

        let mut blocking = writer.begin().await.unwrap();
        sqlx::query("INSERT INTO busy_probe (id) VALUES (NULL)")
            .execute(&mut *blocking)
            .await
            .unwrap();

        let attempts = AtomicUsize::new(0);
        let counter = &attempts;
        let pool = &contender;
        let err = with_busy_retry("test", || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            insert_probe(pool).await
        })
        .await
        .expect_err("the lock is never released");

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            BUSY_RETRY_ATTEMPTS as usize
        );
        assert!(err.is_overloaded(), "{err}");
        assert_eq!(err.code(), sure_core::error::OVERLOADED_CODE);
        blocking.rollback().await.unwrap();
    }

    /// The retry must be narrow: anything that is not "another writer has it" is returned on
    /// the first attempt, because replaying a real failure only wastes the caller's deadline.
    #[tokio::test]
    async fn a_failure_that_is_not_busy_is_not_retried() {
        let attempts = AtomicUsize::new(0);
        let counter = &attempts;
        let err = with_busy_retry("test", || async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(sure_core::AppError::validation("nope"))
        })
        .await
        .expect_err("the closure always fails");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn jitter_stays_inside_its_window() {
        for base in [
            BUSY_RETRY_BASE_BACKOFF,
            BUSY_RETRY_BASE_BACKOFF * 4,
            Duration::ZERO,
        ] {
            let wait = jittered(base);
            assert!(wait >= base, "{wait:?} < {base:?}");
            assert!(
                wait <= base + base / 2,
                "{wait:?} is more than 1.5x {base:?}"
            );
        }
    }
}
