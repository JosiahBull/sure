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
//!
//! The other thing a client here is bounded by is *where it may point*. [`Endpoint`] is what
//! makes a provider's base URL injectable — so a test can aim an adapter at a record/replay
//! proxy on loopback — without also making "plaintext to an arbitrary host" expressible.

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
///
/// `pub(crate)` because Akahu needs the same number and cannot get it the same way: that
/// body is read inside `akahu-client`, so the ceiling is handed to `AkahuClient` instead of
/// being enforced by [`json_capped`] here — see [`akahu_client`] below. One const for both,
/// so "how much of a response may this process buffer?" has a single answer rather than two
/// that drift apart.
pub(crate) const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;

/// Where a provider is allowed to point, and what that implies for the transport.
///
/// Every adapter in this crate reaches a third-party host over TLS, and Akahu sends
/// credentials to do it — which is why the two builders below set `https_only`, refusing a
/// plaintext URL outright instead of downgrading to it. That is also precisely what a test
/// proxy has to be exempt from: `partly-proxy-lib` is a *reverse* proxy, one listener per
/// upstream, with no certificate to present — so pointing an adapter at it means
/// `http://127.0.0.1:<ephemeral port>`.
///
/// The exemption is a **parsed invariant, not configuration**. The alternative considered was
/// an env flag — `ALLOW_PLAINTEXT_PROVIDERS` — and rejected because it is a switch that
/// exists in production: with it set, `AKAHU_BASE_URL=http://evil.example` puts the app token
/// on the wire in the clear, and nothing distinguishes that process from a correctly
/// configured one. Here that URL simply cannot be represented — [`Endpoint::parse`] fails in
/// the composition root, before the server binds, with the offending URL in the message — and
/// the loopback carve-out cannot be widened by anything an operator sets. Widening it takes an
/// edit to this file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// The real thing: an `https://` URL.
    Secure(String),
    /// A loopback `http://` URL — a test proxy standing in for the upstream.
    LoopbackPlaintext(String),
}

/// The hosts a plaintext provider URL may name: this machine, however it is spelled.
///
/// Both `::1` and `[::1]` are listed because the bracketed form is a detail of how
/// `Url::host_str` hands back the authority it parsed, not something worth depending on.
/// Deliberately *not* `IpAddr::is_loopback()`, which would admit all of 127.0.0.0/8: the
/// carve-out should be the smallest one that serves its only caller, and that caller binds
/// `127.0.0.1`.
const LOOPBACK_HOSTS: [&str; 4] = ["127.0.0.1", "::1", "[::1]", "localhost"];

impl Endpoint {
    /// Parse a configured URL, refusing anything that would send credentials in plaintext to
    /// a host that is not this machine.
    ///
    /// The caller's own string is what gets stored, not `Url`'s re-serialisation of it: every
    /// adapter builds its request path by concatenation (`format!("{base}/latest?…")`), and
    /// `Url` normalises an empty path to `/` — so round-tripping `http://127.0.0.1:8080`
    /// through it would silently produce `http://127.0.0.1:8080//latest`. Parsing is used
    /// here for its *judgement* (scheme and host), not for its output.
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let parsed = reqwest::Url::parse(url)
            .with_context(|| format!("provider endpoint '{url}' is not a valid URL"))?;
        let host = parsed.host_str().unwrap_or_default();
        match parsed.scheme() {
            "https" => Ok(Self::Secure(url.to_string())),
            "http" if LOOPBACK_HOSTS.contains(&host) => {
                Ok(Self::LoopbackPlaintext(url.to_string()))
            }
            "http" => anyhow::bail!(
                "provider endpoint '{url}' is plaintext http:// to '{host}', which is not this \
                 machine — only https:// may leave it. A test proxy must be reached on one of \
                 {LOOPBACK_HOSTS:?}."
            ),
            // A URL scheme is a genuinely open string (CLAUDE.md rule 2's first escape
            // hatch), so this arm is the only way to name "anything else".
            other => anyhow::bail!(
                "provider endpoint '{url}' uses the '{other}' scheme; only https:// (or \
                 http:// on loopback, for a test proxy) can carry a provider request"
            ),
        }
    }

    /// The URL to build request paths from, exactly as it was configured.
    pub fn url(&self) -> &str {
        match self {
            Self::Secure(url) | Self::LoopbackPlaintext(url) => url,
        }
    }

    /// Whether a client aimed here must refuse a non-TLS request.
    ///
    /// One function rather than the same two-arm match inside each builder below: two copies
    /// of one mapping is what CLAUDE.md rule 1 exists to prevent, and this is the mapping
    /// that decides whether an app token can go out in the clear.
    pub(crate) fn requires_tls(&self) -> bool {
        match self {
            Self::Secure(_) => true,
            Self::LoopbackPlaintext(_) => false,
        }
    }
}

/// The bounded client the workspace-version providers share (Frankfurter, Yahoo), aimed at
/// `endpoint`.
///
/// Panics only where `reqwest::Client::new()` — the call this replaces — already did: a
/// TLS backend that could not initialise. Deliberately not a fallback to the default
/// client, which would quietly hand back the unbounded one this module exists to prevent.
pub(crate) fn client(endpoint: &Endpoint) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        // reqwest's default is `Policy::limited(10)`, and on a cross-host redirect it strips
        // only the headers it knows are credentials — `Authorization`, `Cookie`,
        // `Proxy-Authorization`, `WWW-Authenticate`. A *custom* header is forwarded, and
        // Akahu splits its credentials across two, one of which (`X-Akahu-Id`, the app
        // token) is exactly such a header: a redirecting upstream would hand it to whatever
        // host it names, in plaintext if that host is `http://`. None of these three APIs
        // redirects, so refusing outright costs nothing.
        //
        // What a 3xx then *does* cost is worth stating exactly, because it is not what it looks
        // like: `Policy::none()` returns the redirect response as-is, and `error_for_status()`
        // fails only on `is_client_error() || is_server_error()`. So the 3xx arrives as `Ok`,
        // and its body — empty, or the HTML a redirect usually carries — reaches [`json_capped`]
        // and fails to deserialise. The error is "decode the JSON body from <url>", not a status
        // error. `tests/http_bounds.rs` pins that, and pins the part that actually matters: the
        // host named in `Location` is never contacted.
        .redirect(reqwest::redirect::Policy::none())
        // No longer a blanket `true`, and still not a flag: the answer comes from the
        // [`Endpoint`], which decided it once at parse time. `https_only` rejects the request
        // before it is sent, which is exactly what a loopback proxy — no certificate, nothing
        // to hand a TLS handshake — needs to be exempt from.
        .https_only(endpoint.requires_tls())
        .build()
        .expect("build a bounded HTTP client")
}

/// The same, for `akahu-client`, which takes a `reqwest` 0.13 client — a different major
/// from the workspace's 0.12, so the two builders cannot be one function. Same limits, on
/// purpose: the provider poll is a scheduled task like the others.
///
/// One limit is missing here and cannot be added here: the byte ceiling. `akahu-client`
/// executes the request and reads the body itself, so by the time a caller sees anything the
/// buffering has already happened — [`json_capped`] never gets a `Response` to bound. Since
/// 0.3 the crate takes the ceiling as a setting instead, and `akahu::AkahuProvider::client`
/// hands it [`MAX_BODY_BYTES`]; a client built from this function and left at the crate's own
/// default is bounded, but by *its* number rather than ours.
pub(crate) fn akahu_client(endpoint: &Endpoint) -> reqwest_akahu::Client {
    reqwest_akahu::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        // Same reasoning as `client()` above, and it bites hardest here: the `X-Akahu-Id`
        // app token is a custom header, so it is the one reqwest would *not* strip on a
        // cross-host redirect.
        .redirect(reqwest_akahu::redirect::Policy::none())
        // And the same reason this is a question rather than a constant — with the same
        // token at stake, which is why [`Endpoint`] refuses to represent plaintext to
        // anywhere but this machine.
        .https_only(endpoint.requires_tls())
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

    #[test]
    fn an_https_endpoint_keeps_the_transport_it_was_configured_with() {
        let endpoint = Endpoint::parse("https://api.akahu.io/v1").expect("https parses");
        assert_eq!(endpoint, Endpoint::Secure("https://api.akahu.io/v1".into()));
        assert!(endpoint.requires_tls());
        // Verbatim, not `Url`'s re-serialisation: the adapters append `/latest?…` to this.
        assert_eq!(endpoint.url(), "https://api.akahu.io/v1");
    }

    /// The one case that has to work for a fixture to exist at all: a proxy on an ephemeral
    /// loopback port, and the `https_only` exemption that lets a client actually reach it.
    #[test]
    fn a_loopback_http_endpoint_is_the_one_plaintext_case() {
        for url in [
            "http://127.0.0.1:53219/v1",
            "http://localhost:53219/v1",
            "http://[::1]:53219/v1",
        ] {
            let endpoint = Endpoint::parse(url).unwrap_or_else(|e| panic!("{url}: {e}"));
            assert_eq!(endpoint, Endpoint::LoopbackPlaintext(url.to_string()));
            assert!(!endpoint.requires_tls(), "{url}");
            assert_eq!(endpoint.url(), url);
        }
    }

    /// The misconfiguration the type exists to make unrepresentable. An env flag would have
    /// accepted every one of these; the error has to name the URL, because it is read at
    /// startup where the operator's only clue is the message.
    #[test]
    fn plaintext_off_this_machine_is_refused_by_name() {
        for url in [
            "http://evil.example/v1",
            // A loopback *spelling* inside another host's name is not loopback — and neither
            // is a public address that merely resolves there.
            "http://localhost.evil.example/v1",
            "http://10.0.0.5:8080/v1",
        ] {
            let err = Endpoint::parse(url)
                .expect_err("plaintext to a non-loopback host must not parse")
                .to_string();
            assert!(err.contains(url), "the error must name the URL: {err}");
        }
    }

    #[test]
    fn a_non_http_scheme_or_a_non_url_is_refused_by_name() {
        // `file://` and `ftp://` parse fine as URLs and are still not something a provider
        // request can travel over; a bare host is not a URL at all. Both arrive as an error
        // naming the input rather than as a client that fails later, mid-sync.
        for url in [
            "file:///etc/passwd",
            "ftp://api.akahu.io/v1",
            "api.akahu.io",
        ] {
            let err = Endpoint::parse(url)
                .expect_err("only http(s) can carry a provider request")
                .to_string();
            assert!(err.contains(url), "the error must name the URL: {err}");
        }
    }
}
