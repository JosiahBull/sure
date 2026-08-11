//! Signal handling, installed exactly once.
//!
//! The reason this is a struct and not a `wait_for_signal()` free function: the shutdown
//! sequence waits for a signal *twice* — once to start shutting down, and again during
//! the pre-drain delay so a second Ctrl-C can skip it. Building fresh
//! [`tokio::signal::unix::Signal`] streams for the second wait drops the first pair and
//! re-registers, and a signal that lands in the gap between those two moments has no
//! receiver and is silently lost. Registering once and reusing the streams closes that
//! window.

use crate::Trigger;

/// The installed signal handlers, held for the life of the shutdown sequence.
#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct Signals {
    interrupt: Option<tokio::signal::unix::Signal>,
    terminate: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl Signals {
    /// Register handlers for `SIGINT` and `SIGTERM`.
    ///
    /// A handler that will not install is logged and dropped rather than being fatal: a
    /// process that refuses to start because it cannot catch `SIGINT` is worse than one
    /// that starts and can only be stopped the hard way. If neither installs,
    /// [`recv`](Self::recv) simply never resolves — which is the honest representation of
    /// "this process cannot be asked to stop politely".
    pub(crate) fn install() -> Self {
        use tokio::signal::unix::{SignalKind, signal};

        let open = |kind: SignalKind, name: &'static str| match signal(kind) {
            Ok(stream) => Some(stream),
            Err(err) => {
                tracing::warn!(signal = name, error = %err, "could not install a signal handler");
                None
            }
        };

        Self {
            interrupt: open(SignalKind::interrupt(), "SIGINT"),
            terminate: open(SignalKind::terminate(), "SIGTERM"),
        }
    }

    /// Wait for the next `SIGINT` or `SIGTERM`.
    ///
    /// Cancel-safe — both underlying streams are, and losing a `select!` race here only
    /// means the delivered-signal flag stays set for the next poll.
    pub(crate) async fn recv(&mut self) -> Trigger {
        match (&mut self.interrupt, &mut self.terminate) {
            (Some(interrupt), Some(terminate)) => {
                tokio::select! {
                    _ = interrupt.recv() => Trigger::Interrupt,
                    _ = terminate.recv() => Trigger::Terminate,
                }
            }
            (Some(interrupt), None) => {
                interrupt.recv().await;
                Trigger::Interrupt
            }
            (None, Some(terminate)) => {
                terminate.recv().await;
                Trigger::Terminate
            }
            (None, None) => std::future::pending().await,
        }
    }
}

/// Everywhere else there is only Ctrl-C. Nothing in this workspace targets a non-unix
/// host, but the crate compiles there so `cargo check` on one is not a wall of errors.
#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct Signals {
    /// Set once `ctrl_c()` has failed, so the failure is logged once rather than spun on.
    unavailable: bool,
}

#[cfg(not(unix))]
impl Signals {
    pub(crate) fn install() -> Self {
        Self { unavailable: false }
    }

    pub(crate) async fn recv(&mut self) -> Trigger {
        if self.unavailable {
            return std::future::pending().await;
        }
        match tokio::signal::ctrl_c().await {
            Ok(()) => Trigger::Interrupt,
            Err(err) => {
                tracing::warn!(error = %err, "could not listen for Ctrl-C");
                self.unavailable = true;
                std::future::pending().await
            }
        }
    }
}
