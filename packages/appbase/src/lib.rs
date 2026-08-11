//! Process lifecycle: run an async application until it finishes or the operator stops
//! it, then shut down in a defined order under a bounded budget — and say honestly what
//! was still running when the process went away.
//!
//! # The sequence
//!
//! [`run`] drives five phases. Every one of them happens on *every* exit path, including
//! the one where the application returns an error:
//!
//! 1. **Serve.** The application future runs against the [`Shutdown`] handle, racing
//!    `SIGINT`/`SIGTERM`. Whichever finishes first decides the [`Trigger`].
//! 2. **Pre-drain delay.** Only when the trigger came from outside. A load balancer or a
//!    Kubernetes service needs a moment to stop routing to a process that has been told
//!    to stop; an application that simply ran out of work has nothing to wait for, and
//!    delaying it would be pure latency. A second signal skips the wait.
//! 3. **Cancel.** [`Shutdown::cancel`], once, for everything downstream.
//! 4. **Drain.** Wait for the application future to return, then for every task spawned
//!    through the handle. Both are `await`s on real completion signals, not polls.
//! 5. **Backstop.** [`Runtime::shutdown_timeout`] for blocking-pool work nothing above
//!    can see.
//!
//! Each phase has its own grace period ([`LifecycleConfig`]) and the sequence as a whole
//! is capped at their sum, so an overrunning phase eats into the budget but can never
//! leave a later phase with a negative one.
//!
//! # What this deliberately does not do
//!
//! It does not build the runtime. `sure-server` has to apply its Landlock sandbox while
//! the process is still single-threaded, which means it must own the moment the runtime
//! is constructed; a lifecycle helper that insisted on building its own would make that
//! impossible. Hand [`run`] a runtime you built.
//!
//! It does not count tasks. Sampling [`RuntimeMetrics::num_alive_tasks`][alive] and
//! waiting for it to fall below some threshold cannot distinguish the application's work
//! from a client library's idle connection reaper, so the threshold becomes a magic
//! number that is wrong in both directions: too high and real work is abandoned, too low
//! and every shutdown burns its whole budget. [`Shutdown`] tracks what the application
//! actually spawned instead.
//!
//! [`Runtime::shutdown_timeout`]: tokio::runtime::Runtime::shutdown_timeout
//! [alive]: tokio::runtime::RuntimeMetrics::num_alive_tasks
//!
//! # Example
//!
//! ```no_run
//! # use std::time::Duration;
//! use sure_appbase::{run, LifecycleConfig, Shutdown};
//!
//! async fn serve(shutdown: Shutdown) -> anyhow::Result<()> {
//!     shutdown.spawn({
//!         let shutdown = shutdown.clone();
//!         async move {
//!             // Background work that stops when asked.
//!             shutdown.cancelled().await;
//!         }
//!     });
//!     shutdown.cancelled().await;
//!     Ok(())
//! }
//!
//! # fn main() -> anyhow::Result<()> {
//! let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
//! run(runtime, LifecycleConfig::default(), serve).result
//! # }
//! ```

mod callsite;
mod shutdown;
mod signal;

use std::future::Future;
use std::pin::pin;
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;

pub use shutdown::{DrainOutcome, Shutdown};
// Re-exported so an application can name the token type without depending on
// `tokio-util` directly.
pub use tokio_util::sync::CancellationToken;

use crate::signal::Signals;

/// How long each phase of the shutdown sequence gets.
///
/// The total budget is the sum of these. They are separate rather than one number carved
/// up by subtraction because subtraction is where the arithmetic goes wrong: a phase that
/// overruns silently steals from the next, and the last one ends up with a negative
/// remainder that has to be clamped and then explained.
#[derive(Clone, Copy, Debug)]
pub struct LifecycleConfig {
    /// Phase 2. How long to keep serving after an external signal, before anything is
    /// cancelled, so whatever routes traffic here has time to notice.
    ///
    /// Zero by default: this is a local-first application with nothing in front of it.
    /// A deployment behind a load balancer wants roughly its health-check interval.
    pub predrain_delay: Duration,
    /// Phase 4a. How long the application future gets to return after cancellation. Must
    /// comfortably exceed whatever the application does in its own teardown — for
    /// `sure-server` that is the HTTP connection drain plus closing the database pool.
    pub app_grace: Duration,
    /// Phase 4b. How long the tasks spawned through [`Shutdown`] get, once the
    /// application future has returned.
    pub drain_grace: Duration,
    /// Phase 5. How long the blocking pool gets. A backstop for work that was never
    /// tracked, so it is short by design.
    pub blocking_grace: Duration,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            predrain_delay: Duration::ZERO,
            app_grace: Duration::from_secs(30),
            drain_grace: Duration::from_secs(10),
            blocking_grace: Duration::from_secs(5),
        }
    }
}

impl LifecycleConfig {
    /// The ceiling on the whole sequence, from the moment shutdown begins.
    pub fn total_budget(&self) -> Duration {
        self.predrain_delay
            .saturating_add(self.app_grace)
            .saturating_add(self.drain_grace)
            .saturating_add(self.blocking_grace)
    }
}

/// What started the shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// The application future returned `Ok` — it ran out of work.
    Completed,
    /// The application future returned `Err`.
    Failed,
    /// The application called [`Shutdown::cancel`] itself — a background task hit
    /// something fatal that the main future had no way to observe.
    Requested,
    /// `SIGINT`: a Ctrl-C at a terminal.
    Interrupt,
    /// `SIGTERM`: what a container runtime or `systemd` sends first.
    Terminate,
}

impl Trigger {
    /// Whether shutdown was asked for from outside the process. Only an external trigger
    /// gets the pre-drain delay — see [`LifecycleConfig::predrain_delay`].
    ///
    /// [`Requested`](Trigger::Requested) is not external despite coming from outside the
    /// main future: the token is already cancelled by the time it is observed, so a delay
    /// would keep taking traffic that nothing is left to serve.
    fn is_external(self) -> bool {
        match self {
            Trigger::Interrupt | Trigger::Terminate => true,
            Trigger::Completed | Trigger::Failed | Trigger::Requested => false,
        }
    }

    /// The one legal rendering of this value as text, for a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Completed => "completed",
            Trigger::Failed => "failed",
            Trigger::Requested => "requested",
            Trigger::Interrupt => "interrupt",
            Trigger::Terminate => "terminate",
        }
    }
}

/// What became of the application future.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppOutcome {
    /// It returned within its grace period.
    Finished,
    /// It was still running when the grace period expired, and was dropped mid-flight.
    Abandoned,
}

impl AppOutcome {
    /// The one legal rendering of this value as text, for a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            AppOutcome::Finished => "finished",
            AppOutcome::Abandoned => "abandoned",
        }
    }
}

/// What became of the blocking pool.
///
/// Inferred rather than reported: [`Runtime::shutdown_timeout`][st] returns `()` whether
/// it drained or gave up, so the only available signal is whether the call used its whole
/// grace. Accurate enough for a backstop, and the case it cannot distinguish gets its own
/// variant rather than being rounded to the flattering one.
///
/// [st]: tokio::runtime::Runtime::shutdown_timeout
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockingOutcome {
    /// The pool was empty, or emptied within its grace.
    Drained,
    /// The grace elapsed with blocking work still running; it was abandoned.
    TimedOut,
    /// There was no budget left, so the pool was never given a chance and nothing can be
    /// said about what was on it.
    Skipped,
}

impl BlockingOutcome {
    /// The one legal rendering of this value as text, for a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            BlockingOutcome::Drained => "drained",
            BlockingOutcome::TimedOut => "timed_out",
            BlockingOutcome::Skipped => "skipped",
        }
    }
}

/// What the shutdown sequence did, phase by phase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    pub trigger: Trigger,
    pub app: AppOutcome,
    pub drain: DrainOutcome,
    pub blocking: BlockingOutcome,
    /// Wall-clock from the start of the shutdown sequence to the end of phase 5. Excludes
    /// however long the application spent serving.
    pub elapsed: Duration,
}

impl ShutdownReport {
    /// Whether the process exited with nothing left running.
    ///
    /// The single question worth asking of a shutdown, and the one the e2e suite asserts.
    /// A `false` here means work was dropped mid-flight — which, for anything that writes
    /// to durable state, is the difference between a clean restart and a torn one.
    pub fn is_clean(&self) -> bool {
        let app_finished = match self.app {
            AppOutcome::Finished => true,
            AppOutcome::Abandoned => false,
        };
        let drained = match &self.drain {
            DrainOutcome::Drained { .. } => true,
            DrainOutcome::TimedOut { .. } => false,
        };
        let blocking_done = match self.blocking {
            BlockingOutcome::Drained => true,
            BlockingOutcome::TimedOut | BlockingOutcome::Skipped => false,
        };
        app_finished && drained && blocking_done
    }

    /// Log the report: one INFO summary always, plus a WARN for each thing that was left
    /// behind.
    ///
    /// The summary is emitted at INFO even when the shutdown was dirty, so a log filter
    /// tuned to INFO gets the whole picture from one line and does not have to correlate
    /// it against the warnings.
    pub fn record(&self) {
        match self.app {
            AppOutcome::Finished => {}
            AppOutcome::Abandoned => {
                tracing::warn!("the application did not return before its deadline; dropped");
            }
        }
        match &self.drain {
            DrainOutcome::Drained { .. } => {}
            // One line per abandoned task, each naming the line that spawned it — the
            // point of the whole call-site table. In a release build `sites` is empty and
            // this degrades to the count, which is what the summary carries anyway.
            DrainOutcome::TimedOut { abandoned, sites } => {
                tracing::warn!(
                    abandoned,
                    "drain deadline exceeded; tasks left running (spawn sites below)"
                );
                for site in sites {
                    tracing::warn!(site = site.as_str(), "task still running at shutdown");
                }
            }
        }
        match self.blocking {
            BlockingOutcome::Drained => {}
            BlockingOutcome::TimedOut => {
                tracing::warn!("blocking-pool work was still running; abandoned");
            }
            BlockingOutcome::Skipped => {
                tracing::warn!("no budget left to drain the blocking pool");
            }
        }

        tracing::info!(
            trigger = self.trigger.as_str(),
            app = self.app.as_str(),
            drain = self.drain.as_str(),
            blocking = self.blocking.as_str(),
            abandoned = self.drain.abandoned(),
            elapsed_ms = u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
            clean = self.is_clean(),
            "shutdown complete"
        );
    }
}

/// The result of a whole process lifetime.
#[derive(Debug)]
pub struct Outcome {
    /// Whatever the application future returned. This is what `main` should propagate:
    /// the exit status belongs to the application, not to how tidily it stopped.
    pub result: anyhow::Result<()>,
    /// How the shutdown itself went. Already logged by [`run`]; returned so tests (and an
    /// application that wants to be stricter than the default) can assert on it.
    pub report: ShutdownReport,
}

/// The wall-clock ceiling on the shutdown sequence, as a deadline rather than a running
/// subtraction — an `Instant` cannot go negative, and nothing here truncates to whole
/// seconds on the way through.
#[derive(Clone, Copy, Debug)]
struct Budget {
    deadline: Instant,
}

impl Budget {
    fn starting_now(total: Duration) -> Self {
        Self {
            deadline: Instant::now() + total,
        }
    }

    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// The grace a phase actually gets: what it asked for, or what is left, whichever is
    /// smaller. Zero is a legitimate answer and every caller handles it.
    fn allow(&self, phase: Duration) -> Duration {
        phase.min(self.remaining())
    }
}

/// What [`block_on`](Runtime::block_on) hands back to the synchronous tail of [`run`].
struct Served {
    trigger: Trigger,
    result: anyhow::Result<()>,
    app: AppOutcome,
    drain: DrainOutcome,
    budget: Budget,
    started_at: Instant,
}

/// Run `app` on `runtime` until it finishes or the process is signalled, then shut down.
///
/// `app` is handed a [`Shutdown`]; anything it spawns through that handle is waited for.
/// The return value carries both the application's own result and a [`ShutdownReport`]
/// describing what, if anything, was still running at the end.
///
/// This consumes the runtime — phase 5 needs to, and leaving a half-shut-down runtime in
/// the caller's hands would be a trap.
pub fn run<App, Fut>(runtime: Runtime, config: LifecycleConfig, app: App) -> Outcome
where
    App: FnOnce(Shutdown) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let shutdown = Shutdown::new();
    let served = runtime.block_on(serve(shutdown.clone(), config, app));

    let Served {
        trigger,
        result,
        app,
        drain,
        budget,
        started_at,
    } = served;

    // Phase 5. Blocking-pool tasks are not tasks as far as the runtime's own accounting
    // is concerned: nothing above this line can see them, and `shutdown_background()`
    // would drop the pool without waiting at all. `shutdown_timeout` is the only API that
    // gives in-flight blocking work a chance to finish.
    let blocking_grace = budget.allow(config.blocking_grace);
    let blocking = if blocking_grace.is_zero() {
        BlockingOutcome::Skipped
    } else {
        let before = Instant::now();
        runtime.shutdown_timeout(blocking_grace);
        if before.elapsed() >= blocking_grace {
            BlockingOutcome::TimedOut
        } else {
            BlockingOutcome::Drained
        }
    };

    let report = ShutdownReport {
        trigger,
        app,
        drain,
        blocking,
        elapsed: started_at.elapsed(),
    };
    report.record();

    Outcome { result, report }
}

/// Phases 1 to 4, all of which need the runtime alive.
async fn serve<App, Fut>(shutdown: Shutdown, config: LifecycleConfig, app: App) -> Served
where
    App: FnOnce(Shutdown) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    // Installed before the application starts, so a signal arriving during a slow startup
    // is caught rather than killing the process outright.
    let mut signals = Signals::install();
    let mut app_fut = pin!(app(shutdown.clone()));

    // Phase 1: serve until the application finishes or someone asks us to stop. On a
    // signal the future is deliberately *not* dropped — it gets phase 4 to wind down.
    //
    // The token is watched too, so `Shutdown::cancel` from a background task is a
    // first-class way to stop the process even if the application future would not
    // otherwise notice.
    let (trigger, finished) = tokio::select! {
        result = &mut app_fut => match result {
            Ok(()) => (Trigger::Completed, Some(Ok(()))),
            Err(err) => (Trigger::Failed, Some(Err(err))),
        },
        source = signals.recv() => (source, None),
        () = shutdown.cancelled() => (Trigger::Requested, None),
    };

    // The budget starts here: everything before this was the application doing its job.
    let started_at = Instant::now();
    let budget = Budget::starting_now(config.total_budget());

    // Phase 2: only for an external trigger. An application that returned on its own has
    // nothing left to drain traffic from, and a one-shot job should not pay a delay it
    // exists to avoid.
    if trigger.is_external() {
        let delay = budget.allow(config.predrain_delay);
        if !delay.is_zero() {
            tracing::info!(
                delay_secs = delay.as_secs(),
                "shutdown requested; still serving briefly (signal again to skip)"
            );
            // The same `signals` as phase 1 — re-installing handlers here would drop the
            // originals and lose anything delivered in between.
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                _ = signals.recv() => tracing::info!("signalled again; not waiting"),
            }
        }
    }

    // Phase 3. Unconditional, and in particular it happens when the application returned
    // an error: that is precisely when background tasks are still running and nobody has
    // told them the process is going away.
    tracing::debug!(trigger = trigger.as_str(), "cancelling");
    shutdown.cancel();

    // Phase 4a: give the application future a bounded chance to return.
    let (result, app_outcome) = match finished {
        Some(result) => (result, AppOutcome::Finished),
        None => {
            let grace = budget.allow(config.app_grace);
            match tokio::time::timeout(grace, &mut app_fut).await {
                Ok(result) => (result, AppOutcome::Finished),
                Err(_) => (Ok(()), AppOutcome::Abandoned),
            }
        }
    };

    // Phase 4b: and then everything it spawned. The application may already have drained
    // (to sequence its own teardown behind its background tasks); this is then a no-op
    // that reports zero.
    let drain = shutdown.drain(budget.allow(config.drain_grace)).await;

    Served {
        trigger,
        result,
        app: app_outcome,
        drain,
        budget,
        started_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_never_hands_out_a_negative_grace() {
        // Partly's version computed each phase's grace by subtracting elapsed whole
        // seconds from a total, which underflows into a clamp and then reports the clamp
        // as a configuration error. A deadline cannot do that.
        let budget = Budget::starting_now(Duration::ZERO);
        assert_eq!(budget.allow(Duration::from_secs(30)), Duration::ZERO);
        assert_eq!(budget.remaining(), Duration::ZERO);
    }

    #[test]
    fn a_phase_gets_the_smaller_of_its_grace_and_what_is_left() {
        let budget = Budget::starting_now(Duration::from_secs(10));
        assert_eq!(budget.allow(Duration::from_secs(3)), Duration::from_secs(3));
        // Asking for more than the whole budget yields the remainder, not the ask.
        assert!(budget.allow(Duration::from_secs(60)) <= Duration::from_secs(10));
    }

    #[test]
    fn sub_second_graces_survive_the_budget() {
        // The truncation bug in the crate this replaces: `elapsed().as_secs()` threw away
        // up to a second per phase, three phases deep.
        let budget = Budget::starting_now(Duration::from_millis(1500));
        let allowed = budget.allow(Duration::from_millis(250));
        assert_eq!(allowed, Duration::from_millis(250));
    }

    #[test]
    fn only_an_external_trigger_delays() {
        assert!(Trigger::Interrupt.is_external());
        assert!(Trigger::Terminate.is_external());
        assert!(!Trigger::Completed.is_external());
        assert!(!Trigger::Failed.is_external());
    }

    #[test]
    fn a_report_is_clean_only_when_every_phase_finished() {
        let clean = ShutdownReport {
            trigger: Trigger::Terminate,
            app: AppOutcome::Finished,
            drain: DrainOutcome::Drained { tasks: 2 },
            blocking: BlockingOutcome::Drained,
            elapsed: Duration::from_millis(5),
        };
        assert!(clean.is_clean());

        assert!(
            !ShutdownReport {
                drain: DrainOutcome::TimedOut {
                    abandoned: 1,
                    sites: vec!["src/lib.rs:1:1".to_string()],
                },
                ..clean.clone()
            }
            .is_clean()
        );
        assert!(
            !ShutdownReport {
                app: AppOutcome::Abandoned,
                ..clean.clone()
            }
            .is_clean()
        );
        // "We never looked" is not the same as "there was nothing there".
        assert!(
            !ShutdownReport {
                blocking: BlockingOutcome::Skipped,
                ..clean
            }
            .is_clean()
        );
    }

    #[test]
    fn abandoned_counts_only_what_was_left_running() {
        assert_eq!(DrainOutcome::Drained { tasks: 7 }.abandoned(), 0);
        assert_eq!(
            DrainOutcome::TimedOut {
                abandoned: 3,
                sites: Vec::new(),
            }
            .abandoned(),
            3
        );
    }
}
