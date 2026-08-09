//! The import endpoint: one upload in, one [`ImportResult`] out, whatever the file was.
//!
//! This module is glue, and deliberately thin. Recognising the file, routing each thing in it
//! to an account, deriving that account's cutover, reconciling and writing all live in
//! [`sure_app::import`] — which is where they can be unit-tested without a socket, and where a
//! fifth importer joins without touching anything here. What is genuinely transport lives here:
//! reading the body, keeping the parse off the async runtime's worker threads, spawning the one
//! piece of follow-up work through the shutdown handle, and turning a refusal into a 422.
//!
//! It replaces four routes that each did all of that themselves —
//! `/accounts/{id}/asb/import`, `/asb/import`, `/accounts/{id}/student-loan/import` and
//! `/accounts/{id}/brokerage/import` — and disagreed about most of it: only ASB could preview,
//! only ASB could be undone, and the myIR route had never been given the long deadline the
//! other three had.
//!
//! `?dry_run=true` runs everything except the write and reports what a commit would do. It's
//! the same code path up to one branch, so a preview cannot describe an import that wouldn't
//! happen — and `?assign=` sends the choices back on the commit, so what was on screen is what
//! runs.

use std::future::Future;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::post;
use axum::Router;
use serde::Deserialize;
use sure_app::import::{FollowUp, ImportOptions};
use sure_appbase::Shutdown;
use tokio::task::JoinHandle;
use utoipa::IntoParams;

use crate::config::Limits;
use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{ImportRecord, ImportResult, ImportSource, ImportUndoResult};

const IMPORT: &str = "import.run";
const IMPORT_UNDO: &str = "import.undo";
const IMPORT_HISTORY: &str = "import.history";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct ImportQuery {
    /// Parse and report, but write nothing. Defaults to committing.
    pub dry_run: Option<bool>,
    /// Whether to also record the opening balance an export implies — the account's value
    /// immediately before its first row — as a one-off transaction. On by default: without it
    /// the reconstructed history starts from nothing rather than from what the account held.
    /// Ignored by sources whose exports state no balance to work back from, and skipped anyway
    /// when the account already has a row from before that date.
    pub opening_balance: Option<bool>,
    /// Which Sure account each thing in the upload belongs to, as
    /// `12-3456-0000123-50:8,12-3456-0000123-51:12`. Overrides whatever the routing tiers would
    /// have worked out, and is how the UI commits exactly what its preview showed.
    ///
    /// `12-3456-0000123-51:skip` imports nothing of that one. That is a statement, not the
    /// absence of one: leaving an item out of this parameter only means the assignment tier has
    /// nothing to say about it, and the evidence tiers below go on to place it anyway.
    pub assign: Option<String>,
    /// The date a feed that has **never posted** owns from, per thing in the upload, as
    /// `12-3456-0000123-50:2026-08-01`. The third way out of an `unsynced_feed` block, for when
    /// syncing the feed and disabling it are both wrong — only the person importing can know when
    /// a silent feed will start posting from.
    ///
    /// Ignored wherever the cutover can be derived, which is what stops it widening an import:
    /// an account whose feeds have posted, or whose balance-derived connection states its own
    /// start date, keeps the window they establish and the item says so in its warnings.
    pub cutover: Option<String>,
    /// Read the upload as this source instead of sniffing it. The escape hatch for a file the
    /// sniff gets wrong — and what the UI offers after a failed detect.
    pub source: Option<String>,
}

impl ImportQuery {
    fn options(&self) -> AppResult<ImportOptions> {
        Ok(ImportOptions {
            dry_run: self.dry_run.unwrap_or(false),
            // On unless asked otherwise: an imported history that starts from nothing is wrong,
            // and the preview shows the figure before anything is written.
            opening_balance: self.opening_balance.unwrap_or(true),
            assign: self.assign.clone(),
            cutover: self.cutover.clone(),
            source: self.source.as_deref().map(parse_source).transpose()?,
        })
    }
}

/// The wire name of a source into the enum, at the one edge it arrives as text (CLAUDE.md
/// rule 1). An unrecognised value is a 400-shaped refusal that lists what there is, rather
/// than a silent fall back to sniffing — a caller naming a source is overriding the sniff on
/// purpose, and quietly ignoring them would import the file the very way they said not to.
fn parse_source(raw: &str) -> AppResult<ImportSource> {
    raw.parse().map_err(|_| {
        AppError::validation(format!(
            "'{raw}' is not an import source — expected one of asb_csv, myir_sls, \
             sharesies_zip, csv_upload"
        ))
    })
}

/// Import an upload: a bank transaction `.csv`, a myIR `.xlsx`, a Sharesies export `.zip`, or a
/// `.zip` of any of those. The source is recognised from the bytes; `?source=` overrides it.
///
/// Idempotent — re-uploading the same file imports nothing new, so overlapping download windows
/// are free. A zip spanning several accounts is reported account by account, and anything the
/// routing tiers can't place is described rather than guessed at.
///
/// **One thing in an upload cannot fail the upload.** An item nothing could place, and an item
/// whose target account has a conflict to resolve (`blocked`), are both reported in `items` with
/// nothing written for them, while every other item imports. A 422 is reserved for what makes the
/// whole request unanswerable: an unreadable blob, an upload over the size cap, or an `?assign=`
/// naming an account that doesn't exist or can't take this file.
#[utoipa::path(post, path = "/api/import", tag = "transactions",
    params(ImportQuery),
    request_body(content = Vec<u8>, description = "An export file, or a .zip of them", content_type = "application/octet-stream"),
    responses((status = 200, body = ImportResult), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = IMPORT,
    level = "debug",
    skip_all,
    fields(dry_run = %q.dry_run.unwrap_or(false), source = tracing::field::Empty),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn run(
    State(st): State<AppState>,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> AppResult<Json<ImportResult>> {
    let opts = q.options()?;
    // Unzipping and parsing a few thousand rows is CPU-bound: keep it off the async runtime's
    // worker threads. Only the parse — the write half awaits SQLite and belongs on the runtime,
    // which is why the service splits into these two calls rather than one.
    let service = st.import.clone();
    let parse_opts = opts.clone();
    let upload = tokio::task::spawn_blocking(move || service.parse(&body, &parse_opts))
        .await
        .map_err(|e| AppError::Internal(e.into()))??;

    tracing::Span::current().record("source", upload.source.as_str());
    let imported = st.import.commit(upload, &opts).await?;
    if let Some(follow_up) = imported.follow_up {
        spawn_follow_up(&st, follow_up);
    }
    Ok(Json(imported.result))
}

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct HistoryQuery {
    /// Narrow the log to one account. Omitted, it is every import.
    pub account_id: Option<i64>,
}

/// What file imports have done, newest first.
///
/// The alternative, before this existed, was reading it back out of the transactions: two panels
/// fetched up to ten thousand rows and filtered client-side on the provider tag to work out how
/// much of an account came from an export and how far back it reached. The import already knew.
#[utoipa::path(get, path = "/api/imports", tag = "transactions",
    params(HistoryQuery),
    responses((status = 200, body = Vec<ImportRecord>)))]
#[tracing::instrument(
    name = IMPORT_HISTORY,
    level = "debug",
    skip_all,
    fields(account_id = ?q.account_id),
    err(level = tracing::Level::WARN),
)]
pub async fn history(
    State(st): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> AppResult<Json<Vec<ImportRecord>>> {
    Ok(Json(st.import.history(q.account_id).await?))
}

/// Remove one source's import from one account, leaving every other source untouched.
///
/// Source-wide rather than per-upload, because per-upload has no honest answer: two overlapping
/// uploads of the same window share their content-derived ids, so the second one's rows were
/// skipped rather than written and there is nothing left of it to take back.
#[utoipa::path(delete, path = "/api/import/{account_id}/{source}", tag = "transactions",
    params(("account_id" = i64, Path,), ("source" = String, Path,)),
    responses((status = 200, body = ImportUndoResult), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = IMPORT_UNDO,
    level = "debug",
    skip_all,
    fields(account_id = %account_id, source = %source),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn undo(
    State(st): State<AppState>,
    Path((account_id, source)): Path<(i64, String)>,
) -> AppResult<Json<ImportUndoResult>> {
    let source = parse_source(&source)?;
    Ok(Json(st.import.undo(account_id, source).await?))
}

/// Start whatever an import left behind, on the process's own tracker.
///
/// Matched exhaustively, so a source that brings a new kind of follow-up has to come here and
/// decide how it gets spawned rather than inheriting a default.
#[track_caller]
fn spawn_follow_up(st: &AppState, follow_up: FollowUp) -> JoinHandle<()> {
    match follow_up {
        FollowUp::BackfillValuations { account_id } => {
            let brokerage = st.brokerage.clone();
            let provider = st.stock_price_provider.clone();
            spawn_backfill(&st.shutdown, account_id, async move {
                brokerage
                    .backfill_history(provider.as_ref(), account_id)
                    .await
            })
        }
    }
}

/// Start the post-import valuation backfill as a **tracked** background task, logging a
/// failure rather than propagating it (nobody is waiting for the result — the response for
/// the import that started it went out long before).
///
/// Tracked, i.e. [`Shutdown::spawn`] and never a bare `tokio::spawn`. The backfill makes one
/// upstream price call per ticker and then walks every calendar day since inception writing
/// a valuation, so it is routinely still running minutes after its response. Spawned bare it
/// is invisible to the drain: on `SIGTERM` the drain finds nothing, `pool.close()` runs
/// underneath the task, and the shutdown report prints `abandoned=0 clean=true` over a
/// valuation write that was cut in half — a report that is confidently wrong is worse than
/// no report. Tracked, the same task is either waited out or named as abandoned.
///
/// The future is a parameter rather than built here so the tracking can be tested without a
/// live `BrokerageService`; `#[track_caller]` (as [`Shutdown::spawn`]'s own docs require of a
/// wrapper) keeps the reported spawn site at [`spawn_follow_up`] instead of at this line.
///
/// [`Shutdown::spawn`]: sure_appbase::Shutdown::spawn
#[track_caller]
fn spawn_backfill(
    shutdown: &Shutdown,
    account_id: i64,
    backfill: impl Future<Output = AppResult<usize>> + Send + 'static,
) -> JoinHandle<()> {
    shutdown.spawn(async move {
        if let Err(e) = backfill.await {
            tracing::warn!(account_id, error = %e, "brokerage history backfill failed");
        }
    })
}

pub fn router(limits: &Limits) -> Router<AppState> {
    Router::new()
        .route(
            "/import",
            // The raised limit is layered onto the upload only.
            post(run).layer(DefaultBodyLimit::max(limits.max_import_body_bytes)),
        )
        // A separate template from the upload, so the undo keeps the global body limit and the
        // ordinary deadline: it takes no body and is one statement, and a 50 MiB allowance on a
        // route that ignores what arrives is an allowance nobody chose.
        .route("/import/{account_id}/{source}", axum::routing::delete(undo))
        .route("/imports", axum::routing::get(history))
}

/// The post-import backfill's *lifecycle*, not its arithmetic (that is
/// `sure_app::brokerage`'s): it must be visible to the shutdown drain, because it is the one
/// piece of work in `sure-api` that outlives the response that started it. These tests moved
/// here from `routes::brokerage` with the code they cover.
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use sure_appbase::{DrainOutcome, Shutdown};

    use super::{parse_source, spawn_backfill, ImportQuery, ImportSource};
    use crate::error::AppError;

    /// The fix for W-27. A tracked backfill is counted by the drain *and* waited for, so a
    /// `SIGTERM` mid-backfill cannot close the pool underneath a valuation write.
    #[tokio::test]
    async fn the_backfill_is_tracked_so_the_drain_waits_for_it() {
        let shutdown = Shutdown::new();
        let finished = Arc::new(AtomicBool::new(false));

        let flag = finished.clone();
        let handle = spawn_backfill(&shutdown, 7, async move {
            // Stands in for the day-by-day valuation walk: still running when the signal
            // lands, which is the whole situation under test.
            tokio::time::sleep(Duration::from_millis(50)).await;
            flag.store(true, Ordering::SeqCst);
            Ok(365)
        });
        assert_eq!(
            shutdown.tracked(),
            1,
            "the backfill must be in the tracker the moment it is spawned"
        );

        let outcome = shutdown.drain(Duration::from_secs(5)).await;
        assert_eq!(outcome, DrainOutcome::Drained { tasks: 1 });
        assert!(
            finished.load(Ordering::SeqCst),
            "the drain returned before the backfill had finished writing"
        );
        handle.await.expect("the backfill task must not panic");
    }

    /// What the bug looked like from the outside, and why no exit code or WAL check could
    /// catch it: the report is *clean* while the work is demonstrably unfinished.
    #[tokio::test]
    async fn a_bare_spawn_is_reported_as_nothing_running() {
        let shutdown = Shutdown::new();
        let finished = Arc::new(AtomicBool::new(false));

        // Deliberately bare, and only here: this is the pattern being ruled out, reproduced
        // so the assertion below is about observed behaviour rather than a claim.
        let flag = finished.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            flag.store(true, Ordering::SeqCst);
        });

        let outcome = shutdown.drain(Duration::from_millis(50)).await;
        assert_eq!(
            outcome,
            DrainOutcome::Drained { tasks: 0 },
            "an untracked task is invisible: the drain reports a clean, empty shutdown"
        );
        assert!(
            !finished.load(Ordering::SeqCst),
            "…while the task it could not see is still mid-flight"
        );
        handle.abort();
    }

    /// `#[track_caller]` on the wrapper, per `Shutdown::spawn`'s docs: without it every
    /// abandoned backfill would be reported at `spawn_backfill`'s own line, which names the
    /// helper instead of the code that started the work.
    #[tokio::test]
    async fn an_overrunning_backfill_is_named_at_its_call_site() {
        let shutdown = Shutdown::new();
        // Built first, so the `spawn_backfill` call is a single line and `line!()` below can
        // predict exactly what `Location::caller` will record.
        let never_finishes = async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(0)
        };
        let call_line = line!() + 1;
        let handle = spawn_backfill(&shutdown, 7, never_finishes);

        let outcome = shutdown.drain(Duration::from_millis(50)).await;
        assert_eq!(
            outcome.abandoned(),
            1,
            "a task left running must be counted"
        );
        // Call-site bookkeeping is debug-only by design; a release build reports the count
        // and an empty list.
        if cfg!(debug_assertions) {
            let expected = format!("{}:{call_line}:", file!());
            assert!(
                outcome.sites().iter().any(|s| s.starts_with(&expected)),
                "expected the abandoned site to be this test's call at {expected}, got {:?}",
                outcome.sites()
            );
        }
        handle.abort();
    }

    /// A backfill that fails (an unreachable price feed is the common case) is logged and
    /// ends, so it neither poisons the tracker nor delays the drain.
    #[tokio::test]
    async fn a_failing_backfill_still_drains_cleanly() {
        let shutdown = Shutdown::new();
        let handle = spawn_backfill(&shutdown, 7, async {
            Err(AppError::validation(
                "could not parse earliest activity date",
            ))
        });

        let outcome = shutdown.drain(Duration::from_secs(5)).await;
        assert_eq!(outcome, DrainOutcome::Drained { tasks: 1 });
        handle
            .await
            .expect("a failed backfill is logged, not propagated as a panic");
    }

    /// The regression guard. The fix is one call, and reaching for `tokio::spawn` here again
    /// would compile, pass every e2e test, and silently restore a shutdown report that lies.
    #[test]
    fn nothing_in_this_module_reaches_for_a_bare_tokio_spawn() {
        let source = include_str!("import.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields the part before the test module");

        assert!(
            !production.contains("tokio::spawn("),
            "background work in this module must go through Shutdown::spawn, not tokio::spawn"
        );
        assert!(
            production.contains("shutdown.spawn("),
            "the post-import backfill must still be spawned on the tracker"
        );
    }

    #[test]
    fn a_source_name_parses_and_an_unknown_one_lists_the_options() {
        assert_eq!(parse_source("asb_csv").unwrap(), ImportSource::AsbCsv);
        assert_eq!(parse_source("csv_upload").unwrap(), ImportSource::CsvUpload);
        let err = parse_source("asb").expect_err("refused").to_string();
        assert!(err.contains("asb_csv"), "{err}");
    }

    /// A caller who names a source is overriding the sniff on purpose. Falling back to sniffing
    /// on a typo would import the file the one way they said not to, and report success.
    #[test]
    fn a_bad_source_is_refused_rather_than_ignored() {
        let q = ImportQuery {
            source: Some("sharesies".to_string()),
            ..Default::default()
        };
        assert!(q.options().is_err());
    }

    #[test]
    fn the_opening_balance_defaults_to_on_and_the_dry_run_to_off() {
        let opts = ImportQuery::default().options().unwrap();
        assert!(opts.opening_balance);
        assert!(!opts.dry_run);
        assert!(opts.source.is_none());
    }
}
