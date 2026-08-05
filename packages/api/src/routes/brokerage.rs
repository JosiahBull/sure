//! Brokerage account endpoints: the computed value snapshot, the raw holdings/dividends
//! ledgers, manual lot entry, a bulk zip import of a Sharesies export, and manual
//! revalue/backfill triggers. The heavy lifting (parsing, persistence, pricing) lives in
//! `sure_providers::sharesies` and `sure_app::brokerage`'s `BrokerageService` (behind the
//! `BrokerageRepo`/`AccountRepo` ports); these handlers are thin glue.

use std::future::Future;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use sure_appbase::Shutdown;
use sure_core::AccountKind;
use tokio::task::JoinHandle;
use utoipa::IntoParams;

use crate::config::Limits;
use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{
    BrokerageActivity30d, BrokerageImportResult, BrokerageSnapshot, Dividend, DividendDetail,
    DividendWithholding, HoldingLot, LotKind, Position, SaveHoldingLot, WalletBalance,
};

// OTEL span names for this module's handlers.
const BROKERAGE_SNAPSHOT: &str = "brokerage.snapshot";
const BROKERAGE_LIST_HOLDINGS: &str = "brokerage.list_holdings";
const BROKERAGE_CREATE_HOLDING: &str = "brokerage.create_holding";
const BROKERAGE_DELETE_HOLDING: &str = "brokerage.delete_holding";
const BROKERAGE_LIST_DIVIDENDS: &str = "brokerage.list_dividends";
const BROKERAGE_IMPORT: &str = "brokerage.import";
const BROKERAGE_REVALUE: &str = "brokerage.revalue";
const BROKERAGE_BACKFILL: &str = "brokerage.backfill";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct AsOfQuery {
    /// ISO-8601 date (`YYYY-MM-DD`); defaults to today. An unparseable value also falls
    /// back to today, matching the equity/stock-price endpoints.
    pub as_of: Option<String>,
}

fn parse_as_of(q: &AsOfQuery) -> NaiveDate {
    q.as_of
        .as_deref()
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Utc::now().date_naive())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn ensure_brokerage(st: &AppState, id: i64) -> AppResult<()> {
    let account = st.accounts.get(id).await?;
    if account.kind != AccountKind::Brokerage {
        return Err(AppError::validation("account is not a brokerage account"));
    }
    Ok(())
}

/// The account's computed value snapshot (positions priced + wallet cash) as of a date.
#[utoipa::path(get, path = "/api/accounts/{id}/brokerage", tag = "brokerage",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = BrokerageSnapshot), (status = 404, body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_SNAPSHOT,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn snapshot(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<BrokerageSnapshot>> {
    ensure_brokerage(&st, id).await?;
    Ok(Json(
        st.brokerage
            .snapshot(Some(st.stock_price_provider.as_ref()), id, parse_as_of(&q))
            .await?,
    ))
}

/// The raw holdings ledger (every buy/sell/corporate lot) for an audit view.
#[utoipa::path(get, path = "/api/accounts/{id}/brokerage/holdings", tag = "brokerage",
    params(("id" = i64, Path,)), responses((status = 200, body = [HoldingLot])))]
#[tracing::instrument(
    name = BROKERAGE_LIST_HOLDINGS,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_holdings(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<HoldingLot>>> {
    Ok(Json(st.brokerage.list_holdings(id).await?))
}

/// Manually record a lot (most arrive via import; this is parity with equity's manual grant).
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/holdings", tag = "brokerage",
    params(("id" = i64, Path,)), request_body = SaveHoldingLot,
    responses((status = 201, body = HoldingLot), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_CREATE_HOLDING,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create_holding(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveHoldingLot>,
) -> AppResult<(StatusCode, Json<HoldingLot>)> {
    ensure_brokerage(&st, id).await?;
    Ok((
        StatusCode::CREATED,
        Json(st.brokerage.create_holding(id, input).await?),
    ))
}

#[utoipa::path(delete, path = "/api/brokerage/holdings/{id}", tag = "brokerage",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_DELETE_HOLDING,
    level = "debug",
    skip_all,
    fields(holding_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete_holding(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    st.brokerage.delete_holding(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Dividend/distribution history with per-jurisdiction withholding detail.
#[utoipa::path(get, path = "/api/accounts/{id}/brokerage/dividends", tag = "brokerage",
    params(("id" = i64, Path,)), responses((status = 200, body = [DividendDetail])))]
#[tracing::instrument(
    name = BROKERAGE_LIST_DIVIDENDS,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_dividends(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<DividendDetail>>> {
    Ok(Json(st.brokerage.list_dividends(id).await?))
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
/// wrapper) keeps the reported spawn site at the handler below instead of at this line.
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

/// Bulk-import a Sharesies export zip: wallet transactions, holding lots, and dividends.
/// Auto-links unambiguous wallet ↔ bank transfers synchronously, then kicks off a
/// background historical valuation backfill so net worth is accurate retroactively.
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/import", tag = "brokerage",
    params(("id" = i64, Path,)),
    request_body(content = Vec<u8>, description = "A Sharesies export .zip", content_type = "application/zip"),
    responses((status = 200, body = BrokerageImportResult), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_IMPORT,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn import(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    body: Bytes,
) -> AppResult<Json<BrokerageImportResult>> {
    let account = st.accounts.get(id).await?;
    if account.kind != AccountKind::Brokerage {
        return Err(AppError::validation("account is not a brokerage account"));
    }

    // Unzip + JSON-parse is CPU-bound: keep it off the async runtime's worker threads.
    let export =
        tokio::task::spawn_blocking(move || sure_providers::sharesies::parse_export(&body))
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .map_err(|e| AppError::validation(format!("could not read export: {e}")))?;

    let provider_tag = format!("sharesies#{id}");
    let wallet_rows: Vec<sure_app::ports::ImportRow> = export
        .wallet_transactions
        .into_iter()
        .map(|t| sure_app::ports::ImportRow {
            external_id: t.external_id,
            posted_at: t.posted_at,
            amount_minor: t.amount_minor,
            currency_code: t.currency_code,
            description: t.description,
            merchant: t.merchant,
            category_name: t.category.as_ref().map(|c| c.name.clone()),
            category_kind: t.category.as_ref().and_then(|c| c.kind),
            category_group: t.category.and_then(|c| c.group),
            is_one_off: false,
        })
        .collect();
    let holdings: Vec<sure_app::ports::HoldingImport> = export
        .holdings
        .into_iter()
        .map(|h| sure_app::ports::HoldingImport {
            ticker: h.ticker,
            exchange: h.exchange,
            name: h.name,
            currency_code: h.currency_code,
            trade_date: h.trade_date,
            quantity: h.quantity,
            unit_price: h.unit_price,
            fee_minor: h.fee_minor,
            kind: h.kind,
            external_id: h.external_id,
        })
        .collect();
    let dividends: Vec<sure_app::ports::DividendImport> = export
        .dividends
        .into_iter()
        .map(|d| sure_app::ports::DividendImport {
            ticker: d.ticker,
            exchange: d.exchange,
            record_date: d.record_date,
            paid_date: d.paid_date,
            shares_held: d.shares_held,
            gross_amount_minor: d.gross_amount_minor,
            net_amount_minor: d.net_amount_minor,
            currency_code: d.currency_code,
            external_id: d.external_id,
            withholdings: d
                .withholdings
                .into_iter()
                .map(|w| sure_app::ports::WithholdingImport {
                    owed_to: w.owed_to,
                    tax_amount_minor: w.tax_amount_minor,
                    tax_credit_minor: w.tax_credit_minor,
                    currency_code: w.currency_code,
                })
                .collect(),
        })
        .collect();

    let counts = st
        .brokerage
        .import_export(
            id,
            &account.currency_code,
            &provider_tag,
            &wallet_rows,
            &holdings,
            &dividends,
        )
        .await?;

    // Transfer auto-linking (wallet deposit/withdrawal ↔ the matching bank transaction) is
    // not done here: the bank side is often synced *after* this import, so it wouldn't
    // exist to match yet. The scheduled `TransferLinkTask` reconciles both sides regardless
    // of import order — see `sure_app::tasks::transfer_link`.

    // Backfill the daily valuation history in the background: it makes one upstream price
    // call per ticker then loops every day since inception, which is too slow to block the
    // upload response on. Idempotent, so the panel's "Backfill" button is the retry path.
    let brokerage = st.brokerage.clone();
    let provider = st.stock_price_provider.clone();
    spawn_backfill(&st.shutdown, id, async move {
        brokerage.backfill_history(provider.as_ref(), id).await
    });

    Ok(Json(BrokerageImportResult {
        transactions_imported: counts.transactions_imported,
        transactions_skipped: counts.transactions_skipped,
        holdings_imported: counts.holdings_imported,
        holdings_skipped: counts.holdings_skipped,
        dividends_imported: counts.dividends_imported,
        dividends_skipped: counts.dividends_skipped,
        warnings: export.warnings,
    }))
}

/// Snapshot the account's current value into a `source='brokerage'` valuation (mirrors
/// equity's "Revalue").
///
/// 422 when a holding's currency has no exchange rate to the account's: the snapshot would
/// understate the account, and a stored valuation carries no hint that it did.
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/revalue", tag = "brokerage",
    params(("id" = i64, Path,), AsOfQuery),
    responses((status = 200, body = BrokerageSnapshot), (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_REVALUE,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn revalue(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<AsOfQuery>,
) -> AppResult<Json<BrokerageSnapshot>> {
    ensure_brokerage(&st, id).await?;
    Ok(Json(
        st.brokerage
            .revalue(Some(st.stock_price_provider.as_ref()), id, parse_as_of(&q))
            .await?,
    ))
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct BackfillResult {
    /// Number of days for which a valuation was (re)computed.
    pub days: i64,
}

/// Rebuild the whole daily valuation history from the price cache/provider. Runs
/// synchronously (unlike the post-import background backfill) so the response reports how
/// many days were valued — the manual retry escape hatch.
///
/// Note this route's response set differs from `snapshot`'s and `revalue`'s, and not by
/// oversight: **it cannot answer 502.** `BrokerageService::backfill_history` catches each
/// ticker's `fetch_daily_prices` failure and only warns — one delisted ticker must not sink a
/// whole history — then walks the days with `provider=None`, which opens no socket. So a total
/// price-feed outage answers 200 here, with a history of cash-only valuations. Declaring a 502 a
/// client can never receive is worse than declaring nothing: it invites a caller to wait for a
/// signal that does not come. The 422 below is the one it really can produce, propagated out of
/// `revalue`'s refusal to persist a total it could not convert.
#[utoipa::path(post, path = "/api/accounts/{id}/brokerage/backfill", tag = "brokerage",
    params(("id" = i64, Path,)),
    responses((status = 200, body = BackfillResult), (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = BROKERAGE_BACKFILL,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn backfill(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<BackfillResult>> {
    ensure_brokerage(&st, id).await?;
    let days = st
        .brokerage
        .backfill_history(st.stock_price_provider.as_ref(), id)
        .await? as i64;
    Ok(Json(BackfillResult { days }))
}

pub fn router(limits: &Limits) -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/brokerage", get(snapshot))
        .route(
            "/accounts/{id}/brokerage/holdings",
            get(list_holdings).post(create_holding),
        )
        .route(
            "/brokerage/holdings/{id}",
            axum::routing::delete(delete_holding),
        )
        .route("/accounts/{id}/brokerage/dividends", get(list_dividends))
        .route(
            "/accounts/{id}/brokerage/import",
            post(import).layer(DefaultBodyLimit::max(limits.max_import_body_bytes)),
        )
        .route("/accounts/{id}/brokerage/revalue", post(revalue))
        .route("/accounts/{id}/brokerage/backfill", post(backfill))
}

/// The post-import backfill's *lifecycle*, not its arithmetic (that is
/// `sure_app::brokerage`'s): it must be visible to the shutdown drain, because it is the one
/// piece of work in `sure-api` that outlives the response that started it.
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use sure_appbase::{DrainOutcome, Shutdown};

    use super::spawn_backfill;
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
    /// helper instead of the handler that started the work.
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
        let source = include_str!("brokerage.rs");
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
}
