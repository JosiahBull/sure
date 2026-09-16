//! Generic, storage-agnostic scheduler for recurring background tasks. Knows nothing
//! about SQL, HTTP, or this app's domain — just "run this job on an interval, but only
//! if it's actually due." [`Scheduler`] periodically checks each registered
//! [`ScheduledTask`] against a [`TaskStateStore`] and runs it once its interval has
//! elapsed since the last *successful* run, durably — so a process restart doesn't
//! cause extra (or missed) work. A failed run is not recorded, so it's retried on the
//! next check rather than waiting out the full interval — and a *panicking* run is
//! contained and treated the same way, so one broken job can't take the others down with
//! it (see `Scheduler::run_if_due`). Shutdown is cooperative in both directions: the loop
//! stops between tasks, and each task is handed the same [`CancellationToken`] so a long
//! multi-item sweep can stop between items and say so ([`TaskRun::Interrupted`]) rather than
//! being waited out — or, past the drain deadline, abandoned mid-write.

use std::any::Any;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// How much of its work a run got done. Decides whether the run is recorded, so it is a
/// two-variant enum rather than a `bool`: [`Scheduler::run_if_due`] matches it exhaustively
/// (CLAUDE.md rule 2) and a third outcome added later has to be answered there, not defaulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRun {
    /// Everything this run had to do is done. Recorded, so the interval starts again from now.
    Completed,
    /// The run stopped early because the cancellation token fired part-way through — a
    /// shutdown, not a failure. **Deliberately not recorded**: the schedule keeps claiming
    /// the task hasn't run since its last *complete* run, so the next process start picks it
    /// up immediately instead of leaving the half-done sweep to age out its full interval.
    /// Reported at DEBUG, not WARN: nothing went wrong.
    Interrupted,
}

/// How long the loop waits after a nudge before sweeping, so a burst becomes one run.
///
/// An import writes hundreds of rows and finishes with a single nudge, but several *different*
/// things routinely finish together — a provider sync ends, the transfer linker pairs what it
/// brought in, a valuation lands — and each wants the same derived tasks re-run. Without a
/// settle window that is three sweeps in as many milliseconds, all doing the same work.
const NUDGE_SETTLE: Duration = Duration::from_millis(250);

/// A handle for telling the scheduler that a task's inputs have changed and it should run now
/// rather than at its next interval.
///
/// The interval is a floor on *staleness*, not a statement that nothing can happen sooner. A
/// person who has just edited an income stream, finished an import, or recorded a share price is
/// watching the screen, and waiting out five minutes for a derived figure to catch up reads as
/// the app being broken — which is exactly how the manual "Rebuild history" button came to
/// exist. The point of nudging is that the button should never have been the answer.
///
/// Cheap and idempotent: nudges for one task coalesce into one pending entry, and a nudge for a
/// task nobody registered is silently dropped rather than an error, so a caller never has to
/// know which tasks this process happens to run.
#[derive(Clone, Default)]
pub struct Nudge(Arc<NudgeInner>);

#[derive(Default)]
struct NudgeInner {
    pending: Mutex<HashSet<&'static str>>,
    notify: Notify,
}

impl Nudge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask for `task` to run at the next opportunity. Never blocks; safe from any thread.
    pub fn wake(&self, task: &'static str) {
        // Poisoning is ignored: the only thing behind this lock is a set of static strings, so a
        // panic elsewhere cannot have left it meaningfully inconsistent.
        self.0
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task);
        // `notify_one` rather than `notify_waiters`: it stores a permit when the loop is busy
        // sweeping, so a nudge that lands mid-sweep is picked up on the next pass instead of
        // being dropped on the floor.
        self.0.notify.notify_one();
    }

    async fn notified(&self) {
        self.0.notify.notified().await;
    }

    fn take(&self) -> HashSet<&'static str> {
        std::mem::take(&mut *self.0.pending.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// A recurring background job.
#[async_trait]
pub trait ScheduledTask: Send + Sync {
    /// Stable identifier, used as the key in the task-state store (e.g.
    /// `"exchange_rate_poll"`).
    fn name(&self) -> &'static str;
    /// How often this task needs to run.
    fn interval(&self) -> Duration;
    /// Whether to run once on startup even when the interval has not elapsed.
    ///
    /// Default `false`, and that default is the safe one: the tasks that talk to a third party
    /// must **not** opt in. A restart re-runs them, and a process that restarts in a loop — a
    /// crash loop, or a developer saving a file under a reloading dev server — would turn that
    /// into a burst against someone else's rate limit, which is charged to the household rather
    /// than to this app (see `docs/HTTP.md`).
    ///
    /// The tasks that *should* opt in are the purely local, derived ones: they read this
    /// database and write this database, so the only cost is a few queries, and the benefit is
    /// that whatever changed while the process was down — or whatever the last release computes
    /// differently — is reconciled at boot instead of up to a full interval later.
    fn run_on_startup(&self) -> bool {
        false
    }
    /// Do the work.
    ///
    /// `cancel` is the process-wide shutdown token, handed down so a task that loops over
    /// many items (every provider, every ticker, every currency) can stop *between* items and
    /// return [`TaskRun::Interrupted`]. Checking it is what makes the drain fast rather than
    /// merely bounded: without it, shutdown waits out whatever sweep was in flight and — once
    /// past `SHUTDOWN_DRAIN_GRACE_SECS` (10s, see `docs/HTTP.md`) — abandons the task
    /// mid-write, which is the difference between "finished" and "abandoned" in the shutdown
    /// report.
    ///
    /// What a task must **not** do is check it mid-write: the contract is "stop at a point
    /// where stopping loses nothing", i.e. between whole items, never between the two halves
    /// of one item's writes.
    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun>;
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
    nudge: Nudge,
}

impl Scheduler {
    pub fn new(store: Arc<dyn TaskStateStore>, check_interval: Duration) -> Self {
        Self {
            store,
            tasks: Vec::new(),
            check_interval,
            nudge: Nudge::new(),
        }
    }

    /// A handle for waking this scheduler between ticks. Clone it to whoever writes the inputs
    /// a task derives from; see [`Nudge`].
    pub fn nudge(&self) -> Nudge {
        self.nudge.clone()
    }

    /// Listen on a handle that already exists, instead of this scheduler's own.
    ///
    /// For a composition root that has to hand the handle to the HTTP state before it builds the
    /// scheduler — the alternative is constructing the scheduler first purely to borrow its
    /// handle, which orders the wiring around this detail rather than around what depends on
    /// what.
    #[must_use]
    pub fn with_nudge(mut self, nudge: Nudge) -> Self {
        self.nudge = nudge;
        self
    }

    pub fn register(&mut self, task: Box<dyn ScheduledTask>) {
        self.tasks.push(task);
    }

    /// The names of every registered task, in registration order.
    ///
    /// For the telemetry sampler, which reports how long ago each task last completed. Asked of
    /// the scheduler rather than listed a second time at the call site: a hand-maintained copy
    /// would silently stop covering a task the day one is added, which is precisely the day the
    /// gauge matters.
    pub fn task_names(&self) -> Vec<&'static str> {
        self.tasks.iter().map(|task| task.name()).collect()
    }

    /// Run the check loop until `cancel` fires. The first check happens immediately, so a
    /// never-run task executes on startup rather than waiting a full `check_interval`.
    ///
    /// The loop never *drops* a task's future: cancellation is checked between tasks here, and
    /// handed to the task itself so it can stop between its own items (see
    /// [`ScheduledTask::run`] and [`TaskRun::Interrupted`]). Dropping the future part-way
    /// through would abandon whatever write it was in the middle of, and — because only
    /// completed runs are recorded — leave the schedule claiming the task still hasn't run.
    /// Cooperative cancellation is what keeps the drain quick without that cost; the caller
    /// still drains this under a deadline, because a task is free to ignore the token and a
    /// single in-flight upstream request is bounded by its own timeout rather than by us.
    pub async fn run(self, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(self.check_interval);
        // The first sweep is the startup one, and the only sweep on which `run_on_startup`
        // overrides the interval. Set false after it however it went, so a task that failed at
        // boot waits for its interval like any other rather than re-running every pass.
        let mut startup = true;
        loop {
            // Which tasks were explicitly asked for this pass, as opposed to merely being due.
            let nudged = tokio::select! {
                // Biased so a cancellation delivered in the same moment as a tick wins:
                // there is no reason to start another sweep on the way out.
                biased;
                () = cancel.cancelled() => break,
                _ = interval.tick() => HashSet::new(),
                () = self.nudge.notified() => {
                    // Let a burst settle before sweeping, and let a cancellation through it —
                    // an unconditional sleep here would add `NUDGE_SETTLE` to every shutdown
                    // that happens to land just after a change.
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(NUDGE_SETTLE) => {}
                    }
                    self.nudge.take()
                }
            };

            for task in &self.tasks {
                if cancel.is_cancelled() {
                    break;
                }
                // Two ways to jump the interval, and both are deliberate rather than merely
                // early: something changed under this task, or the process just started and its
                // inputs may have moved while it was down.
                let forced = nudged.contains(task.name()) || (startup && task.run_on_startup());
                self.run_if_due(task.as_ref(), &cancel, forced).await;
            }
            startup = false;
        }
        tracing::debug!("scheduler stopped");
    }

    /// Run one task if it's due, recording the run only if it ran to completion — not if it
    /// failed, panicked, or stopped early for shutdown.
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
    async fn run_if_due(&self, task: &dyn ScheduledTask, cancel: &CancellationToken, forced: bool) {
        let last_run_at = match self.store.last_run_at(task.name()).await {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(task = task.name(), error = %err, "could not read task schedule state");
                // Counted, because until now this path was invisible: the task silently does
                // not run, its `last_run_at` never advances, and the only evidence is one WARN
                // among a day's logs. A non-zero rate here means jobs are not running at all.
                record_outcome(task.name(), JobOutcome::StateUnavailable, None);
                return;
            }
        };
        if !forced && !is_due(last_run_at, Utc::now(), task.interval()) {
            return;
        }
        // Only the runs that actually happen are timed — `is_due` returning false is not a
        // zero-length job, and recording it would bury the real durations under a check that
        // fires every minute per task.
        let started = Instant::now();
        let outcome = match AssertUnwindSafe(task.run(cancel)).catch_unwind().await {
            Ok(Ok(TaskRun::Completed)) => {
                if let Err(err) = self.store.record_run(task.name(), Utc::now()).await {
                    tracing::warn!(task = task.name(), error = %err, "could not record task run");
                }
                JobOutcome::Completed
            }
            // Not recorded on purpose — see `TaskRun::Interrupted`. A shutdown that lands
            // half-way through a sweep must not look like a completed run, or the work skipped
            // on the way out waits out a full interval after the restart.
            Ok(Ok(TaskRun::Interrupted)) => {
                tracing::debug!(
                    task = task.name(),
                    "scheduled task stopped early for shutdown; not recorded, so it runs again on the next check"
                );
                JobOutcome::Interrupted
            }
            Ok(Err(err)) => {
                tracing::warn!(task = task.name(), error = %err, "scheduled task failed");
                JobOutcome::Failed
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
                JobOutcome::Panicked
            }
        };
        record_outcome(task.name(), outcome, Some(started.elapsed()));
    }
}

/// How a scheduled run ended.
///
/// An enum rather than a `&str` label built at each site (CLAUDE.md rule 1), so the five
/// outcomes `run_if_due` can produce are a closed set and a sixth cannot be added without
/// deciding what it means here. `StateUnavailable` is the one that is not a job result at all:
/// the schedule could not be read, so the job never started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JobOutcome {
    Completed,
    Interrupted,
    Failed,
    Panicked,
    StateUnavailable,
}

impl JobOutcome {
    fn as_str(self) -> &'static str {
        match self {
            JobOutcome::Completed => "completed",
            JobOutcome::Interrupted => "interrupted",
            JobOutcome::Failed => "failed",
            JobOutcome::Panicked => "panicked",
            JobOutcome::StateUnavailable => "state_unavailable",
        }
    }
}

/// Count a run, and time it when there was one to time.
///
/// `task` is a `&'static str` from [`ScheduledTask::name`] and the outcome is a closed enum, so
/// the label set is the five registered jobs times five outcomes at most.
fn record_outcome(task: &'static str, outcome: JobOutcome, elapsed: Option<Duration>) {
    let attributes = [
        sure_telemetry::KeyValue::new("job", task),
        sure_telemetry::KeyValue::new("outcome", outcome.as_str()),
    ];
    let instruments = sure_telemetry::instruments();
    instruments.scheduler_job_total.add(1, &attributes);
    if let Some(elapsed) = elapsed {
        instruments
            .scheduler_job_duration
            .record(sure_telemetry::secs(elapsed), &attributes);
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// A store where nothing is ever due: every task has just run. The only thing that can make
    /// a task run against it is an override — a nudge, or the startup pass.
    struct JustRanStore;

    #[async_trait]
    impl TaskStateStore for JustRanStore {
        async fn last_run_at(&self, _task_name: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
            Ok(Some(Utc::now()))
        }
        async fn record_run(&self, _task_name: &str, _at: DateTime<Utc>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A task with a long interval that counts its runs, and says whether it wants the startup
    /// pass. Everything below turns on "did this run when it was not due".
    struct IdleTask {
        name: &'static str,
        runs: Arc<AtomicUsize>,
        on_startup: bool,
    }

    #[async_trait]
    impl ScheduledTask for IdleTask {
        fn name(&self) -> &'static str {
            self.name
        }
        fn interval(&self) -> Duration {
            Duration::from_secs(3600)
        }
        fn run_on_startup(&self) -> bool {
            self.on_startup
        }
        async fn run(&self, _cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(TaskRun::Completed)
        }
    }

    const PANICKING: &str = "panicking_task";
    const HEALTHY: &str = "healthy_task";
    const COOPERATIVE: &str = "cooperative_task";

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
        async fn run(&self, _cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
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
        async fn run(&self, _cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(TaskRun::Completed)
        }
    }

    /// The shape every real task now has: a loop over many items that checks the token at the
    /// top of each iteration. `ITEMS` is far more work than the test lets it finish, so
    /// "stopped because it was cancelled" is distinguishable from "ran out of items".
    struct CooperativeTask {
        started: Arc<AtomicUsize>,
        processed: Arc<AtomicUsize>,
    }

    impl CooperativeTask {
        const ITEMS: usize = 1_000_000;
    }

    #[async_trait]
    impl ScheduledTask for CooperativeTask {
        fn name(&self) -> &'static str {
            COOPERATIVE
        }
        fn interval(&self) -> Duration {
            Duration::ZERO
        }
        async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
            self.started.fetch_add(1, Ordering::SeqCst);
            for _ in 0..Self::ITEMS {
                if cancel.is_cancelled() {
                    return Ok(TaskRun::Interrupted);
                }
                self.processed.fetch_add(1, Ordering::SeqCst);
                // Yield rather than sleep: the test needs the task to be *interruptible*, not
                // slow, and a timer would make the assertion depend on wall-clock timing.
                tokio::task::yield_now().await;
            }
            Ok(TaskRun::Completed)
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

    /// W-17, the scheduler half: the drain has to be *fast*, not merely bounded. Before the
    /// token reached `run`, a sweep in flight when `SIGTERM` landed ran to its own end — up to
    /// 83 minutes for a paginated Akahu fetch against a slow-but-up upstream — so the 10s
    /// drain grace expired and the task was abandoned mid-write. Here the task is mid-sweep
    /// with ~a million items left when cancellation lands, and the whole loop still returns
    /// well inside the grace.
    #[tokio::test]
    async fn a_cancelled_task_stops_mid_sweep_instead_of_running_to_its_end() {
        let store = Arc::new(RecordingStore::default());
        let started = Arc::new(AtomicUsize::new(0));
        let processed = Arc::new(AtomicUsize::new(0));

        let mut scheduler = Scheduler::new(store.clone(), Duration::from_millis(1));
        scheduler.register(Box::new(CooperativeTask {
            started: started.clone(),
            processed: processed.clone(),
        }));

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(scheduler.run(cancel.clone()));

        // Wait until the task is genuinely inside its loop, so this asserts "interrupted
        // part-way" rather than "never started".
        tokio::time::timeout(Duration::from_secs(5), async {
            while processed.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the task should start on the first sweep");

        cancel.cancel();
        // The number that matters: comfortably under `SHUTDOWN_DRAIN_GRACE_SECS` (10s), and
        // nowhere near the time the full million items would take.
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the loop must return promptly once cancelled")
            .expect("the scheduler loop must not unwind");

        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(
            processed.load(Ordering::SeqCst) < CooperativeTask::ITEMS,
            "the sweep must have stopped early, not run to its end"
        );

        // And an interrupted run is not recorded, so the restart picks it straight back up
        // instead of waiting out its interval with half the work done.
        let recorded = store.recorded.lock().unwrap();
        assert!(
            !recorded.iter().any(|name| name == COOPERATIVE),
            "an interrupted run must not be recorded: {recorded:?}"
        );
    }

    /// The other half of the same contract, at the unit level: a run that *completes* is
    /// recorded, an `Interrupted` one is not. Asserted through `run_if_due` so it covers the
    /// match arms rather than the enum alone.
    #[tokio::test]
    async fn records_a_completed_run_and_not_an_interrupted_one() {
        let store = Arc::new(RecordingStore::default());
        let scheduler = Scheduler::new(store.clone(), Duration::from_millis(1));

        let healthy = CountingTask(Arc::new(AtomicUsize::new(0)));
        scheduler
            .run_if_due(&healthy, &CancellationToken::new(), false)
            .await;
        assert_eq!(store.recorded.lock().unwrap().as_slice(), [HEALTHY]);

        // Same task type, same store — only the token differs, and an already-cancelled token
        // is what a task sees when the signal lands just as its turn comes up.
        let cooperative = CooperativeTask {
            started: Arc::new(AtomicUsize::new(0)),
            processed: Arc::new(AtomicUsize::new(0)),
        };
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        scheduler.run_if_due(&cooperative, &cancelled, false).await;
        assert_eq!(
            store.recorded.lock().unwrap().as_slice(),
            [HEALTHY],
            "the interrupted run must add nothing"
        );
        assert_eq!(
            cooperative.processed.load(Ordering::SeqCst),
            0,
            "a token already cancelled stops the sweep before the first item"
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

    /// A task that opted in runs at boot even though its interval says it is not due; one that
    /// did not, does not. The second half is the one that matters — it is what keeps a restart
    /// loop from turning every boot into a third-party request.
    #[tokio::test]
    async fn startup_runs_only_the_tasks_that_opted_in() {
        let eager = Arc::new(AtomicUsize::new(0));
        let lazy = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(Arc::new(JustRanStore), Duration::from_millis(50));
        scheduler.register(Box::new(IdleTask {
            name: "eager",
            runs: eager.clone(),
            on_startup: true,
        }));
        scheduler.register(Box::new(IdleTask {
            name: "lazy",
            runs: lazy.clone(),
            on_startup: false,
        }));

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(scheduler.run(cancel.clone()));
        // Long enough for several ticks, so a second startup run would show up as a count of 2.
        tokio::time::sleep(Duration::from_millis(250)).await;
        cancel.cancel();
        handle.await.unwrap();

        assert_eq!(
            eager.load(Ordering::SeqCst),
            1,
            "opted in: runs once, at boot"
        );
        assert_eq!(
            lazy.load(Ordering::SeqCst),
            0,
            "not opted in: waits for its interval"
        );
    }

    /// A nudge runs a task that is not due. The task it did not name stays put, which is what
    /// makes a nudge a request about one task rather than a general "sweep now".
    #[tokio::test]
    async fn a_nudge_runs_only_the_task_it_names() {
        let wanted = Arc::new(AtomicUsize::new(0));
        let other = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(Arc::new(JustRanStore), Duration::from_secs(3600));
        scheduler.register(Box::new(IdleTask {
            name: "wanted",
            runs: wanted.clone(),
            on_startup: false,
        }));
        scheduler.register(Box::new(IdleTask {
            name: "other",
            runs: other.clone(),
            on_startup: false,
        }));
        let nudge = scheduler.nudge();

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(scheduler.run(cancel.clone()));
        // After the immediate first tick, so this is a wake rather than the startup sweep.
        tokio::time::sleep(Duration::from_millis(50)).await;
        nudge.wake("wanted");
        tokio::time::sleep(Duration::from_millis(400)).await;
        cancel.cancel();
        handle.await.unwrap();

        assert_eq!(wanted.load(Ordering::SeqCst), 1);
        assert_eq!(
            other.load(Ordering::SeqCst),
            0,
            "a nudge is not a general sweep"
        );
    }

    /// A burst of nudges for one task is one run, not one per nudge — an import that finishes
    /// beside a sync beside a valuation must not sweep three times.
    #[tokio::test]
    async fn a_burst_of_nudges_coalesces_into_one_run() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(Arc::new(JustRanStore), Duration::from_secs(3600));
        scheduler.register(Box::new(IdleTask {
            name: "wanted",
            runs: runs.clone(),
            on_startup: false,
        }));
        let nudge = scheduler.nudge();

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(scheduler.run(cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        for _ in 0..25 {
            nudge.wake("wanted");
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        cancel.cancel();
        handle.await.unwrap();

        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "25 nudges inside the settle window are one sweep"
        );
    }

    /// A nudge naming a task nobody registered is dropped rather than doing anything: the
    /// scheduler cannot tell a typo from a task this deployment does not run, and a caller
    /// should not have to know which it is.
    #[tokio::test]
    async fn a_nudge_for_an_unregistered_task_is_harmless() {
        let runs = Arc::new(AtomicUsize::new(0));
        let mut scheduler = Scheduler::new(Arc::new(JustRanStore), Duration::from_secs(3600));
        scheduler.register(Box::new(IdleTask {
            name: "registered",
            runs: runs.clone(),
            on_startup: false,
        }));
        let nudge = scheduler.nudge();

        let cancel = CancellationToken::new();
        let handle = tokio::spawn(scheduler.run(cancel.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        nudge.wake("no_such_task");
        tokio::time::sleep(Duration::from_millis(400)).await;
        cancel.cancel();
        handle.await.unwrap();

        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
}
