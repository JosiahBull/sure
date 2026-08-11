//! `SIGTERM` — the one a container runtime or `systemd` sends first, and the path that
//! matters most.
//!
//! Its own test binary on purpose. A signal is delivered to the *process*, so a test that
//! raises one cannot share an executable with tests running in parallel beside it: they
//! would all see it. One test per file is the price of testing the real path instead of a
//! stand-in for it.

#![cfg(unix)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sure_appbase::{AppOutcome, LifecycleConfig, Shutdown, Trigger, run};

#[test]
fn sigterm_cancels_the_application_and_drains_what_it_spawned() {
    let served = Arc::new(AtomicBool::new(false));
    let task_stopped = Arc::new(AtomicBool::new(false));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime");

    let config = LifecycleConfig {
        predrain_delay: Duration::ZERO,
        app_grace: Duration::from_secs(5),
        drain_grace: Duration::from_secs(5),
        blocking_grace: Duration::from_millis(200),
    };

    let outcome = run(runtime, config, {
        let served = served.clone();
        let task_stopped = task_stopped.clone();
        |shutdown: Shutdown| async move {
            // A background task in the shape every real one has: work until told to stop,
            // then tidy up.
            shutdown.spawn({
                let token = shutdown.child_token();
                let task_stopped = task_stopped.clone();
                async move {
                    token.cancelled().await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    task_stopped.store(true, Ordering::SeqCst);
                }
            });

            served.store(true, Ordering::SeqCst);

            // Signal ourselves once the runtime is up and the handler is installed —
            // `Signals::install` runs before the application future is first polled, so
            // by here it is registered.
            //
            // SAFETY: `raise` is async-signal-safe and the target is this process, which
            // has a tokio handler installed for SIGTERM. Nothing else in this binary runs
            // concurrently — that is why this test has an executable to itself.
            unsafe {
                libc::raise(libc::SIGTERM);
            }

            shutdown.cancelled().await;
            Ok(())
        }
    });

    assert!(served.load(Ordering::SeqCst), "the application never ran");
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(
        outcome.report.trigger,
        Trigger::Terminate,
        "SIGTERM must be distinguishable from SIGINT in the report"
    );
    assert_eq!(outcome.report.app, AppOutcome::Finished);
    assert!(
        task_stopped.load(Ordering::SeqCst),
        "the background task was cut off rather than drained"
    );
    assert_eq!(outcome.report.drain.abandoned(), 0);
    assert!(
        outcome.report.is_clean(),
        "SIGTERM should be a clean shutdown: {:?}",
        outcome.report
    );
}
