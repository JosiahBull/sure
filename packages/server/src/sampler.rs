//! The periodic sampler: the one task that turns the state of the ledger into gauges.
//!
//! # Why a task rather than observable gauges
//!
//! OpenTelemetry's natural fit for a gauge is an *observable* one — a callback the SDK invokes
//! when it collects. That callback runs on the metric reader's own OS thread, synchronously,
//! with no tokio runtime in reach: it cannot `await` a sqlx query, and blocking it stalls
//! export for every other metric in the process. Every gauge here is the answer to a database
//! question, so a callback is the wrong shape for all of them.
//!
//! Instead this runs as an ordinary tracked task and *writes* synchronous gauges. The periodic
//! reader then exports the last value it was given, which is exactly the semantics of a gauge.
//!
//! # Why it is slower than the export interval
//!
//! `SURE_OTEL_SAMPLE_INTERVAL_SECS` defaults to 300s against a 60s export interval, so the same
//! value is exported about five times before it is recomputed. That is deliberate. Net worth
//! reads every account's valuations and transaction sums; "how many transactions have no
//! category" is a `COUNT(*)` over the whole table. None of these change meaningfully inside a
//! minute, and a gauge that costs a ledger scan every 60s forever is a self-inflicted load
//! problem — the app would be spending more time measuring itself than serving the household.
//!
//! # Failure is not fatal
//!
//! Every query is best-effort: a gauge that could not be read is left at its previous value and
//! logged at DEBUG. This task must never be the reason a shutdown is dirty or a request is slow,
//! and a missing data point on a dashboard is a smaller problem than either.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sure_app::reports::{ReportQuery, ReportService};
use sure_appbase::Shutdown;
use sure_core::AccountClass;
use sure_dal::Db;
use sure_scheduler::TaskStateStore;
use sure_telemetry::{KeyValue, instruments};

/// Every account class, so an emptied one reports zero rather than going stale.
///
/// Listed rather than derived because `AccountClass` has no `ALL`; the compiler will not catch a
/// fifth variant being added here, so the exhaustive `match` in `AccountClass::as_str` is what a
/// reviewer should check this against.
const ACCOUNT_CLASSES: [AccountClass; 4] = [
    AccountClass::Cash,
    AccountClass::Asset,
    AccountClass::Investment,
    AccountClass::Liability,
];

/// Everything the sampler reads, so `serve` hands it one value rather than five.
///
/// The services are built by `serve` rather than here: the composition root is the only place
/// that wires a `ReportService`, and a sampler that assembled its own would be a second opinion
/// about how a report is put together.
pub struct Sampler {
    pub pool: Db,
    pub reports: Arc<ReportService>,
    pub task_state: Arc<dyn TaskStateStore>,
    pub interval: Duration,
    /// Names of the registered scheduled tasks, for the last-run-age gauge. Empty when
    /// `BACKGROUND_TASKS=off`, in which case that gauge is legitimately absent rather than zero.
    pub task_names: Vec<&'static str>,
}

/// Sample once every `interval` until shutdown.
///
/// The first sample is taken immediately, so a freshly started process has gauges before the
/// first interval elapses rather than a five-minute hole at the start of every restart.
pub async fn run(sampler: Sampler, shutdown: Shutdown) {
    let mut ticker = tokio::time::interval(sampler.interval);
    loop {
        tokio::select! {
            // Biased, matching `sure_scheduler::Scheduler::run`: a cancellation arriving in the
            // same moment as a tick should win, because there is no point sampling on the way
            // out — the reader will not get another chance to export it.
            biased;
            () = shutdown.cancelled() => break,
            _ = ticker.tick() => {}
        }
        sample(&sampler, &shutdown).await;
    }
    tracing::debug!("telemetry sampler stopped");
}

/// One pass. Each gauge is independent: one failing query does not stop the others.
async fn sample(sampler: &Sampler, shutdown: &Shutdown) {
    let instruments = instruments();

    // Process-level, and free — no query behind either.
    sure_telemetry::instruments::record_pool(
        sampler.pool.size(),
        sampler.pool.num_idle(),
        sure_dal::max_connections(),
    );
    instruments
        .tracked_tasks
        .record(u64::try_from(shutdown.tracked()).unwrap_or(0), &[]);

    // Accounts by class. `AccountKind::class` is the single mapping (CLAUDE.md rule 1), so this
    // cannot disagree with what the reports show.
    match sure_dal::reports::active_accounts(&sampler.pool).await {
        Ok(accounts) => {
            // Every class is reported, including the ones with nothing in them. A gauge is only
            // ever the last value written, so *omitting* an empty class would freeze it at
            // whatever it last was — archive the only liability and the dashboard goes on
            // claiming there is one, indefinitely. That is the kind of wrongness that surfaces
            // months later with no way to tell when it started.
            for class in ACCOUNT_CLASSES {
                let count = accounts
                    .iter()
                    .filter(|account| account.kind.class() == class)
                    .count();
                instruments.accounts_count.record(
                    u64::try_from(count).unwrap_or(0),
                    &[KeyValue::new("class", class.as_str())],
                );
            }
        }
        Err(err) => tracing::debug!(error = %err, "could not sample the account count"),
    }

    match sure_dal::rules::count_uncategorized(&sampler.pool).await {
        Ok(count) => instruments
            .transactions_uncategorized
            .record(u64::try_from(count).unwrap_or(0), &[]),
        Err(err) => tracing::debug!(error = %err, "could not sample the uncategorised count"),
    }

    // Provider freshness. `last_synced_at` is written only on a successful sync, so the age is
    // the real question: a feed that has been erroring for a week has a growing number here
    // while its error counter may have long since stopped being looked at.
    match sure_dal::providers::list(&sampler.pool).await {
        Ok(providers) => {
            let now = Utc::now();
            for provider in providers {
                if !provider.enabled {
                    continue;
                }
                let age = provider
                    .last_synced_at
                    .as_deref()
                    .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                    .map(|at| {
                        now.signed_duration_since(at.with_timezone(&Utc))
                            .num_seconds()
                    });
                // A provider that has *never* synced is left unreported rather than recorded as
                // age 0, which would read as "just synced" — the opposite of the truth.
                if let Some(age) = age {
                    instruments.provider_last_sync_age.record(
                        age.max(0),
                        &[
                            KeyValue::new("provider_kind", provider.kind.clone()),
                            KeyValue::new("provider_name", provider.name.clone()),
                        ],
                    );
                }
            }
        }
        Err(err) => tracing::debug!(error = %err, "could not sample provider sync ages"),
    }

    // Scheduled-task freshness, the same question for work with no provider behind it.
    for name in &sampler.task_names {
        match sampler.task_state.last_run_at(name).await {
            Ok(Some(at)) => instruments.scheduled_task_last_run_age.record(
                Utc::now().signed_duration_since(at).num_seconds().max(0),
                &[KeyValue::new("job", *name)],
            ),
            // Never run yet — same reasoning as a provider that has never synced.
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(task = name, error = %err, "could not sample a task's last run")
            }
        }
    }

    // Net worth, and the currencies that could not be converted into it. Both come from one
    // `balances` call, which is also the heaviest thing in this function — the reason the
    // sample interval is minutes rather than seconds.
    match sampler.reports.balances(&ReportQuery::default()).await {
        Ok(report) => {
            instruments.net_worth_minor.record(
                report.total_minor,
                &[KeyValue::new("currency", report.currency)],
            );
            instruments
                .fx_unconverted_currencies
                .record(u64::try_from(report.unconverted.len()).unwrap_or(0), &[]);
        }
        Err(err) => tracing::debug!(error = %err, "could not sample net worth"),
    }
}
