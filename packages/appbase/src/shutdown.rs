//! The handle the application is given: "stop what you are doing" on one side, "here is
//! everything I started" on the other.

use std::future::Future;
use std::panic::Location;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, WaitForCancellationFuture};
use tokio_util::task::TaskTracker;

use crate::callsite::CallSites;

/// Ask-to-stop and wait-for-stopped, in one cheap-to-clone handle.
///
/// The two halves are deliberately paired. A [`CancellationToken`] on its own tells tasks
/// to stop but gives no way to know when they have; a [`TaskTracker`] on its own knows
/// what is running but has no way to ask it to finish. Together they answer the only
/// question that matters at shutdown — *is anything still running?* — exactly, rather
/// than by sampling [`RuntimeMetrics::num_alive_tasks`][alive] and hoping.
///
/// The catch, and it is the whole contract: **only tasks spawned through [`spawn`] and
/// [`spawn_blocking`] are tracked.** A bare `tokio::spawn` is invisible here and will be
/// abandoned at exit. That is the trade for exactness — the alternative, counting every
/// task on the runtime, cannot tell an application's own work apart from a connection
/// pool's idle reaper, and ends up encoding a magic number.
///
/// [`spawn`]: Shutdown::spawn
/// [`spawn_blocking`]: Shutdown::spawn_blocking
/// [alive]: tokio::runtime::RuntimeMetrics::num_alive_tasks
#[derive(Clone, Debug, Default)]
pub struct Shutdown {
    token: CancellationToken,
    tracker: TaskTracker,
    /// Debug builds only — see [`crate::callsite`]. Lets a timed-out drain name the
    /// lines that spawned whatever is still running instead of just counting them.
    call_sites: CallSites,
}

impl Shutdown {
    /// A fresh, uncancelled handle with nothing tracked.
    ///
    /// [`run`](crate::run) makes one for you. Construct one directly to exercise an
    /// application's own startup/teardown in a test without going through the runtime
    /// lifecycle.
    pub fn new() -> Self {
        Self::default()
    }

    /// The underlying token, for handing to code that takes one (a `tower` layer, say).
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// A token that is cancelled when this one is, but can also be cancelled on its own.
    ///
    /// Use it for a subsystem that may need to be stopped early without taking the rest
    /// of the process down with it.
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// Resolves once shutdown has been requested. Cancel-safe, so it can sit in a
    /// `select!` arm that loses the race and be polled again next time round.
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.token.cancelled()
    }

    /// Whether shutdown has been requested. For loops that would rather check between
    /// units of work than be dropped part-way through one.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Request shutdown.
    ///
    /// [`run`](crate::run) calls this on every exit path. An application may also call it
    /// to bring the process down on its own terms — a fatal error on a background task,
    /// say, that the main future has no way to observe.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Spawn a tracked task. Same signature and semantics as [`tokio::spawn`], except
    /// that [`drain`](Self::drain) will wait for it.
    ///
    /// The task is still responsible for noticing cancellation — tracking observes, it
    /// does not interrupt. A task that ignores [`cancelled`](Self::cancelled) is exactly
    /// what turns a clean drain into an abandoned one.
    ///
    /// `#[track_caller]`, so a debug build records *this* line and can name it if the
    /// task is still running at the deadline. Keep that in mind when wrapping this in a
    /// helper of your own: without `#[track_caller]` on the wrapper too, every task it
    /// spawns is reported at the wrapper instead of at the interesting call site.
    #[track_caller]
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let site = self.call_sites.record(Location::caller());
        self.tracker.spawn(async move {
            // Moved in, so it is released whether the future completes or is dropped.
            let _site = site;
            future.await
        })
    }

    /// Spawn a tracked task on the blocking pool.
    ///
    /// Worth preferring over a bare [`tokio::task::spawn_blocking`] for anything that
    /// touches durable state: blocking-pool work is not a task in the runtime's eyes, so
    /// nothing else at shutdown can see it. Tracked, it is drained with everything else.
    #[track_caller]
    pub fn spawn_blocking<F, T>(&self, f: F) -> JoinHandle<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let site = self.call_sites.record(Location::caller());
        self.tracker.spawn_blocking(move || {
            let _site = site;
            f()
        })
    }

    /// How many tracked tasks are still running.
    pub fn tracked(&self) -> usize {
        self.tracker.len()
    }

    /// Stop accepting new tracked tasks, then wait up to `grace` for the ones already
    /// running to finish.
    ///
    /// Idempotent: [`run`](crate::run) calls this after the application future returns,
    /// so an application that needs to sequence its own teardown *after* its background
    /// tasks — closing a connection pool, say — can call it first and let the second call
    /// return immediately.
    ///
    /// Never call this from inside a tracked task: it would be waiting for itself.
    pub async fn drain(&self, grace: Duration) -> DrainOutcome {
        self.tracker.close();
        // Sampled before the wait so the report can say how much there was to do, not
        // just that there is none left.
        let tasks = self.tracker.len();
        match tokio::time::timeout(grace, self.tracker.wait()).await {
            Ok(()) => DrainOutcome::Drained { tasks },
            // Sampled here, while the tasks are still alive: once the runtime drops them
            // their guards run and there is nothing left to name.
            Err(_) => DrainOutcome::TimedOut {
                abandoned: self.tracker.len(),
                sites: self.call_sites.outstanding(),
            },
        }
    }
}

/// What became of the tracked tasks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DrainOutcome {
    /// Everything finished. `tasks` is how many were running when the drain began — zero
    /// is the normal case for a process whose work is all request-scoped.
    Drained { tasks: usize },
    /// The grace period ran out. `abandoned` tasks were still running and were dropped
    /// when the runtime went away.
    TimedOut {
        abandoned: usize,
        /// `file:line:col` for each one, from [`Shutdown::spawn`]'s `#[track_caller]`.
        /// **Empty in a release build** — the bookkeeping is debug-only, so trust
        /// `abandoned` for the count and treat this as the debugging aid it is.
        sites: Vec<String>,
    },
}

impl DrainOutcome {
    /// How many tasks were left running. The number an operator actually wants.
    pub fn abandoned(&self) -> usize {
        match self {
            DrainOutcome::Drained { .. } => 0,
            DrainOutcome::TimedOut { abandoned, .. } => *abandoned,
        }
    }

    /// Where the tasks that were left running had been spawned. Empty when the drain was
    /// clean, and always empty in a release build.
    pub fn sites(&self) -> &[String] {
        match self {
            DrainOutcome::Drained { .. } => &[],
            DrainOutcome::TimedOut { sites, .. } => sites,
        }
    }

    /// The one legal rendering of this value as text, for a log field.
    pub fn as_str(&self) -> &'static str {
        match self {
            DrainOutcome::Drained { .. } => "drained",
            DrainOutcome::TimedOut { .. } => "timed_out",
        }
    }
}
