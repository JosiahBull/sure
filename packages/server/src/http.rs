//! The TCP accept loop, and the connection-level limits that only exist here.
//!
//! # Why this isn't `axum::serve`
//!
//! `axum::serve` constructs its hyper connection builder internally and exposes none of
//! it. That is fine until you want the guards below, and one of them is not optional:
//! hyper's `header_read_timeout` defaults to 30s but is silently disabled unless a
//! [`Timer`](hyper::rt::Timer) is installed on the builder, which `axum::serve` never
//! does. Without it a client can open a connection, send one byte of a request line, and
//! hold the connection open indefinitely — the classic slowloris — and enough of those
//! exhaust the process's file descriptors.
//!
//! Everything else here follows from owning the loop: a ceiling on concurrent
//! connections, HTTP/2 stream limits, an accept loop that doesn't spin on `EMFILE`, and a
//! graceful drain so an in-flight SQLite write finishes before the process goes away.
//!
//! The drain is *started* elsewhere. Signals belong to `sure-appbase`, which owns the
//! whole shutdown sequence; this module waits on the [`Shutdown`] token it is handed. Two
//! independent listeners for the same `SIGTERM` would race over who gets to decide the
//! process is stopping.
//!
//! Nothing is given up in exchange. The app uses no protocol upgrades (no WebSockets), so
//! `serve_connection` is sufficient — and it is the variant that participates in graceful
//! shutdown.

use std::io;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, Request};
use axum::Router;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use sure_appbase::Shutdown;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower::ServiceExt;

/// hyper refuses a smaller HTTP/1 buffer than this.
const MIN_HTTP1_BUF: usize = 8192;

/// How long to pause after an accept failure that looks like resource exhaustion, so the
/// loop doesn't burn a core retrying thousands of times a second while the kernel has no
/// descriptors to give.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Connection-level tunables. Every field has a default in
/// [`Config::from_env`](crate::config::Config::from_env).
#[derive(Clone, Copy, Debug)]
pub struct HttpConfig {
    /// Ceiling on concurrent TCP connections. The loop stops accepting at the limit, so
    /// excess connections wait in the kernel's backlog instead of consuming descriptors.
    pub max_connections: usize,
    /// How long a client may take to send a complete request head. The guard against
    /// slowloris.
    pub header_read_timeout: Duration,
    /// Per-connection HTTP/1 read/write buffer ceiling. hyper's default is 400 KiB, which
    /// is generous for an API whose largest request is a file upload read in chunks.
    pub http1_max_buf_size: usize,
    /// Concurrent HTTP/2 streams per connection. hyper leaves this unlimited by default;
    /// a cap bounds how much one connection can ask for at once.
    pub h2_max_concurrent_streams: u32,
    /// HTTP/2 PING interval and deadline, so a connection to a peer that has vanished is
    /// reclaimed instead of held forever.
    pub h2_keep_alive_interval: Duration,
    pub h2_keep_alive_timeout: Duration,
    /// How long to let in-flight requests finish after a shutdown signal.
    pub shutdown_grace: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            max_connections: 512,
            header_read_timeout: Duration::from_secs(15),
            http1_max_buf_size: 64 * 1024,
            h2_max_concurrent_streams: 128,
            h2_keep_alive_interval: Duration::from_secs(30),
            h2_keep_alive_timeout: Duration::from_secs(20),
            shutdown_grace: Duration::from_secs(15),
        }
    }
}

/// Accept connections until `shutdown` is cancelled, then drain.
///
/// Returns once every connection has finished or the grace period has elapsed, so the
/// caller can close the database pool knowing no handler still holds it.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    cfg: HttpConfig,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    let builder = connection_builder(&cfg);
    let graceful = GracefulShutdown::new();
    let connections = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    let mut cancelled = pin!(shutdown.cancelled());

    loop {
        // Taken before accepting, so at the ceiling the loop simply stops calling
        // `accept()` and the kernel queues (then refuses) new connections for us. The
        // permit is released when the connection task ends.
        let permit = tokio::select! {
            permit = connections.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                // Only reachable if the semaphore is closed, which nothing here does.
                Err(_) => break,
            },
            () = &mut cancelled => break,
        };

        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    on_accept_error(&err).await;
                    continue;
                }
            },
            () = &mut cancelled => break,
        };

        // Small JSON responses shouldn't wait on Nagle's algorithm for a coalescing
        // partner that never comes.
        if let Err(err) = stream.set_nodelay(true) {
            tracing::debug!(%peer, error = %err, "could not set TCP_NODELAY");
        }

        // `ConnectInfo` is what the rate limiter keys on. `axum::serve` would supply it
        // via `into_make_service_with_connect_info`; owning the loop, we insert it here.
        let service = TowerToHyperService::new(app.clone().map_request(
            move |mut request: Request<Incoming>| {
                request.extensions_mut().insert(ConnectInfo(peer));
                request
            },
        ));

        // `into_owned` detaches the connection from the borrowed builder so it can be
        // spawned; the builder itself is cheap to clone.
        let connection = builder
            .serve_connection(TokioIo::new(stream), service)
            .into_owned();
        let watcher = graceful.watcher();

        // Tracked, not bare `tokio::spawn`. `graceful.shutdown()` below already waits for
        // these, so tracking changes nothing in the normal case — but when the drain
        // deadline is hit and connections are abandoned, tracking is what lets the
        // shutdown report say so (and, in a debug build, point at this line) instead of
        // the process exiting quietly over the top of them.
        shutdown.spawn(async move {
            if let Err(err) = watcher.watch(connection).await {
                // Client disconnects are routine and not worth an operator's attention.
                tracing::debug!(%peer, error = %err, "connection closed with an error");
            }
            drop(permit);
        });
    }

    // Stop accepting before draining, so the count can actually reach zero.
    drop(listener);
    tracing::info!(
        connections = graceful.count(),
        grace_secs = cfg.shutdown_grace.as_secs(),
        "shutdown signal received; draining"
    );
    tokio::select! {
        _ = graceful.shutdown() => tracing::info!("all connections drained"),
        _ = tokio::time::sleep(cfg.shutdown_grace) => {
            tracing::warn!("drain deadline exceeded; abandoning remaining connections");
        }
    }
    Ok(())
}

/// The hyper connection builder, with every abuse-relevant setting made explicit.
fn connection_builder(cfg: &HttpConfig) -> Builder<TokioExecutor> {
    let mut builder = Builder::new(TokioExecutor::new());

    builder
        .http1()
        // Installing the timer is what makes `header_read_timeout` take effect at all;
        // hyper logs a warning and ignores the timeout when none is set.
        .timer(TokioTimer::new())
        .header_read_timeout(cfg.header_read_timeout)
        .max_buf_size(cfg.http1_max_buf_size.max(MIN_HTTP1_BUF));

    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(cfg.h2_max_concurrent_streams)
        .keep_alive_interval(cfg.h2_keep_alive_interval)
        .keep_alive_timeout(cfg.h2_keep_alive_timeout);

    builder
}

/// Decide what to do about a failed `accept()`.
///
/// Never fatal: a listener that gives up on the first transient error is worse than one
/// that retries. Errors that mean "this particular connection went away" retry
/// immediately; anything else (descriptor or memory exhaustion, typically) backs off so
/// the loop can't spin a core while the kernel recovers.
async fn on_accept_error(err: &io::Error) {
    let transient = matches!(
        err.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
    );
    if transient {
        tracing::debug!(error = %err, "connection went away before it could be accepted");
    } else {
        tracing::warn!(error = %err, "accept failed; backing off");
        tokio::time::sleep(ACCEPT_BACKOFF).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_http1_buffer_never_goes_below_hypers_minimum() {
        // hyper panics rather than clamping, so a misconfigured env var must not reach it.
        let cfg = HttpConfig {
            http1_max_buf_size: 1,
            ..HttpConfig::default()
        };
        // Would panic inside hyper if the clamp were missing.
        let _ = connection_builder(&cfg);
    }

    #[test]
    fn defaults_are_sane() {
        let cfg = HttpConfig::default();
        assert!(
            cfg.header_read_timeout > Duration::ZERO,
            "slowloris guard must be on"
        );
        assert!(cfg.max_connections > 0);
        assert!(cfg.h2_max_concurrent_streams > 0);
    }
}
