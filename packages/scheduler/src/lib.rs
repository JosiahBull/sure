//! Generic, storage-agnostic scheduler for recurring background tasks. Knows nothing
//! about SQL, HTTP, or this app's domain — just "run this job on an interval, but only
//! if it's actually due." [`Scheduler`] periodically checks each registered
//! [`ScheduledTask`] against a [`TaskStateStore`] and runs it once its interval has
//! elapsed since the last *successful* run, durably — so a process restart doesn't
//! cause extra (or missed) work. A failed run is not recorded, so it's retried on the
//! next check rather than waiting out the full interval.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

/// A recurring background job.
#[async_trait]
pub trait ScheduledTask: Send + Sync {
    /// Stable identifier, used as the key in the task-state store (e.g.
    /// `"exchange_rate_poll"`).
    fn name(&self) -> &'static str;
    /// How often this task needs to run.
    fn interval(&self) -> Duration;
    /// Do the work.
    async fn run(&self) -> anyhow::Result<()>;
}

/// Durable "when did each named task last run" state — the persistence port. Only
/// successful runs are recorded (see [`Scheduler`]).
#[async_trait]
pub trait TaskStateStore: Send + Sync {
    async fn last_run_at(&self, task_name: &str) -> anyhow::Result<Option<DateTime<Utc>>>;
    async fn record_run(&self, task_name: &str, at: DateTime<Utc>) -> anyhow::Result<()>;
}

/// Runs registered tasks against a state store, waking up every `check_interval` to see
/// whether any are due. `check_interval` only controls how often the clock is glanced
/// at — it should be much shorter than any registered task's own interval.
pub struct Scheduler {
    store: Arc<dyn TaskStateStore>,
    tasks: Vec<Box<dyn ScheduledTask>>,
    check_interval: Duration,
}

impl Scheduler {
    pub fn new(store: Arc<dyn TaskStateStore>, check_interval: Duration) -> Self {
        Self {
            store,
            tasks: Vec::new(),
            check_interval,
        }
    }

    pub fn register(&mut self, task: Box<dyn ScheduledTask>) {
        self.tasks.push(task);
    }

    /// Run the check loop until `cancel` fires. The first check happens immediately, so a
    /// never-run task executes on startup rather than waiting a full `check_interval`.
    ///
    /// Cancellation is checked *between* tasks, never during one. Dropping a task's
    /// future part-way through would abandon whatever write it was in the middle of and
    /// — because only successful runs are recorded — leave the schedule claiming the task
    /// still hasn't run. The cost is that shutdown waits out the task in flight, which is
    /// why the caller drains this under a deadline rather than trusting it to be quick.
    pub async fn run(self, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(self.check_interval);
        loop {
            tokio::select! {
                // Biased so a cancellation delivered in the same moment as a tick wins:
                // there is no reason to start another sweep on the way out.
                biased;
                () = cancel.cancelled() => break,
                _ = interval.tick() => {}
            }

            for task in &self.tasks {
                if cancel.is_cancelled() {
                    break;
                }
                self.run_if_due(task.as_ref()).await;
            }
        }
        tracing::debug!("scheduler stopped");
    }

    async fn run_if_due(&self, task: &dyn ScheduledTask) {
        let last_run_at = match self.store.last_run_at(task.name()).await {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(task = task.name(), error = %err, "could not read task schedule state");
                return;
            }
        };
        if !is_due(last_run_at, Utc::now(), task.interval()) {
            return;
        }
        match task.run().await {
            Ok(()) => {
                if let Err(err) = self.store.record_run(task.name(), Utc::now()).await {
                    tracing::warn!(task = task.name(), error = %err, "could not record task run");
                }
            }
            Err(err) => {
                tracing::warn!(task = task.name(), error = %err, "scheduled task failed");
            }
        }
    }
}

/// Whether a task last run at `last_run_at` (or never) is due at `now`, given its
/// `interval`. A `last_run_at` in the future (clock skew) is treated as not due, rather
/// than as an overflow error that would otherwise make it due.
fn is_due(last_run_at: Option<DateTime<Utc>>, now: DateTime<Utc>, interval: Duration) -> bool {
    match last_run_at {
        None => true,
        Some(last) => now
            .signed_duration_since(last)
            .to_std()
            .map(|elapsed| elapsed >= interval)
            .unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_when_never_run_before() {
        assert!(is_due(None, Utc::now(), Duration::from_secs(60)));
    }

    #[test]
    fn does_not_run_before_the_interval_elapses() {
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(30);
        assert!(!is_due(Some(last), now, Duration::from_secs(60)));
    }

    #[test]
    fn runs_once_the_interval_has_elapsed() {
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(90);
        assert!(is_due(Some(last), now, Duration::from_secs(60)));
    }

    #[test]
    fn treats_a_future_last_run_as_not_due() {
        let now = Utc::now();
        let last = now + chrono::Duration::seconds(30);
        assert!(!is_due(Some(last), now, Duration::from_secs(60)));
    }
}
