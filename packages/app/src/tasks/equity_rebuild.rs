//! Keeping an equity account's valuation history in step with the grants and prices behind it.
//!
//! **Why this is a background task and not a button.** An account's net-worth history is
//! *derived*: every valuation is the vesting schedule and the price ledger evaluated on a date.
//! Change either — record a new mark, correct a grant's size, enter an exercise — and every
//! valuation from that date forward is stale. Until this task existed the app said so in a
//! notice ("Price saved. Rebuild history to apply it to past valuations.") and offered a
//! *Rebuild history* button, which is a derived figure asking a person to remember to derive it.
//! Forget once and the net-worth chart is quietly wrong for months, with nothing on screen
//! saying which of the two numbers is the stale one.
//!
//! The rebuild is an upsert per value-change date (`sure_dal::equity::write_valuation`, keyed on
//! `(account_id, as_of)` where `source='equity'`), so running it repeatedly converges rather
//! than accumulating. That is what makes it safe to schedule at all — and, with a nudge from
//! whoever wrote the mark, what makes the button unnecessary rather than merely automated.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_scheduler::{ScheduledTask, TaskRun};
use tokio_util::sync::CancellationToken;

use crate::ports::EquityRepo;

/// Slow, because the inputs almost never change on their own: a vesting tranche landing is the
/// only thing that moves a value without somebody typing something, and that happens monthly.
/// Everything else arrives as a nudge the moment it is written, so this interval is the backstop
/// for "a tranche vested overnight", not the mechanism.
const POLL_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub struct EquityRebuildTask {
    equity: Arc<dyn EquityRepo>,
}

impl EquityRebuildTask {
    pub fn new(equity: Arc<dyn EquityRepo>) -> Self {
        Self { equity }
    }
}

#[async_trait]
impl ScheduledTask for EquityRebuildTask {
    fn name(&self) -> &'static str {
        super::BackgroundTask::EquityRebuild.as_str()
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    /// Local — reads and writes only this database — so it runs at boot as well as on its
    /// interval. See `ScheduledTask::run_on_startup`.
    fn run_on_startup(&self) -> bool {
        super::BackgroundTask::EquityRebuild.is_local()
    }

    /// Stops between accounts, never inside one: a rebuild writes one valuation per
    /// value-change date and stopping half way would leave an account's history correct up to
    /// some arbitrary date and stale after it. Each account is a whole item.
    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
        let accounts = self.equity.accounts_with_equity().await?;
        let mut written = 0;
        for account_id in accounts {
            if cancel.is_cancelled() {
                return Ok(TaskRun::Interrupted);
            }
            // `today: None` means "up to today" — the rebuild never writes a future valuation,
            // so a vesting tranche dated next month is picked up by the run after it lands.
            //
            // One account's failure is not the others'. A rebuild refuses when the grant dates
            // imply more valuations than the cap allows, which is a data problem on that account
            // and a permanent one until somebody fixes it: returning `Err` here would mean the
            // task never records a completed run and retries the whole set every check, forever.
            match self.equity.rebuild_history(account_id, None).await {
                Ok(result) => written += result.written,
                Err(err) => tracing::warn!(
                    account_id,
                    error = %err,
                    "could not rebuild this account's equity history; leaving it as it was"
                ),
            }
        }
        if written > 0 {
            tracing::debug!(written, "equity history rebuilt");
        }
        Ok(TaskRun::Completed)
    }
}
