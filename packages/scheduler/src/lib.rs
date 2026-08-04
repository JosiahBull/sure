//! Generic, storage-agnostic scheduler for recurring background tasks. Knows nothing
//! about SQL, HTTP, or this app's domain — just "run this job on an interval, but only
//! if it's actually due." [`Scheduler`] periodically checks each registered
//! [`ScheduledTask`] against a [`TaskStateStore`] and runs it once its interval has
//! elapsed since the last *successful* run, durably — so a process restart doesn't
//! cause extra (or missed) work. A failed run is not recorded, so it's retried on the
//! next check rather than waiting out the full interval — and a *panicking* run is
//! contained and treated the same way, so one broken job can't take the others down with
//! it (see `Scheduler::run_if_due`).

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
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

    /// Run one task if it's due, recording the run only if it succeeded.
    ///
    /// The `catch_unwind` is load-bearing: this whole loop is a *single* task, so a panic
    /// escaping one job's `run` would unwind the sweep, the loop and the spawned future
    /// with it — every registered job dead for the life of the process, while the HTTP
    /// server carries on answering `/api/health` with `ok`. Note the asymmetry, because it
    /// is easy to be misled by: the same panic raised through
    /// `POST /api/providers/{id}/sync` *is* caught, by `CatchPanicLayer`, and turns into a
    /// scrubbed 500 — but that layer only wraps the HTTP stack beneath it and can see
    /// nothing in here.
    ///
    /// A panic is handled exactly like a returned `Err`, only louder: nothing is recorded,
    /// so the job is retried on the next check rather than waiting out its full interval.
    ///
    /// `AssertUnwindSafe` is justified rather than paved over. Nothing observable survives
    /// the unwind: the future is dropped on the spot, `task` is behind a shared reference
    /// and cannot be mutated through it, and the only mutable state in reach — the run
    /// records — lives behind `store`, whose SQLite transactions do their own recovery. The
    /// worst case is the same one a returned `Err` already has: a job that panicked
    /// half-way through its writes is re-run from the top.
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
        match AssertUnwindSafe(task.run()).catch_unwind().await {
            Ok(Ok(())) => {
                if let Err(err) = self.store.record_run(task.name(), Utc::now()).await {
                    tracing::warn!(task = task.name(), error = %err, "could not record task run");
                }
            }
            Ok(Err(err)) => {
                tracing::warn!(task = task.name(), error = %err, "scheduled task failed");
            }
            // ERROR, not WARN: an `Err` is a job reporting a condition it expected to be
            // possible, a panic is a bug. Without this the only trace was a default
            // panic-hook line on stderr — nothing through `tracing`, nothing correlated to
            // the task that caused it.
            Err(payload) => {
                tracing::error!(
                    task = task.name(),
                    panic = panic_message(payload.as_ref()),
                    "scheduled task panicked; the scheduler survived and will retry it on the next check"
                );
            }
        }
    }
}

/// Whatever text a caught panic carries. `panic!` boxes a `&'static str` for a bare literal
/// and a `String` for a formatted message — those two cover everything the standard
/// machinery (including `unwrap`/`expect`/index-out-of-bounds) produces. A `panic_any` of
/// some other type has no text to show, so say that rather than print a type name nobody
/// can act on.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else {
        "<non-string panic payload>"
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;

    /// A store that reports every task as never-run — so everything is always due — and
    /// remembers what got recorded, which is what lets a test assert on the *absence* of a
    /// record for the task that panicked.
    #[derive(Default)]
    struct RecordingStore {
        recorded: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl TaskStateStore for RecordingStore {
        async fn last_run_at(&self, _task_name: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
            Ok(None)
        }

        async fn record_run(&self, task_name: &str, _at: DateTime<Utc>) -> anyhow::Result<()> {
            self.recorded.lock().unwrap().push(task_name.to_string());
            Ok(())
        }
    }

    const PANICKING: &str = "panicking_task";
    const HEALTHY: &str = "healthy_task";

    /// Stands in for the real failure mode: an `unwrap` inside a provider poll, say, that
    /// nothing between here and the runtime catches.
    struct PanickingTask(Arc<AtomicUsize>);

    #[async_trait]
    impl ScheduledTask for PanickingTask {
        fn name(&self) -> &'static str {
            PANICKING
        }
        fn interval(&self) -> Duration {
            Duration::ZERO
        }
        async fn run(&self) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("scheduled task blew up");
        }
    }

    struct CountingTask(Arc<AtomicUsize>);

    #[async_trait]
    impl ScheduledTask for CountingTask {
        fn name(&self) -> &'static str {
            HEALTHY
        }
        fn interval(&self) -> Duration {
            Duration::ZERO
        }
        async fn run(&self) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// The whole point of the `catch_unwind`: five jobs share one task, so without it the
    /// first `unwrap` in any of them ends *all* background work for the life of the
    /// process. Expect a panic-hook line on stderr while this test runs — the payload is
    /// caught and logged, not suppressed.
    #[tokio::test]
    async fn a_panicking_task_stops_neither_the_loop_nor_its_siblings() {
        let store = Arc::new(RecordingStore::default());
        let panics = Arc::new(AtomicUsize::new(0));
        let healthy_runs = Arc::new(AtomicUsize::new(0));

        let mut scheduler = Scheduler::new(store.clone(), Duration::from_millis(1));
        // Registered first, so on every sweep it panics *before* the sibling is reached.
        scheduler.register(Box::new(PanickingTask(panics.clone())));
        scheduler.register(Box::new(CountingTask(healthy_runs.clone())));

        let cancel = CancellationToken::new();
        // A bare `tokio::spawn` rather than `Shutdown::spawn`: this crate deliberately
        // doesn't depend on `sure-appbase`, and what the test needs is the raw `JoinHandle`
        // to prove the loop returned instead of unwinding.
        let handle = tokio::spawn(scheduler.run(cancel.clone()));

        // Two sweeps, so this asserts "survived a panic and came back round" rather than
        // merely "reached the sibling once".
        tokio::time::timeout(Duration::from_secs(5), async {
            while healthy_runs.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the sibling task should still be running after the panic");

        cancel.cancel();
        handle.await.expect("the scheduler loop must not unwind");

        // The panicking task is retried, not quietly disabled.
        assert!(panics.load(Ordering::SeqCst) >= 2);

        let recorded = store.recorded.lock().unwrap();
        assert!(!recorded.is_empty(), "the healthy task's runs are recorded");
        // …and the panicking one is left looking never-run, which is what makes the next
        // check pick it up again instead of waiting out its interval.
        assert!(
            !recorded.iter().any(|name| name == PANICKING),
            "a panicked run must not be recorded: {recorded:?}"
        );
    }

    #[test]
    fn reads_the_text_of_a_caught_panic() {
        let payload = std::panic::catch_unwind(|| {
            panic!("literal message");
        })
        .expect_err("the closure panics");
        assert_eq!(panic_message(payload.as_ref()), "literal message");

        let n = 7;
        let payload = std::panic::catch_unwind(|| {
            panic!("formatted {n}");
        })
        .expect_err("the closure panics");
        assert_eq!(panic_message(payload.as_ref()), "formatted 7");

        // `panic_any` of something with no text: named, rather than logged as an empty
        // message that reads as though the panic had no cause.
        let payload = std::panic::catch_unwind(|| {
            std::panic::panic_any(7u8);
        })
        .expect_err("the closure panics");
        assert_eq!(
            panic_message(payload.as_ref()),
            "<non-string panic payload>"
        );
    }

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
