//! The process-wide bound on how much CPU-bound work runs at once, and the one way a
//! handler is allowed to run some.
//!
//! Two guards that look similar solve different problems, and this is the third of them.
//! [`crate::limits::RateLimiter`] bounds how *often* one client may ask;
//! [`crate::limits::InFlight`] bounds how many requests are in flight at all, whoever asked.
//! Neither bounds *cores*: a forecast or a whole-ledger report is tens of milliseconds to
//! seconds of uninterrupted arithmetic, and the in-flight ceiling is dozens of slots wide
//! precisely because most requests are a query and a serialise.
//!
//! Moving that arithmetic to [`sure_appbase::Shutdown::spawn_blocking`] is what stops it from
//! stalling the async workers — on a four-worker box, four concurrent `GET /api/forecast`s
//! with the loop inline mean no connections accepted, `/api/health` silent, no scheduler tick
//! and no shutdown watcher, with no external failure required. But `spawn_blocking` on its own
//! only relocates the problem: tokio's blocking pool defaults to **512** threads, so it would
//! trade "four requests stall the runtime" for "512 Monte Carlo runs fighting over four cores",
//! where every one of them misses its deadline instead of a few of them being refused quickly.
//!
//! So the pool is not the bound — this semaphore is, sized to the machine's actual parallelism.
//! It sheds rather than queues, for the same reason `snapshot`'s `EXPORT_SLOT` and
//! [`crate::limits::shed_when_saturated`] do: a fast 503 with `Retry-After` lets a client come
//! back, whereas queueing turns a burst into a pile of requests that each still cost a full run
//! when their turn finally comes, long after the caller has given up. The refusal is
//! [`crate::limits::overloaded_response`] — byte-identical to the in-flight shedder's and the
//! exhausted-pool one, so "busy, come back" has exactly one shape on the wire.

use std::sync::LazyLock;
use std::thread::available_parallelism;

use axum::response::Response;
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::error::AppError;
use crate::limits::overloaded_response;

/// How many CPU-bound handler runs may be in progress at once.
///
/// One less than the machine's parallelism, floored at one: the runtime still has to accept
/// connections, answer `/api/health`, tick the scheduler and watch for shutdown while the
/// compute is going, and taking every core leaves nothing to do that on — which is the exact
/// symptom this module exists to prevent, merely relocated to the blocking pool. On a
/// single-core box the floor wins and one run at a time is the honest answer.
fn slots() -> usize {
    available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_sub(1)
        .max(1)
}

/// Sized once, on first use. `LazyLock` rather than `Semaphore::const_new` because the size
/// comes from [`available_parallelism`], which is not a constant.
static COMPUTE_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(slots()));

/// A slot to run CPU-bound work in, or `None` if every core is already committed.
///
/// `try_acquire`, never `acquire`: see the module docs on shedding instead of queueing. Hold
/// the permit for the whole run — dropping it early would let the next request in while this
/// one is still on a core, which is the same over-subscription with an extra step.
#[must_use = "dropping the permit immediately releases the slot and defeats the bound"]
pub fn try_slot() -> Option<SemaphorePermit<'static>> {
    // `.ok()` rather than a `match` on `TryAcquireError`: the two arms (no permits, closed)
    // mean the same thing here — not now, come back — and nothing closes this semaphore.
    COMPUTE_SLOTS.try_acquire().ok()
}

/// One or more slots, and permission to use exactly that many threads.
///
/// Held for the whole run, like [`try_slot`]'s permit. The two permits are separate because
/// [`Semaphore::try_acquire_many`] is all-or-nothing and the second half is optional: the
/// mandatory first slot is what decides whether the request runs at all, and the rest is how
/// wide it may go.
#[must_use = "dropping the permits releases the slots and defeats the bound"]
pub struct Slots {
    _first: SemaphorePermit<'static>,
    _rest: Option<SemaphorePermit<'static>>,
    width: usize,
}

impl Slots {
    /// How many threads the holder may run at once. At least 1.
    pub fn width(&self) -> usize {
        self.width
    }
}

/// One mandatory slot, plus as many spare ones as can be taken without emptying the pool.
///
/// The counterpart to `sure_app::forecast::Parallelism`: the forecast can now run its paths
/// across threads, and something has to decide how many without over-subscribing the machine.
/// That decision belongs here, because this semaphore is the only thing that knows what else is
/// running — the same question [`slots`] already answers for concurrent *requests*, asked per
/// request instead of per core.
///
/// Every thread a run spawns is backed by a permit it actually holds, which is the invariant
/// that matters: **this pool bounds cores, not requests.** So a wide run does admit fewer
/// concurrent ones — five 1-thread runs where there used to be nine — and that is the honest
/// answer rather than a regression. Nine threads is nine threads whether one request is using
/// them or nine are; a sixth request arriving to a committed machine gets the same fast 503 with
/// `Retry-After` it would have got by being the tenth.
///
/// # Why it does not simply take everything
///
/// The first version was greedy, and greedy is wrong here. A forecast that grabs all nine slots
/// on this box sheds the three or four `/api/reports/*` calls the *same dashboard load* fires
/// beside it — each of which is tens of milliseconds and would have been served — in exchange
/// for finishing the forecast a bit sooner. Refusing cheap work to hurry expensive work is the
/// wrong trade, and it is not a trade anything asked for.
///
/// So half the pool is left alone. On an idle nine-slot box the first run takes five and leaves
/// four, which is four more requests admitted while it runs. The read of
/// [`Semaphore::available_permits`] is a racy snapshot on purpose — it only sizes the *ask*, and
/// losing the race costs a narrower run, never an over-subscribed one.
pub fn try_slots() -> Option<Slots> {
    let first = COMPUTE_SLOTS.try_acquire().ok()?;
    let keep_free = slots() / 2;
    let want = COMPUTE_SLOTS
        .available_permits()
        .saturating_sub(keep_free)
        .min(u32::MAX as usize) as u32;
    // `try_acquire_many` is all-or-nothing; a lost race just leaves this run single-threaded.
    let rest = (want > 0)
        .then(|| COMPUTE_SLOTS.try_acquire_many(want).ok())
        .flatten();
    let width = 1 + rest.as_ref().map_or(0, |_| want as usize);
    Some(Slots {
        _first: first,
        _rest: rest,
        width,
    })
}

/// The standard "busy, come back" refusal, with a log line naming which endpoint was shed.
///
/// `endpoint` is a static route name, not user input — it is only ever a literal at the call
/// site, so it cannot smuggle anything into the log.
pub fn shed(endpoint: &'static str) -> Response {
    tracing::warn!(endpoint, "shedding request: compute slots all busy");
    overloaded_response()
}

/// Turn a [`tokio::task::JoinError`] from a compute task into a 500.
///
/// This is the one behavioural difference from running the work inline. A panic inside the
/// arithmetic used to unwind through the handler and be caught by [`CatchPanicLayer`]; on the
/// blocking pool it is caught by the runtime and surfaces here as a `JoinError` instead. Mapped
/// to [`AppError::Internal`], it produces the same scrubbed 500 that layer would have — so the
/// client sees no difference — and it must never be `unwrap`ped, which would re-panic on the
/// worker that was awaiting the join and take a connection down with it.
///
/// The other `JoinError` case, cancellation, cannot happen: nothing aborts these handles, and
/// the tracked task is waited for at drain rather than cancelled.
///
/// [`CatchPanicLayer`]: tower_http::catch_panic::CatchPanicLayer
pub fn joined(err: tokio::task::JoinError, endpoint: &'static str) -> AppError {
    tracing::error!(endpoint, error = %err, "compute task did not complete");
    AppError::Internal(anyhow::anyhow!("{endpoint} compute task failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::http::StatusCode;

    /// [`COMPUTE_SLOTS`] is one semaphore for the process, which is the whole point of it — so
    /// two tests that both want an *idle* pool cannot run at the same time, and cargo runs the
    /// tests in one binary on many threads. Every test below takes this first. Without it the
    /// two are order-dependent and fail each other: the pool test asserted "started from an idle
    /// pool" and found one slot, because the greedy test was holding the other eight.
    static POOL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// At least one slot on any machine, and never so many that the runtime is left without a
    /// core — the two ends the sizing has to get right.
    #[test]
    fn the_pool_is_sized_to_the_machine_leaving_the_runtime_a_core() {
        let cores = available_parallelism().map(|n| n.get()).unwrap_or(1);
        assert!(slots() >= 1);
        assert!(slots() <= cores.max(1));
        if cores > 1 {
            assert_eq!(slots(), cores - 1, "one core is reserved for the runtime");
        }
    }

    /// Full means refused, not queued — and refused with the *same* 503 envelope the
    /// in-flight shedder uses, so a client has one contract for "busy" rather than one per
    /// guard it happened to trip.
    #[tokio::test]
    async fn a_full_pool_sheds_and_a_free_one_admits() {
        let _serialised = POOL.lock().unwrap();
        // Drain every slot by holding all of them, whatever the machine's size is.
        let held: Vec<_> = std::iter::from_fn(try_slot).collect();
        assert_eq!(held.len(), slots(), "started from an idle pool");

        assert!(
            try_slot().is_none(),
            "a full pool must refuse rather than queue"
        );
        assert_eq!(shed("test").status(), StatusCode::SERVICE_UNAVAILABLE);

        drop(held);
        assert!(
            try_slot().is_some(),
            "a slot must become available again once the run finishes"
        );
    }

    /// A wide run must leave the pool able to serve the cheap work beside it.
    ///
    /// Three properties. The first is what the greedy first attempt got wrong: after a run has
    /// taken its width, at least half the pool is still free, so the `/api/reports/*` calls the
    /// same dashboard load fires are served rather than shed. The second is the invariant the
    /// whole module exists for — however the slots end up divided, the threads promised never
    /// exceed the permits held. The third is that it still refuses rather than queues.
    #[tokio::test]
    async fn a_wide_run_still_leaves_the_pool_able_to_admit_work() {
        let _serialised = POOL.lock().unwrap();
        let total = slots();

        let first = try_slots().expect("an idle pool admits the first run");
        assert!(first.width() >= 1);
        if total >= 4 {
            assert!(
                first.width() > 1,
                "a {total}-slot pool should give the first run real width, got {}",
                first.width()
            );
            assert!(
                first.width() <= total - total / 2,
                "took {} of {total}, leaving less than half free",
                first.width()
            );
            // The point of the headroom: something else can still get in.
            let beside = try_slots().expect("a cheap call beside a wide run must be admitted");
            assert_eq!(beside.width(), 1, "the headroom is not for widening");
            drop(beside);
        }

        // Threads promised never exceed permits held, whatever the split. Drain the pool to be
        // sure the sum is over every holder there can be.
        let mut held = vec![first];
        while let Some(more) = try_slots() {
            held.push(more);
        }
        let promised: usize = held.iter().map(Slots::width).sum();
        assert_eq!(
            promised, total,
            "the pool hands out its permits exactly once, as threads"
        );
    }

    /// A panic in the compute half is a 500, not a hung request and not an abort of the
    /// process: `spawn_blocking` hands it back as a `JoinError`, and mapping it is the only
    /// thing standing between that and an `unwrap` re-panicking on the awaiting worker.
    #[tokio::test]
    async fn a_panic_in_the_compute_half_becomes_a_500() {
        use axum::response::IntoResponse;

        let shutdown = sure_appbase::Shutdown::new();
        let handle = shutdown.spawn_blocking(|| -> crate::error::AppResult<i64> {
            panic!("arithmetic exploded");
        });

        let err = match handle.await {
            Ok(_) => panic!("the closure panicked, so the join cannot succeed"),
            Err(join) => joined(join, "test"),
        };

        assert_eq!(
            err.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
