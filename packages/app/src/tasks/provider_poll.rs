//! Generalized background poller: syncs every enabled provider that doesn't need a
//! human-supplied payload (e.g. CSV import) — anything credential/API-based, today just
//! `akahu`, gets this for free as new such kinds are added, with no per-kind wiring here.
//! Persistence/audit reuses the same [`SyncService::sync_provider`] path the manual sync
//! route (`sure-api`'s `routes::providers::sync`) drives; scheduling (surviving restarts
//! without early re-runs) is handled generically by `sure_scheduler`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_core::SyncOutcome;
use sure_scheduler::{ScheduledTask, TaskRun};
use tokio_util::sync::CancellationToken;

use crate::ports::{ProviderRegistry, ProviderRepo};
use crate::sync::SyncService;

/// Bank data itself only refreshes a few times a day upstream, so there's no value in
/// polling more often than this (mirrors the reasoning behind `exchange_rates::POLL_INTERVAL`).
const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub struct ProviderPollTask {
    providers: Arc<dyn ProviderRepo>,
    registry: Arc<dyn ProviderRegistry>,
    sync: Arc<SyncService>,
}

impl ProviderPollTask {
    pub fn new(
        providers: Arc<dyn ProviderRepo>,
        registry: Arc<dyn ProviderRegistry>,
        sync: Arc<SyncService>,
    ) -> Self {
        Self {
            providers,
            registry,
            sync,
        }
    }
}

#[async_trait]
impl ScheduledTask for ProviderPollTask {
    fn name(&self) -> &'static str {
        "provider_poll"
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
        let providers = self.providers.list().await?;
        let mut tally = Tally::default();

        for provider in providers {
            // Checked before each provider rather than inside one: a sync in flight is
            // committing rows, and the token cannot reach into it (`SyncContext` carries no
            // cancellation — one paginated fetch is bounded by its own wall-clock budget
            // instead, see `sure_providers::akahu`). Stopping between providers is what keeps
            // a household with several connections from spending the whole drain grace on the
            // ones it hadn't got to yet.
            if cancel.is_cancelled() {
                tally.report("provider poll stopped early for shutdown");
                return Ok(TaskRun::Interrupted);
            }
            if !provider.enabled {
                continue;
            }
            // Payload providers (e.g. CSV) need a human to supply the payload each time,
            // so they can never be run unattended; anything else is safe to auto-sync.
            let accepts_payload = self
                .registry
                .get(&provider.kind)
                .map(|p| p.accepts_payload())
                .unwrap_or(true);
            if accepts_payload {
                continue;
            }

            let name = provider.name.clone();
            // Each provider's outcome is already durably recorded (as a sync row) inside
            // `sync_provider`, so one connection that cannot be synced doesn't abort the rest
            // of the batch — the household's other banks are unaffected by this one's bad day.
            match self.sync.sync_provider(provider, None).await {
                // Exhaustive on purpose (CLAUDE.md rule 2): a fourth outcome has to be decided
                // here too, because this is where "did the poll go well?" is answered.
                Ok(sync) => match sync.status {
                    SyncOutcome::Ok => tally.synced += 1,
                    // Already warned once, by `sync_provider`, with the detail attached. Not
                    // warned again per poll: it is a standing state until someone re-links,
                    // and a line every six hours for a month is how a real one gets ignored.
                    SyncOutcome::Disconnected => tally.disconnected += 1,
                    // Not reachable today — `sync_provider` returns `Err` for a failure it
                    // recorded as `error` — but counting it is what keeps this arm honest if
                    // that ever changes.
                    SyncOutcome::Error => tally.failed += 1,
                },
                Err(e) => {
                    tally.failed += 1;
                    tracing::warn!(provider = %name, error = %e, "scheduled provider sync failed");
                }
            }
        }
        tally.report("provider poll finished");
        Ok(TaskRun::Completed)
    }
}

/// What one poll made of the household's connections.
///
/// Reported as a single line rather than left implicit in per-provider logs: with several banks
/// linked, "did anything sync?" is otherwise a question you answer by counting WARNs, and the
/// most important state — a connection the upstream has retired — produces no line at all after
/// the first time it is recorded.
#[derive(Default)]
struct Tally {
    synced: u32,
    disconnected: u32,
    failed: u32,
}

impl Tally {
    fn report(&self, message: &'static str) {
        // At WARN when something needs a person, INFO otherwise: a healthy household polls four
        // times a day and should not be writing warnings, while a disconnected feed silently
        // stops updating an account's balance and is worth finding in a log.
        if self.disconnected > 0 || self.failed > 0 {
            tracing::warn!(
                synced = self.synced,
                disconnected = self.disconnected,
                failed = self.failed,
                "{message}"
            );
        } else {
            tracing::info!(synced = self.synced, "{message}");
        }
    }
}
