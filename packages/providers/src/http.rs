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

use anyhow::Context;

/// Ceiling on a whole request — connect, send, and read the body. Roughly 40× what these
/// APIs actually take (Frankfurter answers in ~150ms), and well under the 10s drain grace,
/// so the scheduler always finishes the task it is on.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
/// Sub-limit on establishing the connection, so a host that blackholes SYNs fails fast
/// instead of spending the whole [`REQUEST_TIMEOUT`] before the first byte is sent.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Ceiling on a response body we will hold in memory. [`REQUEST_TIMEOUT`] bounds *time, not
/// bytes*: 6s on a gigabit link is ~750MB of body, and `Response::json` accumulates the
/// whole thing and then copies it contiguously, so the peak is roughly twice that. The real
/// payloads are nowhere near it — Frankfurter's entire rate table is ~2KB and a decade of
/// daily Yahoo closes ~200KB — so 8MiB is already ~40× the largest thing either API has
/// reason to send, while capping the damage a compromised or malfunctioning upstream can do
/// to this process's memory.
const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// The bounded client the workspace-version providers share (Frankfurter, Yahoo).
///
/// Panics only where `reqwest::Client::new()` — the call this replaces — already did: a
/// TLS backend that could not initialise. Deliberately not a fallback to the default
/// client, which would quietly hand back the unbounded one this module exists to prevent.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        // reqwest's default is `Policy::limited(10)`, and on a cross-host redirect it strips
        // only the headers it knows are credentials — `Authorization`, `Cookie`,
        // `Proxy-Authorization`, `WWW-Authenticate`. A *custom* header is forwarded, and
        // Akahu splits its credentials across two, one of which (`X-Akahu-Id`, the app
        // token) is exactly such a header: a redirecting upstream would hand it to whatever
        // host it names, in plaintext if that host is `http://`. None of these three APIs
        // redirects, so refusing outright costs nothing — a 3xx just arrives as an ordinary
        // non-success status through the existing `error_for_status()` handling.
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
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
        // Same reasoning as `client()` above, and it bites hardest here: the `X-Akahu-Id`
        // app token is a custom header, so it is the one reqwest would *not* strip on a
        // cross-host redirect.
        .redirect(reqwest_akahu::redirect::Policy::none())
        .https_only(true)
        .build()
        .expect("build a bounded HTTP client")
}

/// Read a JSON response body into `T`, refusing to buffer more than [`MAX_BODY_BYTES`].
///
/// Two guards, because either alone is bypassable. `Content-Length` is checked first so a
/// body the upstream *declares* is oversized costs no allocation at all; but that header is
/// absent on a chunked response and is only ever a claim, so the body is then read
/// chunk-by-chunk against a running total and abandoned — connection dropped mid-stream,
/// rather than drained — the moment it crosses the cap.
///
/// `Response::chunk` rather than `bytes_stream()` on purpose: the latter lives behind
/// reqwest's `stream` feature, which this crate does not enable (see `Cargo.toml`); the
/// bound is identical either way.
pub(crate) async fn json_capped<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> anyhow::Result<T> {
    // Cloned up front: `chunk()` needs `&mut response`, so the borrow `url()` hands out
    // cannot be held across the read loop that reports it.
    let url = response.url().clone();

    if let Some(declared) = response.content_length() {
        enforce_cap(declared, &url)?;
    }

    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        enforce_cap(body.len() as u64 + chunk.len() as u64, &url)?;
        body.extend_from_slice(&chunk);
    }

    serde_json::from_slice(&body).with_context(|| format!("decode the JSON body from {url}"))
}

/// The ceiling comparison itself, applied both to a declared `Content-Length` and to the
/// running total actually read. Split out of [`json_capped`] so the boundary is unit-testable
/// without a `reqwest::Response`: hand-constructing one needs the `http` crate (not a
/// dependency here), and the alternative — a live socket — is far too much machinery to catch
/// an off-by-one in a comparison.
fn enforce_cap(bytes: u64, url: &reqwest::Url) -> anyhow::Result<()> {
    anyhow::ensure!(
        bytes <= MAX_BODY_BYTES,
        "response body from {url} is over the {MAX_BODY_BYTES} byte ceiling ({bytes} bytes)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> reqwest::Url {
        reqwest::Url::parse("https://api.frankfurter.dev/v1/latest?base=NZD").unwrap()
    }

    #[test]
    fn caps_at_the_ceiling_inclusive() {
        // A real payload (~2KB) and a body exactly at the cap both pass; one byte past it,
        // whether declared or accumulated, does not.
        assert!(enforce_cap(2_048, &url()).is_ok());
        assert!(enforce_cap(MAX_BODY_BYTES, &url()).is_ok());
        assert!(enforce_cap(MAX_BODY_BYTES + 1, &url()).is_err());
    }

    #[test]
    fn the_cap_error_names_the_host_and_the_size() {
        let err = enforce_cap(MAX_BODY_BYTES * 2, &url())
            .unwrap_err()
            .to_string();
        assert!(err.contains("api.frankfurter.dev"), "{err}");
        assert!(err.contains(&(MAX_BODY_BYTES * 2).to_string()), "{err}");
    }
}
