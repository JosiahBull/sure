//! Outbound HTTP clients for the provider adapters.
//!
//! Every provider here reaches a third-party host, and three of them are driven by
//! `sure-scheduler` — which cancels *between* tasks, never during one, so a request in
//! flight when the shutdown signal lands is waited out rather than dropped. `reqwest`
//! sets no timeout of any kind by default (`timeout`, `connect_timeout` and
//! `read_timeout` are all `None`), so an upstream that accepts a connection and then
//! goes quiet holds the scheduler open until the drain deadline expires and the task is
//! abandoned — a dirty shutdown, and a WAL segment left behind, caused by someone else's
//! server.
//!
//! Hence: no provider builds a bare `Client::new()`. [`REQUEST_TIMEOUT`] is the number
//! that matters — it must stay comfortably under `SHUTDOWN_DRAIN_GRACE_SECS` (10s, see
//! `docs/HTTP.md`) so a stalled upstream cannot outlive the drain that is waiting on it.
//!
//! Cutting a poll short is cheap: `Scheduler` only records *successful* runs, so a timed-
//! out fetch is retried on the next check rather than waiting out its full interval.

use std::time::Duration;

/// Ceiling on a whole request — connect, send, and read the body. Roughly 40× what these
/// APIs actually take (Frankfurter answers in ~150ms), and well under the 10s drain grace,
/// so the scheduler always finishes the task it is on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
/// Sub-limit on establishing the connection, so a host that blackholes SYNs fails fast
/// instead of spending the whole [`REQUEST_TIMEOUT`] before the first byte is sent.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// The bounded client the workspace-version providers share (Frankfurter, Yahoo).
///
/// Panics only where `reqwest::Client::new()` — the call this replaces — already did: a
/// TLS backend that could not initialise. Deliberately not a fallback to the default
/// client, which would quietly hand back the unbounded one this module exists to prevent.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("build a bounded HTTP client")
}

/// The same, for `akahu-client`, which takes a `reqwest` 0.13 client — a different major
/// from the workspace's 0.12, so the two builders cannot be one function. Same limits, on
/// purpose: the provider poll is a scheduled task like the others.
pub(crate) fn akahu_client() -> reqwest_akahu::Client {
    reqwest_akahu::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .expect("build a bounded HTTP client")
}
