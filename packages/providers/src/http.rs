//! Outbound HTTP clients for the provider adapters.
//!
//! Every provider here reaches a third-party host, and four of them are driven by
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
/// `pub(crate)` because every adapter needs the same number and none of them can enforce it
/// itself: each body is read inside a wire crate (`akahu-client`, `frankfurter-client`,
/// `house-pricer-client`, `yahoo-finance-client`), so the ceiling is handed to each client's
/// `with_max_response_bytes` at construction. One const for all four, so "how much of a response
/// may this process buffer?" has a single answer rather than four that drift apart — each client
/// ships its own, smaller default for callers who have no such policy.
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

/// The bounded client **every** provider here shares — the one handed to each of the four wire
/// crates — aimed at `endpoint`.
///
/// One function rather than two only since reqwest 0.13: `akahu-client` needs 0.13, so while
/// the workspace was on 0.12 this crate carried a renamed second copy of reqwest and a second,
/// byte-identical builder next to this one, because `reqwest::ClientBuilder` and
/// `reqwest_akahu::ClientBuilder` were unrelated types. Nothing about the *bounds* differed
/// then and nothing does now, which is the point: the timeout, the redirect refusal and the
/// TLS decision are one set of answers for every host this process talks to.
///
/// The one bound that is *not* on this client is the byte ceiling, because a `ClientBuilder`
/// has nowhere to put it. Every body is read inside a wire crate, so each adapter passes
/// [`MAX_BODY_BYTES`] to its client's `with_max_response_bytes` at construction instead.
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
        // like: `Policy::none()` returns the redirect response as-is, and a wire crate treats
        // exactly what `error_for_status()` treats as a failure — `is_client_error() ||
        // is_server_error()`. So the 3xx arrives as `Ok`, and its body — empty, or the HTML a
        // redirect usually carries — reaches the client's body reader and fails to deserialise.
        // The error is "could not decode the JSON body from <url>", not a status error.
        // `tests/http_bounds.rs` pins that, and pins the part that actually matters: the host
        // named in `Location` is never contacted.
        .redirect(reqwest::redirect::Policy::none())
        // No longer a blanket `true`, and still not a flag: the answer comes from the
        // [`Endpoint`], which decided it once at parse time. `https_only` rejects the request
        // before it is sent, which is exactly what a loopback proxy — no certificate, nothing
        // to hand a TLS handshake — needs to be exempt from.
        .https_only(endpoint.requires_tls())
        .build()
        .expect("build a bounded HTTP client")
}

/// How long to stand down when an upstream refuses on volume without saying for how long.
///
/// A 429 with no `Retry-After` is the common case (Yahoo's undocumented endpoint sends none at
/// all, and answers a burst with a temporary IP block rather than a header). Long enough that
/// the next scheduled poll is the thing that retries rather than the next page render; short
/// enough that one spurious refusal does not cost a household its prices for the afternoon.
///
/// Every adapter reaches it the same way now: a wire crate consumes the response, so the only
/// evidence that arrives here is an error variant — `AkahuError::RateLimited`, which carries no
/// header at all, or a `RateLimited { retry_after }` from the other three, which carries
/// whatever the upstream named and `None` when it named nothing. One const for all of them, so
/// "how long do we stand down when we were not told?" has a single answer.
pub(crate) const DEFAULT_BACKOFF: Duration = Duration::from_secs(60);

/// How often this process may talk to one upstream host, and how long it stands down when that
/// host tells it to.
///
/// Injected, not read here: `docs/ARCHITECTURE.md` has it that nothing in this crate reads
/// configuration, for the same reason [`Endpoint`] is injected — the composition root owns
/// every decision an operator can change, and an adapter that reached for the environment on a
/// request path would put a second one behind its back. `sure-server`'s `Config::from_env`
/// parses these three from `PROVIDER_MIN_REQUEST_INTERVAL_MS`, `PROVIDER_MAX_BACKOFF_SECS` and
/// `PROVIDER_DISCOVERY_TTL_SECS`; [`Pacing::default`] is what those defaults are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacing {
    /// Floor on the gap between any two requests this process sends to one host.
    ///
    /// Deliberately a *sleep*, not a refusal: it is sub-second, so paying it is invisible next
    /// to the request it precedes, and a caller that queues behind it still gets its answer.
    pub min_request_interval: Duration,
    /// Ceiling on a `Retry-After` this process will honour.
    ///
    /// An upstream must not be able to disable an integration for an hour by saying so —
    /// `Retry-After: 86400` is a header, not a contract, and the scheduled polls behind it are
    /// hours apart anyway. Clamping keeps a hostile or misconfigured value from outliving the
    /// interval that would have retried past it.
    pub max_backoff: Duration,
    /// How long an upstream's answer about *what exists* may be reused without asking again.
    ///
    /// Two things share it because they are the same question in two shapes: which accounts a
    /// set of Akahu credentials can see (`AkahuProvider::list_accounts`), and whether Yahoo has
    /// any price data at all for a symbol (`YahooFinanceProvider`'s empty-result memo). Neither
    /// changes minute to minute, and both are asked once per page render by a UI that has no
    /// idea it is reaching a third party.
    pub discovery_ttl: Duration,
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            min_request_interval: Duration::from_millis(500),
            max_backoff: Duration::from_secs(300),
            discovery_ttl: Duration::from_secs(60),
        }
    }
}

impl Pacing {
    /// Every window zeroed — for a test whose subject is not the pacing.
    ///
    /// A fixture that fires six requests at its own loopback proxy has no upstream to be polite
    /// to, and paying [`Pacing::default`]'s 500ms six times turns a millisecond test into a
    /// three-second one. The tests that *are* about pacing (`tests/yahoo_finance.rs`) pass a
    /// real [`Pacing`] instead, and measure it.
    ///
    /// Note what stays live: [`Self::max_backoff`] keeps its real value, so
    /// [`Throttle::note_refusal`] still arms a cooldown and [`Throttle::acquire`] still refuses
    /// inside one — a test can exercise the 429 path without waiting out a pacing interval it
    /// does not care about. What goes to zero is the sleep, and the
    /// [`Self::discovery_ttl`] — a zero TTL is already expired, so a fixture sees every call
    /// reach its proxy unless the test is specifically about the cache.
    pub fn unpaced() -> Self {
        Self {
            min_request_interval: Duration::ZERO,
            discovery_ttl: Duration::ZERO,
            ..Self::default()
        }
    }
}

/// One upstream host's share of this process's outbound budget: pacing between requests, and a
/// stand-down window the host itself can arm.
///
/// Per adapter *instance*, which is per host — each adapter is built once in the composition
/// root and talks to exactly one API, so "this instance's last request" and "this host's last
/// request" are the same fact. A `static` keyed by hostname would say the same thing and would
/// also leak between the tests in a binary, each of which stands up its own proxy.
///
/// The two mechanisms are deliberately different in kind, and must stay that way:
///
/// * **Pacing sleeps.** [`Pacing::min_request_interval`] is sub-second, and the lock is held
///   across the sleep so concurrent callers *queue* rather than all waking together and firing
///   at once — the property `tests/yahoo_finance.rs`'s concurrency test pins.
/// * **A cooldown refuses.** [`Pacing::max_backoff`] is up to five minutes; sleeping that out
///   inside a request would blow the 30s route deadline (`sure-api`'s `cache::timeout`) and
///   hold an in-flight permit while doing nothing. Erroring immediately is what lets the caller
///   fall back to what it already has — the whole point of the exercise.
pub(crate) struct Throttle {
    pacing: Pacing,
    state: tokio::sync::Mutex<ThrottleState>,
}

#[derive(Default)]
struct ThrottleState {
    /// When the last request *left*, stamped before it is sent rather than after it returns:
    /// the interval is about how often this host is contacted, not about how long it takes to
    /// answer.
    last_request: Option<std::time::Instant>,
    /// Set by [`Throttle::note_refusal`]; cleared lazily by [`Throttle::acquire`] once it has
    /// elapsed, so nothing has to run on a timer to forget it.
    cooldown_until: Option<std::time::Instant>,
}

impl Throttle {
    pub(crate) fn new(pacing: Pacing) -> Self {
        Self {
            pacing,
            state: tokio::sync::Mutex::new(ThrottleState::default()),
        }
    }

    pub(crate) fn pacing(&self) -> Pacing {
        self.pacing
    }

    /// Clear this request to go out: wait out the pacing interval, or refuse if `host` has
    /// asked this process to stand down and the window has not elapsed.
    ///
    /// `host` is only for the message, and the message matters — it is what lands in
    /// `provider_syncs.detail` and in a 422 body, so it has to say that the request was never
    /// sent rather than that the upstream failed.
    ///
    /// Records `provider_throttle_wait`, and does it here rather than at the call sites for two
    /// reasons. Every adapter that paces itself gets the metric without remembering to — this
    /// is the one funnel — and the timing starts *before* the lock, not just around the sleep:
    /// with several callers queued (the stock-price poll and an on-demand lookup sharing one
    /// `Arc`), most of the wait is spent waiting for the mutex rather than in the sleep after
    /// it, so timing only the sleep would report a fraction of the real cost. Without this a
    /// backfill over 40 tickers spends 20 seconds in here and reports only that
    /// `fetch_daily_prices` was slow.
    pub(crate) async fn acquire(&self, host: &str) -> anyhow::Result<()> {
        let started = std::time::Instant::now();
        let mut state = self.state.lock().await;

        if let Some(until) = state.cooldown_until {
            let remaining = until.saturating_duration_since(std::time::Instant::now());
            anyhow::ensure!(
                remaining.is_zero(),
                "{host} asked this process to stop sending requests and {remaining:?} of that \
                 window remains, so this request was not sent"
            );
            // Elapsed: forget it here rather than on a timer.
            state.cooldown_until = None;
        }

        if let Some(last) = state.last_request {
            let elapsed = last.elapsed();
            if elapsed < self.pacing.min_request_interval {
                tokio::time::sleep(self.pacing.min_request_interval - elapsed).await;
            }
        }
        state.last_request = Some(std::time::Instant::now());
        // Only a cleared request is recorded. A refusal above returns early and is counted by
        // the caller's own error path — folding it in here would put a near-zero sample into a
        // histogram that is supposed to answer "how long are we spending waiting to send?".
        sure_telemetry::instruments().provider_throttle_wait.record(
            sure_telemetry::secs(started.elapsed()),
            &[sure_telemetry::KeyValue::new(
                "provider",
                host.to_ascii_lowercase().replace(' ', "_"),
            )],
        );
        Ok(())
    }

    /// An upstream has refused on volume: arm the cooldown and hand back the error to fail this
    /// call with.
    ///
    /// **Which statuses count is no longer decided here**, and cannot be: every adapter in this
    /// crate now learns about a refusal from its wire crate's own error variant rather than from
    /// a `reqwest::Response`, because the client consumed the response to read the body. So the
    /// clients apply the rule (`429`, or a `503` that *names* a `Retry-After` — a plain `503` is
    /// also what an ordinary outage looks like, and backing off on every one would turn a bad
    /// minute at Yahoo into a five-minute one) and hand over what they parsed. What stays here
    /// is what was always this crate's: how long "we were not told" is worth waiting
    /// ([`DEFAULT_BACKOFF`]), the clamp that stops an upstream disabling an integration by
    /// asking, and the message.
    ///
    /// `host` is the adapter's display name for the upstream, the same one it passes to
    /// [`Self::acquire`] — so the refusal and the next call's local rejection name it
    /// identically. Both land in `provider_syncs.detail` and in a 422 body.
    pub(crate) async fn note_refusal(
        &self,
        host: &str,
        status: u16,
        retry_after: Option<Duration>,
    ) -> anyhow::Error {
        let window = retry_after
            .unwrap_or(DEFAULT_BACKOFF)
            .min(self.pacing.max_backoff);
        self.back_off(window).await;

        tracing::warn!(
            %host,
            status,
            backoff = ?window,
            "upstream refused on volume; standing down"
        );
        anyhow::anyhow!(
            "{host} refused this request with HTTP {status} and this process is standing down \
             for {window:?} before contacting it again"
        )
    }

    /// Arm the stand-down window, clamped to [`Pacing::max_backoff`].
    ///
    /// Separate from [`Self::note_refusal`] because Akahu keeps the upstream's own error rather
    /// than replacing it: a `404` from Akahu is classified into an `AccountDisconnected` on the
    /// way past (see `akahu::AkahuProvider::sent`), so that path needs the window armed and the
    /// error left alone. `akahu.rs` calls this directly with [`DEFAULT_BACKOFF`], which is also
    /// all it could pass — `AkahuError::RateLimited` carries a message and no `Retry-After`.
    ///
    /// Never shortens a window already in force — two refusals in flight at once must not let
    /// the second one's smaller number undo the first's.
    pub(crate) async fn back_off(&self, window: Duration) {
        let until = std::time::Instant::now() + window.min(self.pacing.max_backoff);
        let mut state = self.state.lock().await;
        state.cooldown_until = Some(match state.cooldown_until {
            Some(existing) => existing.max(until),
            None => until,
        });
    }
}

/// Time one outbound call and record its duration and outcome.
///
/// Wraps the adapter *method* rather than the `reqwest` call inside it, for a reason worth
/// knowing: Akahu goes through the `akahu-client` SDK, which owns its own HTTP client, so there
/// is no single `send()` in this crate that all three adapters pass through. Wrapping at this
/// level covers all of them identically and measures what a caller actually waits for —
/// including the JSON decode and, for Yahoo, the self-imposed throttle.
///
/// `outcome` is `ok` or `error` only. The error type here is `anyhow::Error` by the time it
/// arrives, so a status code is not recoverable from it without every adapter agreeing on a
/// richer error type — which is a bigger change than this metric justifies. Note the one case
/// that reads oddly as a result: Yahoo turns a 404 into `Ok(vec![])` for a delisted ticker, so
/// that is an `ok` with no rows, not an `error`.
pub(crate) async fn timed<T, F>(
    provider: &'static str,
    operation: &'static str,
    call: F,
) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    let started = std::time::Instant::now();
    let result = call.await;
    let elapsed = started.elapsed();
    sure_telemetry::instruments()
        .provider_request_duration
        .record(
            sure_telemetry::secs(elapsed),
            &[
                sure_telemetry::KeyValue::new("provider", provider),
                sure_telemetry::KeyValue::new("operation", operation),
                sure_telemetry::KeyValue::new(
                    "outcome",
                    if result.is_ok() { "ok" } else { "error" },
                ),
            ],
        );
    result
}
#[cfg(test)]
mod tests {
    use super::*;

    /// The distinction the two mechanisms rest on: pacing sleeps, a cooldown refuses.
    ///
    /// Asserted with a zero pacing interval so the test measures the refusal and not a sleep —
    /// `acquire` must return `Err` immediately while the window holds, because sleeping out a
    /// five-minute `Retry-After` inside a request would blow the 30s route deadline and hold an
    /// in-flight permit doing nothing.
    #[tokio::test]
    async fn a_cooldown_refuses_rather_than_waiting() {
        let throttle = Throttle::new(Pacing::unpaced());
        throttle
            .acquire("Frankfurter")
            .await
            .expect("nothing armed");

        throttle.back_off(Duration::from_secs(30)).await;
        let err = throttle
            .acquire("Frankfurter")
            .await
            .expect_err("the window is still open")
            .to_string();
        // The message is what lands in `provider_syncs.detail` and in a 422 body, so it has to
        // say the request was never sent rather than that the upstream failed.
        assert!(err.contains("Frankfurter"), "{err}");
        assert!(err.contains("not sent"), "{err}");
    }

    /// An elapsed window is forgotten on the next attempt, with nothing running on a timer to
    /// do it — the property that lets the cooldown be one `Option<Instant>` rather than a task.
    #[tokio::test]
    async fn an_elapsed_cooldown_lets_the_next_request_through() {
        let throttle = Throttle::new(Pacing::unpaced());
        throttle.back_off(Duration::ZERO).await;
        throttle
            .acquire("Yahoo Finance")
            .await
            .expect("a zero-length window is already over");
    }

    /// Two refusals in flight at once must not let the second one's smaller number undo the
    /// first's. The shape in production: a burst of price lookups, each getting its own 429,
    /// the last of which carries a shorter `Retry-After` than the first.
    #[tokio::test]
    async fn a_second_backoff_never_shortens_the_window_already_in_force() {
        let throttle = Throttle::new(Pacing::unpaced());
        throttle.back_off(Duration::from_secs(300)).await;
        throttle.back_off(Duration::from_secs(1)).await;
        assert!(
            throttle.acquire("Yahoo Finance").await.is_err(),
            "the longer window has to survive the shorter one"
        );
    }

    /// An upstream must not be able to disable an integration for a day by saying so.
    #[tokio::test]
    async fn a_retry_after_is_clamped_to_max_backoff() {
        let pacing = Pacing {
            min_request_interval: Duration::ZERO,
            max_backoff: Duration::from_millis(50),
            discovery_ttl: Duration::ZERO,
        };
        let throttle = Throttle::new(pacing);
        // A day, asked for; 50ms, honoured.
        throttle.back_off(Duration::from_secs(86_400)).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        throttle
            .acquire("Yahoo Finance")
            .await
            .expect("the clamped window has elapsed");
    }

    /// Pacing, as distinct from the cooldown above: it delays, it does not fail.
    #[tokio::test]
    async fn pacing_spaces_requests_without_refusing_them() {
        let pacing = Pacing {
            min_request_interval: Duration::from_millis(80),
            ..Pacing::unpaced()
        };
        let throttle = Throttle::new(pacing);

        let started = std::time::Instant::now();
        for _ in 0..3 {
            throttle
                .acquire("Yahoo Finance")
                .await
                .expect("pacing delays, it never refuses");
        }
        // Three requests, two gaps. The first goes out immediately: the interval is about the
        // gap between requests, not a toll on the first one.
        assert!(
            started.elapsed() >= Duration::from_millis(160),
            "three paced requests took {:?}; two 80ms gaps is the floor",
            started.elapsed()
        );
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
