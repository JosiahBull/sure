//! Background income matching — the [`crate::income_match::IncomeMatchService`] pass on a
//! timer, the same arrangement as `transfer_link` and for the same reason: an expected payday
//! and its deposit do not arrive together (the schedule exists before the sync lands the
//! money), so a scheduled scan beats a one-shot import hook. The pass is idempotent end to
//! end, which is what makes the timer safe.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_scheduler::{ScheduledTask, TaskRun};
use tokio_util::sync::CancellationToken;

use crate::income_match::IncomeMatchService;

/// Matches land within a few minutes of a sync, like transfer links; the pass over a
/// household's handful of streams is a few indexed queries.
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub struct IncomeMatchTask {
    service: Arc<IncomeMatchService>,
}

impl IncomeMatchTask {
    pub fn new(service: Arc<IncomeMatchService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl ScheduledTask for IncomeMatchTask {
    fn name(&self) -> &'static str {
        "income_match"
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    /// `cancel` unused for the same reason as `transfer_link`: the pass is a bounded set of
    /// SQLite queries with no upstream to wait on, so the drain just waits one run out.
    async fn run(&self, _cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
        let summary = self.service.run().await?;
        if summary.matched > 0 || summary.repaired > 0 || summary.pruned > 0 {
            tracing::info!(
                matched = summary.matched,
                repaired = summary.repaired,
                pruned = summary.pruned,
                "income match pass"
            );
        }
        Ok(TaskRun::Completed)
    }
}
