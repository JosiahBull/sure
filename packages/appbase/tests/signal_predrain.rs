//! The pre-drain delay, and the second signal that skips it.
//!
//! This is the case that fails when signal handlers are re-registered per wait rather
//! than installed once: the first `Signal` stream is dropped when the initial wait
//! completes, the second is created a moment later, and anything delivered in between has
//! no receiver. The window is small and the symptom is "Ctrl-C twice did nothing", which
//! is easy to blame on the terminal.
//!
//! Own test binary — see `signal_terminate.rs`.

#![cfg(unix)]

use std::time::{Duration, Instant};

use sure_appbase::{run, LifecycleConfig, Shutdown, Trigger};

#[test]
fn a_second_signal_skips_the_predrain_delay() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build a runtime");

    // Long enough that sitting through it would be unmistakable in the elapsed time.
    let config = LifecycleConfig {
        predrain_delay: Duration::from_secs(30),
        app_grace: Duration::from_secs(5),
        drain_grace: Duration::from_secs(5),
        blocking_grace: Duration::from_millis(200),
    };

    let started = Instant::now();
    let outcome = run(runtime, config, |shutdown: Shutdown| async move {
        // Untracked on purpose: this stands in for the operator at the keyboard, not for
        // application work, and it must outlive the phase-1 select to send the second
        // signal during the delay.
        tokio::spawn(async {
            // SAFETY: async-signal-safe, targets this process, and this binary runs one
            // test. See `signal_terminate.rs`.
            unsafe {
                libc::raise(libc::SIGINT);
            }
            // Comfortably inside the 30s delay, and long enough after the first that the
            // sequence has certainly entered it.
            tokio::time::sleep(Duration::from_millis(200)).await;
            unsafe {
                libc::raise(libc::SIGINT);
            }
        });

        shutdown.cancelled().await;
        Ok(())
    });
    let elapsed = started.elapsed();

    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.report.trigger, Trigger::Interrupt);
    assert!(
        elapsed < Duration::from_secs(10),
        "the second signal was not seen: waited {elapsed:?} of a 30s delay"
    );
    // ...but the first signal did start the delay, rather than the whole thing being
    // skipped for some unrelated reason.
    assert!(
        elapsed >= Duration::from_millis(150),
        "shutdown finished in {elapsed:?}, before the second signal could have been sent"
    );
    assert!(outcome.report.is_clean(), "{:?}", outcome.report);
}
