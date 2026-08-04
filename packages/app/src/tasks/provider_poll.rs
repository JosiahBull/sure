//! Generalized background poller: syncs every enabled provider that doesn't need a
//! human-supplied payload (e.g. CSV import) — anything credential/API-based, today just
//! `akahu`, gets this for free as new such kinds are added, with no per-kind wiring here.
//! Persistence/audit reuses the same [`SyncService::sync_provider`] path the manual sync
//! route (`sure-api`'s `routes::providers::sync`) drives; scheduling (surviving restarts
//! without early re-runs) is handled generically by `sure_scheduler`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
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

        for provider in providers {
            // Checked before each provider rather than inside one: a sync in flight is
            // committing rows, and the token cannot reach into it (`SyncContext` carries no
            // cancellation — one paginated fetch is bounded by its own wall-clock budget
            // instead, see `sure_providers::akahu`). Stopping between providers is what keeps
            // a household with several connections from spending the whole drain grace on the
            // ones it hadn't got to yet.
            if cancel.is_cancelled() {
                tracing::debug!("provider poll stopped early for shutdown");
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
            // Each provider's failure is already durably recorded (as an "error" sync
            // row) inside `sync_provider`, so one failing provider doesn't need to abort
            // the rest of the batch — just log and move on.
            if let Err(e) = self.sync.sync_provider(provider, None).await {
                tracing::warn!(provider = %name, error = %e, "scheduled provider sync failed");
            }
        }
        Ok(TaskRun::Completed)
    }
}
