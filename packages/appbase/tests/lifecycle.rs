//! The shutdown sequence, end to end, over a real runtime.
//!
//! Everything here drives [`sure_appbase::run`] itself rather than its parts, because the
//! bugs this crate exists to avoid are all ordering bugs — a phase skipped on one exit
//! path, a budget that underflows, a task nobody waited for. Testing the phases in
//! isolation would miss every one of them.
//!
//! Signals live in their own test binaries (`signal_*.rs`): delivering one hits the whole
//! process, so a test that raises `SIGTERM` cannot share an executable with tests running
//! in parallel beside it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sure_appbase::{run, AppOutcome, DrainOutcome, LifecycleConfig, Outcome, Shutdown, Trigger};

/// A runtime per test — `run` consumes the one it is given.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime")
}

/// Short graces so a test that deliberately hangs still finishes quickly. The ratios
/// match the real defaults: the application gets longer than the drain, the drain longer
/// than the blocking backstop.
fn quick() -> LifecycleConfig {
    LifecycleConfig {
        predrain_delay: Duration::ZERO,
        app_grace: Duration::from_millis(400),
        drain_grace: Duration::from_millis(200),
        blocking_grace: Duration::from_millis(200),
    }
}

#[test]
fn a_clean_return_waits_for_the_tasks_the_application_spawned() {
    let finished = Arc::new(AtomicUsize::new(0));

    let outcome = run(runtime(), quick(), {
        let finished = finished.clone();
        |shutdown: Shutdown| async move {
            for _ in 0..4 {
                let finished = finished.clone();
                shutdown.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    finished.fetch_add(1, Ordering::SeqCst);
                });
            }
            // Returns immediately, while all four are still running: the drain, not the
            // application, is what has to wait for them.
            Ok(())
        }
    });

    assert!(outcome.result.is_ok());
    assert_eq!(outcome.report.trigger, Trigger::Completed);
    assert_eq!(outcome.report.app, AppOutcome::Finished);
    assert_eq!(outcome.report.drain, DrainOutcome::Drained { tasks: 4 });
    assert!(outcome.report.is_clean());
    assert_eq!(
        finished.load(Ordering::SeqCst),
        4,
        "every spawned task should have run to completion"
    );
}

#[test]
fn an_application_error_still_cancels_and_drains() {
    // The regression this crate was written for. The shape it replaces bailed out of the
    // async block on `Err`, which skipped cancellation *and* the drain — so on the one
    // path where background tasks are certainly still running, nothing told them to stop.
    let cancelled = Arc::new(AtomicBool::new(false));

    let outcome = run(runtime(), quick(), {
        let cancelled = cancelled.clone();
        |shutdown: Shutdown| async move {
            let watcher = {
                let cancelled = cancelled.clone();
                let token = shutdown.child_token();
                shutdown.spawn(async move {
                    token.cancelled().await;
                    cancelled.store(true, Ordering::SeqCst);
                })
            };
            // Give the watcher a moment to actually park on the token, so a pass here
            // means cancellation was observed rather than merely raced past.
            tokio::time::sleep(Duration::from_millis(20)).await;
            drop(watcher);
            Err(anyhow::anyhow!("startup failed"))
        }
    });

    let err = outcome
        .result
        .expect_err("the application's error must survive");
    assert_eq!(err.to_string(), "startup failed");
    assert_eq!(outcome.report.trigger, Trigger::Failed);
    assert!(
        cancelled.load(Ordering::SeqCst),
        "a failing application must still cancel its background tasks"
    );
    // Whether the watcher had already finished by the time the drain sampled is a race,
    // so assert what is not: nothing was left behind.
    assert_eq!(outcome.report.drain.abandoned(), 0);
    assert!(outcome.report.is_clean());
}

#[test]
fn a_task_that_ignores_cancellation_is_named_not_just_counted() {
    let outcome = run(runtime(), quick(), |shutdown: Shutdown| async move {
        // Never looks at the token. This is the line the report should point at.
        shutdown.spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
        Ok(())
    });

    assert!(outcome.result.is_ok());
    assert!(
        !outcome.report.is_clean(),
        "abandoning a task is not a clean shutdown"
    );
    assert_eq!(outcome.report.drain.abandoned(), 1);

    // Debug-only bookkeeping, and `cargo test` is a debug build — so this is the build
    // where the diagnostic has to work. A bare count would leave an operator grepping.
    let sites = outcome.report.drain.sites();
    assert_eq!(sites.len(), 1, "expected one spawn site, got {sites:?}");
    assert!(
        sites[0].contains("tests/lifecycle.rs"),
        "the site should name this file, got {:?}",
        sites[0]
    );
}

#[test]
fn every_abandoned_task_is_reported_against_its_own_spawn_site() {
    let outcome = run(runtime(), quick(), |shutdown: Shutdown| async move {
        let stuck = || async { tokio::time::sleep(Duration::from_secs(60)).await };
        shutdown.spawn(stuck());
        shutdown.spawn(stuck());
        // A task that does stop, to prove the table is not simply "everything ever
        // spawned" — its guard has to have removed it by the time the drain gives up.
        shutdown.spawn(async {});
        Ok(())
    });

    assert_eq!(outcome.report.drain.abandoned(), 2);
    let sites = outcome.report.drain.sites();
    assert_eq!(sites.len(), 2, "expected two spawn sites, got {sites:?}");
    // Two distinct lines, because `#[track_caller]` resolves to the caller of `spawn` and
    // not to a shared helper inside the crate.
    assert_ne!(sites[0], sites[1], "each site should be its own line");
}

#[test]
fn a_clean_return_pays_no_predrain_delay() {
    // The delay exists so a load balancer can stop routing to a process that has been
    // told to stop. An application that simply ran out of work has nothing to drain
    // traffic away from, and a one-shot job should not pay for the privilege.
    let config = LifecycleConfig {
        predrain_delay: Duration::from_secs(30),
        ..quick()
    };

    let started = Instant::now();
    let outcome = run(runtime(), config, |_shutdown: Shutdown| async { Ok(()) });
    let elapsed = started.elapsed();

    assert!(outcome.result.is_ok());
    assert_eq!(outcome.report.trigger, Trigger::Completed);
    assert!(
        elapsed < Duration::from_secs(5),
        "a clean return waited {elapsed:?} on a delay meant for signals"
    );
}

#[test]
fn tracked_blocking_work_is_waited_for() {
    // Blocking-pool work is not a task as far as the runtime's own accounting goes, so
    // anything that counts tasks cannot see it and will happily report a clean shutdown
    // over the top of a half-finished write. Tracking is what makes it visible.
    let done = Arc::new(AtomicBool::new(false));

    let config = LifecycleConfig {
        drain_grace: Duration::from_secs(5),
        ..quick()
    };

    let outcome = run(runtime(), config, {
        let done = done.clone();
        |shutdown: Shutdown| async move {
            shutdown.spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(150));
                done.store(true, Ordering::SeqCst);
            });
            Ok(())
        }
    });

    assert!(outcome.result.is_ok());
    assert!(
        done.load(Ordering::SeqCst),
        "blocking work was abandoned mid-flight"
    );
    assert_eq!(outcome.report.drain, DrainOutcome::Drained { tasks: 1 });
    assert!(outcome.report.is_clean());
}

#[test]
fn an_application_can_drain_first_to_sequence_its_own_teardown() {
    // `sure-server` closes its database pool only after its background tasks have
    // stopped, so it drains itself and lets `run`'s drain be the no-op. Both calls have
    // to work, and the order between them has to hold.
    let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));

    let outcome = run(runtime(), quick(), {
        let order = order.clone();
        |shutdown: Shutdown| async move {
            shutdown.spawn({
                let order = order.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    order.lock().expect("lock").push("task finished");
                }
            });

            let drain = shutdown.drain(Duration::from_secs(5)).await;
            order.lock().expect("lock").push("teardown");
            assert_eq!(drain, DrainOutcome::Drained { tasks: 1 });
            Ok(())
        }
    });

    assert!(outcome.result.is_ok());
    assert_eq!(
        *order.lock().expect("lock"),
        vec!["task finished", "teardown"],
        "teardown must not overtake the tasks it is tearing down"
    );
    // The second drain, inside `run`, sees an already-closed and empty tracker.
    assert_eq!(outcome.report.drain, DrainOutcome::Drained { tasks: 0 });
    assert!(outcome.report.is_clean());
}

#[test]
fn an_application_that_will_not_return_is_dropped_and_reported() {
    let outcome = run(runtime(), quick(), |shutdown: Shutdown| async move {
        // A background task decides the process should stop; the main future never
        // notices, which is exactly the case `app_grace` bounds.
        shutdown.spawn({
            let shutdown = shutdown.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                shutdown.cancel();
            }
        });
        std::future::pending::<()>().await;
        Ok(())
    });

    assert_eq!(outcome.report.trigger, Trigger::Requested);
    assert_eq!(outcome.report.app, AppOutcome::Abandoned);
    assert!(!outcome.report.is_clean());
    // The exit status belongs to the application. It never failed — it just never
    // finished tidying up, and that is what the report is for.
    assert!(outcome.result.is_ok());
}

#[test]
fn an_exhausted_budget_still_runs_every_phase_and_returns_promptly() {
    // With every grace at zero there is no time for anything, which the crate this
    // replaces reported as `Timeout must be at least 1 second` — a configuration-shaped
    // error for an ordinary out-of-budget condition, raised before it had so much as
    // looked at what was running. Here it is just a series of zero-length waits.
    let config = LifecycleConfig {
        predrain_delay: Duration::ZERO,
        app_grace: Duration::ZERO,
        drain_grace: Duration::ZERO,
        blocking_grace: Duration::ZERO,
    };

    let started = Instant::now();
    let outcome: Outcome = run(runtime(), config, |shutdown: Shutdown| async move {
        shutdown.spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
        Ok(())
    });
    let elapsed = started.elapsed();

    assert!(outcome.result.is_ok());
    assert_eq!(outcome.report.trigger, Trigger::Completed);
    assert_eq!(outcome.report.drain.abandoned(), 1);
    assert!(!outcome.report.is_clean());
    assert!(
        elapsed < Duration::from_secs(2),
        "a zero budget should not wait; took {elapsed:?}"
    );
}

#[test]
fn an_overrunning_phase_cannot_leave_a_later_one_with_a_negative_budget() {
    // The whole sequence is capped at the sum of the phases, so an application that eats
    // its entire grace leaves the drain with what remains — which may be nothing, but is
    // never a wrapped-around enormous number or a clamp reported as an error.
    let config = LifecycleConfig {
        predrain_delay: Duration::ZERO,
        app_grace: Duration::from_millis(150),
        drain_grace: Duration::from_millis(150),
        blocking_grace: Duration::from_millis(50),
    };
    let budget = config.total_budget();

    let started = Instant::now();
    let outcome = run(runtime(), config, |shutdown: Shutdown| async move {
        shutdown.spawn(async { tokio::time::sleep(Duration::from_secs(60)).await });
        shutdown.cancel();
        // Outlives `app_grace`, so phase 4a times out and phase 4b starts late.
        std::future::pending::<()>().await;
        Ok(())
    });
    let elapsed = started.elapsed();

    assert_eq!(outcome.report.app, AppOutcome::Abandoned);
    assert_eq!(outcome.report.drain.abandoned(), 1);
    assert!(
        elapsed < budget + Duration::from_secs(1),
        "shutdown took {elapsed:?}, over its {budget:?} budget"
    );
}
