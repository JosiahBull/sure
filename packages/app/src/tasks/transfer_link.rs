//! Background reconciliation of internal transfers. Instead of linking transfer pairs
//! one-shot at import time (which misses a pair whose two sides are imported/synced at
//! different times — e.g. a Sharesies withdrawal imported before its bank deposit is
//! synced), this scheduled task periodically scans every account and links any newly
//! matchable pair. The matching itself lives behind the [`TransferRepo`] port
//! (`sure_dal::transactions::link_transfers` is the real implementation); scheduling
//! (surviving restarts without early re-runs) is handled by `sure_scheduler`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_scheduler::ScheduledTask;

use crate::ports::TransferRepo;

/// How far apart (days) a transfer's two sides may be posted and still be paired — a
/// bank/broker settlement lag of a few days is normal, so allow a small window.
const WINDOW_DAYS: i64 = 5;

/// Runs often enough that a transfer links within a few minutes of either side being
/// imported/synced, while staying cheap: the scan only touches still-unlinked rows, and an
/// unambiguous-match pass is a couple of indexed lookups per row.
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub struct TransferLinkTask {
    transfers: Arc<dyn TransferRepo>,
}

impl TransferLinkTask {
    pub fn new(transfers: Arc<dyn TransferRepo>) -> Self {
        Self { transfers }
    }
}

#[async_trait]
impl ScheduledTask for TransferLinkTask {
    fn name(&self) -> &'static str {
        "transfer_link"
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self) -> anyhow::Result<()> {
        let linked = self.transfers.link_transfers(WINDOW_DAYS).await?;
        if linked > 0 {
            tracing::info!(linked, "auto-linked transfer pairs");
        }
        Ok(())
    }
}
