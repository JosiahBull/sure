//! The proxy cluster a provider test points `sure` at — one listener per third-party host.
//!
//! `partly-proxy-lib` is a **reverse** proxy, not a `CONNECT` proxy: each named upstream binds
//! its own listener and forwards everything it receives to one fixed base URL. So there is no
//! ambient `HTTPS_PROXY` to set and no interception to arrange — the system under test has to
//! be *told* a different base URL per upstream. That is the whole reason each provider's
//! endpoint is an env var ([`Upstream::env_var`]), and the reason [`Started::endpoints`] is
//! shaped the way it is: it is the environment a harness hands to `sure-server`, already
//! carrying the path prefixes, so no caller has to know that Yahoo's charts live under
//! `/v8/finance/chart`.
//!
//! Two consumers, one library:
//!
//! - in-process `#[tokio::test]`s in `sure-providers` call [`start`] and point a single adapter
//!   at the address it reports;
//! - the Playwright suites (`@sure/api-tests`, `@sure/web`) spawn the `sure-testproxy` binary
//!   once and drive it over the TCP JSON-Lines control plane (`SPECIFICATION.md` §12.2) while
//!   the real `sure-api` binary runs against the listeners.
//!
//! The property both rely on: in [`Mode::Replay`] the proxy never dials the upstream, so a
//! fixture nobody recorded surfaces as a `503`, not as a silent trip to the real internet
//! (`SPECIFICATION.md` §8.3). Everything else here — the canonicalised query, the stripped
//! credentials — exists so a *recorded* snapshot keeps matching next week and on someone
//! else's machine, because a snapshot that stops matching is how a suite gets quietly
//! downgraded to live traffic again.
//!
//! This crate deliberately depends on nothing else in the workspace: `sure-providers`
//! dev-depends on it, and a dependency in the other direction would be a cycle.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use bytes::Bytes;
use http::StatusCode;
use http::header::{CONTENT_TYPE, SET_COOKIE};
use partly_proxy_lib::{
    ClusterHandle, InMemoryStorage, Mode, Next, ProxyClusterBuilder, ProxyConfig, ProxyMiddleware,
    ProxyRequest, ProxyResponse, RecordedExchange, RecordingConfig, RequestContext, SharedStorage,
    UpstreamTarget, shared,
};

/// A third-party host `sure` talks to, and everything the proxy needs to stand in for it.
///
/// A closed set — these are the only hosts any adapter in `sure-providers` reaches — so it is
/// an enum rather than a bare name (CLAUDE.md rule 1). The text spellings live on [`name`],
/// once, at the edges that genuinely need text: a snapshot filename, a wire field, a
/// TypeScript test.
///
/// [`name`]: Upstream::name
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Upstream {
    Frankfurter,
    YahooFinance,
    Akahu,
}

impl Upstream {
    /// Every upstream, in the order the cluster binds them.
    ///
    /// Hand-maintained: Rust cannot enumerate an enum's variants without a macro, so adding a
    /// variant is a compile error at [`Upstream::name`] and the six matches beside it — which
    /// is the prompt to extend this array as well. `all_names_are_distinct` below is the
    /// backstop for the copy-paste failure that survives the compiler: two variants claiming
    /// one name, which `ProxyClusterBuilder::run` rejects at bind time as a duplicate.
    pub const ALL: [Upstream; 3] = [
        Upstream::Frankfurter,
        Upstream::YahooFinance,
        Upstream::Akahu,
    ];

    /// Cluster-facing name. Also the snapshot filename stem, and the `upstream` field on every
    /// recorded exchange and traffic filter — so a TypeScript test that says `"yahoo_finance"`
    /// and this enum are one shared spelling that must not drift. Changing a name here
    /// invalidates every snapshot file named after it and every control-plane command that
    /// scopes itself to it.
    pub fn name(self) -> &'static str {
        match self {
            Upstream::Frankfurter => "frankfurter",
            Upstream::YahooFinance => "yahoo_finance",
            Upstream::Akahu => "akahu",
        }
    }

    /// Scheme and host only — no path. The path lives in [`path_prefix`], and the split is
    /// load-bearing; see that method for why.
    ///
    /// [`path_prefix`]: Upstream::path_prefix
    pub fn target_base_url(self) -> &'static str {
        match self {
            Upstream::Frankfurter => "https://api.frankfurter.dev",
            Upstream::YahooFinance => "https://query1.finance.yahoo.com",
            Upstream::Akahu => "https://api.akahu.io",
        }
    }

    /// The part of the real endpoint that stays on `sure`'s side of the proxy.
    ///
    /// The proxy forwards `base_url + path_and_query` verbatim, so this prefix could equally
    /// well have gone on [`target_base_url`] — `UpstreamTarget` accepts a base URL with a path
    /// and prepends it. Putting it in the URL handed to `sure` instead means the *inbound*
    /// request the proxy sees is `/v1/accounts` rather than `/accounts`, and the replay key is
    /// computed from that inbound origin-form URI. Two payoffs: a snapshot file reads like the
    /// real API's traffic, and a `path_pattern` in a test (`^/v8/finance/chart/AAPL$`) is the
    /// path a developer would find in Yahoo's own documentation. A third, smaller one: a
    /// replay-miss log line carries no upstream name — the listener knows which upstream it
    /// is, the request does not — so the prefix is what tells you which feed the miss belongs
    /// to.
    ///
    /// [`target_base_url`]: Upstream::target_base_url
    pub fn path_prefix(self) -> &'static str {
        match self {
            Upstream::Frankfurter => "/v1",
            Upstream::YahooFinance => "/v8/finance/chart",
            Upstream::Akahu => "/v1",
        }
    }

    /// The env var `sure-server` reads this endpoint from.
    pub fn env_var(self) -> &'static str {
        match self {
            Upstream::Frankfurter => "FRANKFURTER_BASE_URL",
            Upstream::YahooFinance => "YAHOO_FINANCE_BASE_URL",
            Upstream::Akahu => "AKAHU_BASE_URL",
        }
    }

    /// Query parameters whose value is a clock reading, and which therefore have to be
    /// canonicalised before a snapshot key is computed.
    ///
    /// The replay index compares the query **verbatim** (`SPECIFICATION.md` §8.1), and two of
    /// these three feeds put the current time in it: `yahoo_finance.rs` derives
    /// `?period1=<epoch>&period2=<epoch>` from today's date, and `akahu.rs` sends
    /// `?start=<rfc3339>` derived from the last successful sync. Recorded verbatim, both
    /// snapshots stop matching the day after they are taken. Frankfurter's query is just
    /// `?base=NZD`, which is already stable, so it has nothing to canonicalise — an empty list
    /// rather than a special case at the call site.
    pub fn volatile_query_params(self) -> &'static [&'static str] {
        match self {
            Upstream::Frankfurter => &[],
            Upstream::YahooFinance => &["period1", "period2"],
            Upstream::Akahu => &["start"],
        }
    }

    /// Resolve a cluster-facing name, e.g. from a control-plane command or a `--upstream` flag.
    ///
    /// Derived from [`name`] rather than a second match, so the two directions cannot drift
    /// into disagreeing about a spelling.
    ///
    /// [`name`]: Upstream::name
    pub fn from_name(name: &str) -> Option<Self> {
        Upstream::ALL.into_iter().find(|up| up.name() == name)
    }
}

/// The value a canonicalised parameter is rewritten to. Any fixed string works; what matters
/// is that it is *fixed* — `redact_request_for_snapshot` must be a pure function of its input,
/// or the record-side and lookup-side hashes stop agreeing (`SPECIFICATION.md` §6.4).
pub const CANONICAL: &str = "CANONICAL";

/// Rewrites named query parameters to a fixed placeholder at the snapshot boundary, so a
/// request whose query carries a clock reading still matches the snapshot recorded from an
/// earlier one.
///
/// Sorts the surviving pairs as well as substituting them. Sorting is not required by the
/// index — it compares the query verbatim, and a caller that always builds its query in the
/// same order would match without it — but it costs nothing and makes the key independent of a
/// parameter order no test controls.
///
/// The live path is untouched: `handle` passes straight through, so the request that reaches a
/// stub or the real upstream still carries the real epochs.
///
/// Built and pinned by `sure-providers`' `tests/proxy_contract.rs`, which is where the
/// behaviour is actually proven — it records through a stub and replays against a *different*
/// query, and its negative control fails if `partly-proxy-lib` ever grows query normalisation
/// of its own and makes this middleware dead weight.
pub struct CanonicaliseQuery {
    /// Parameter names to substitute. Usually [`Upstream::volatile_query_params`]; a public
    /// field so the contract test can keep constructing it with its own list.
    pub params: &'static [&'static str],
}

#[async_trait]
impl ProxyMiddleware for CanonicaliseQuery {
    async fn handle(
        &self,
        req: ProxyRequest,
        ctx: &mut RequestContext,
        next: Next<'_>,
    ) -> partly_proxy_lib::Result<ProxyResponse> {
        next.run(req, ctx).await
    }

    fn redact_request_for_snapshot(&self, req: &mut ProxyRequest) {
        let Some(query) = req.uri.query() else {
            return;
        };
        let mut pairs: Vec<(String, String)> = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => (key.to_owned(), value.to_owned()),
                // A bare flag (`?debug`) round-trips as `debug=`. Lossy, but lossy the same
                // way on both sides of the boundary, which is the only property the key needs.
                None => (pair.to_owned(), String::new()),
            })
            .collect();
        for (key, value) in &mut pairs {
            if self.params.contains(&key.as_str()) {
                *value = CANONICAL.to_owned();
            }
        }
        pairs.sort();
        let rebuilt = pairs
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        // Origin-form: the inbound URI a proxy listener sees is path + query, with no scheme or
        // authority to preserve. A rewrite that somehow does not parse leaves the URI alone
        // rather than panicking — a redaction hook is infallible by design (§6.4), and an
        // un-canonicalised key costs a replay miss (loud, and logged) rather than the process.
        if let Ok(uri) = format!("{}?{rebuilt}", req.uri.path()).parse() {
            req.uri = uri;
        }
    }
}

/// Request headers that carry a credential, removed at the snapshot boundary.
///
/// Akahu splits its credentials across two headers — `X-Akahu-Id` is the app token identifying
/// this app, `X-Akahu-Auth` the user token identifying whose accounts to read — so stripping
/// only `Authorization` would leave both of Akahu's behind.
const CREDENTIAL_HEADERS: [&str; 3] = ["authorization", "x-akahu-id", "x-akahu-auth"];

/// Strips credentials at the snapshot boundary, and only there.
///
/// Two things this buys, and the second is the one that matters:
///
/// 1. **A snapshot recorded with one developer's token replays against a test that sends a
///    different one.** `redact_request_for_snapshot` runs on the record side *and* on the
///    replay-lookup side (`SPECIFICATION.md` §6.4, §8.2.1), so both keys are computed over the
///    same credential-free request and still agree. Without this, every recording would be
///    bound to the token that made it, and CI — which has no Akahu token at all — could never
///    replay one.
/// 2. **A committed snapshot must not carry a live token.** Rule 3 is about identifiers in
///    fixtures, and a bearer token in a checked-in `.ndjson` is the worst kind: it is valid,
///    it is someone's, and no length-for-length replacement makes it safe afterwards. The
///    strip happens before bytes reach the recorder, so the credential never lands on disk to
///    be noticed later.
///
/// `handle` is a pure pass-through: the *live* request still carries the real credentials,
/// because the upstream in `Mode::Record` will not answer without them.
pub struct RedactCredentials;

#[async_trait]
impl ProxyMiddleware for RedactCredentials {
    async fn handle(
        &self,
        req: ProxyRequest,
        ctx: &mut RequestContext,
        next: Next<'_>,
    ) -> partly_proxy_lib::Result<ProxyResponse> {
        next.run(req, ctx).await
    }

    fn redact_request_for_snapshot(&self, req: &mut ProxyRequest) {
        for header in CREDENTIAL_HEADERS {
            // `HeaderMap::remove` drops every value under the name, not just the first, so a
            // duplicated header cannot leave one copy behind.
            req.headers.remove(header);
        }
    }

    fn redact_response_for_snapshot(&self, resp: &mut ProxyResponse) {
        // A session cookie is a credential travelling the other way, and one nothing in `sure`
        // reads: no adapter here keeps a cookie jar. Persisting it would put a live token in a
        // fixture for no replay benefit at all.
        resp.headers.remove(SET_COOKIE);
    }
}

/// How to bring the cluster up.
pub struct ClusterConfig {
    pub mode: Mode,
    /// Directory holding `<upstream name>.ndjson`. `None` means stubs only — no snapshot is
    /// loaded and nothing is persisted.
    pub snapshot_dir: Option<PathBuf>,
    pub control_bind: SocketAddr,
}

impl Default for ClusterConfig {
    /// Replay, no snapshots, ephemeral control port — the configuration that cannot reach the
    /// internet.
    ///
    /// Hand-written rather than derived precisely because `Mode`'s own `Default` is
    /// `Mode::Record`: a derived `Default` here would make "I didn't think about the mode" mean
    /// "dial the real Akahu", which is the one outcome this crate exists to prevent.
    fn default() -> Self {
        Self {
            mode: Mode::Replay,
            snapshot_dir: None,
            control_bind: ephemeral(),
        }
    }
}

/// A running cluster and the addresses it landed on.
pub struct Started {
    pub cluster: ClusterHandle,
    pub control_addr: SocketAddr,
    /// Env var -> the URL `sure-server` should be given, e.g.
    /// `"YAHOO_FINANCE_BASE_URL"` -> `"http://127.0.0.1:54321/v8/finance/chart"`.
    ///
    /// The path prefix is already joined on, so a harness passes this straight through as
    /// environment and never has to know one.
    pub endpoints: BTreeMap<&'static str, String>,
}

/// Bind every [`Upstream::ALL`] entry on loopback, plus the control plane, and report the
/// environment that points `sure` at them.
///
/// Ports are all ephemeral, and every address comes back from `local_addr()` after the bind
/// (see [`ephemeral`]). Middleware per upstream is `[RedactCredentials, CanonicaliseQuery]`:
/// the two redactions are independent — one touches headers, the other the URI — so the order
/// is only about reading the list, not about correctness.
pub async fn start(config: &ClusterConfig) -> anyhow::Result<Started> {
    if config.mode == Mode::Record && config.snapshot_dir.is_none() {
        // Record with nowhere to write is the one combination that reaches a real bank or price
        // feed and keeps nothing to show for it. Legal — it is how you sanity-check that an
        // adapter still speaks the live API — but never what someone recording fixtures meant.
        tracing::warn!(
            "record mode with no snapshot directory: exchanges will hit the real upstreams and \
             be discarded"
        );
    }

    let mut builder = ProxyClusterBuilder::new()
        // Before any `add_upstream_with`: the builder stamps the current default mode and miss
        // handler onto each upstream as it is registered, so setting either afterwards would
        // silently apply to nothing.
        .default_mode(config.mode)
        // ...and this one has to come *after* it, because `default_mode` does not only set a
        // mode: it overwrites the recording config too (`Record` → enabled, `Replay` →
        // disabled — see `partly-proxy-lib`'s `builder.rs`). Left at that default, a replay
        // cluster records nothing, and `AssertSeen` / `AssertCount` / `QueryTraffic` — the
        // whole out-of-process assertion surface — can only ever answer zero.
        //
        // That default is right for the library's own framing, where `Replay` means "serve
        // from a snapshot and keep no second copy" (`SPECIFICATION.md` §8.3). It is wrong for
        // ours, where `Replay` is chosen for one property only — the upstream is never
        // dialled — and the traffic ring is how a test asks *what did the app send?*. From
        // TypeScript it is the only way to ask at all: the suites drive a spawned binary, so
        // every assertion they can make is one the control plane answers.
        //
        // Be exact about what the ring does and does not carry, because the obvious reading is
        // wrong. `build_recorded` (`listener.rs`) hands the recorder the request *after* every
        // middleware's `redact_request_for_snapshot` — so [`CanonicaliseQuery`] has already
        // rewritten this upstream's [`Upstream::volatile_query_params`] to [`CANONICAL`]. What
        // is assertable is therefore the method, the path, the count, and any query parameter
        // that is *not* clock-derived (Frankfurter's `?base=`); `?period1=`/`?period2=`/
        // `?start=` are not, in any configuration. `recording.rs` pins both halves of that, and
        // the width of Akahu's overlap window is pinned instead by `sure-providers`'
        // `tests/akahu.rs` against a cluster it builds without middleware — an option a spawned
        // binary does not have.
        //
        // The alternative was installing `CanonicaliseQuery` only when a snapshot directory is
        // attached, since with none there is no replay key for it to stabilise and it only costs
        // the ring's copy of the query. Not taken: it would make what the ring shows depend on
        // the configuration, so an assertion written against a real epoch here would start
        // failing the day someone commits a snapshot. Uniform middleware, and one true statement
        // about the ring, is worth more than two readable epochs.
        //
        // §8.1.1 names `Replay + recording` as a supported combination, so this is turning on
        // something the library offers rather than working around it.
        .recording(RecordingConfig::default())
        .on_replay_miss(|req: ProxyRequest| {
            // A missing fixture is the likeliest failure a test author will hit, and "503 from
            // somewhere" is not a diagnosis — the method and URI are. WARN because the default
            // `RUST_LOG` for the binary is `warn`: this line has to survive the quietest
            // configuration anyone will run.
            tracing::warn!(
                method = %req.method,
                uri = %req.uri,
                "replay miss: no stub and no snapshot matched, answering 503",
            );
            // Rebuilt byte-for-byte from `partly-proxy-lib`'s own default, whose constructor is
            // crate-private. Both halves are load-bearing: `sure`'s providers call
            // `error_for_status()`, so the 503 is what surfaces the failure as an adapter error
            // rather than a JSON decode error, and `{}` with `application/json` is what
            // `proxy_contract.rs` asserts the miss looks like.
            ProxyResponse::new(StatusCode::SERVICE_UNAVAILABLE)
                .with_header(CONTENT_TYPE, Bytes::from_static(b"application/json"))
                .with_body(Bytes::from_static(b"{}"))
        })
        .tcp_control_plane(config.control_bind);

    for upstream in Upstream::ALL {
        let storage = match &config.snapshot_dir {
            Some(dir) => snapshot_storage(config.mode, dir, upstream).await?,
            None => None,
        };
        builder = builder.add_upstream_with(
            upstream.name(),
            ProxyConfig::http(ephemeral(), UpstreamTarget::new(upstream.target_base_url())),
            vec![
                shared(RedactCredentials),
                shared(CanonicaliseQuery {
                    params: upstream.volatile_query_params(),
                }),
            ],
            storage,
        );
    }

    let cluster = builder.run().await?;

    let control_addr = cluster
        .tcp_control_addr()
        .context("the control plane was requested but reported no bound address")?;
    let mut endpoints = BTreeMap::new();
    for upstream in Upstream::ALL {
        let addr = cluster
            .addr(upstream.name())
            .with_context(|| format!("upstream {} bound no listener", upstream.name()))?;
        endpoints.insert(
            upstream.env_var(),
            format!("http://{addr}{}", upstream.path_prefix()),
        );
    }

    Ok(Started {
        cluster,
        control_addr,
        endpoints,
    })
}

/// Loopback, port zero.
///
/// Loopback rather than `0.0.0.0` because this process strips credentials and answers from a
/// fixture: on a shared network it is a machine happy to impersonate someone's bank. Port zero
/// because asking the OS for a free port and then binding it in a second step is a race that
/// buys nothing here — `run()` reports `local_addr()`, so the address that comes back is the
/// one actually bound.
fn ephemeral() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// Resolve one upstream's NDJSON snapshot file into a storage backend, if it should have one.
///
/// The mode decides who may create a file, and it matters because `snapshot_dir` is a
/// *committed fixture directory*:
///
/// - [`Mode::Record`] is going to write, so the directory is ours to create and
///   `JsonlStorage::open`'s `create(true)` is what we want. Attaching the same file it will
///   append to also makes it a dedup cache (`SPECIFICATION.md` §8.3): re-recording fetches
///   only what is not already captured.
/// - [`Mode::Replay`] must not leave anything behind. `open` would create an empty
///   `<name>.ndjson` for an upstream nobody has recorded yet, so a replay-only run would dirty
///   the working tree with three files it never read. Skipping the attach is behaviourally
///   identical — no snapshot means every request falls through to the miss handler — and the
///   log line below says which upstream is unfixtured *before* the first 503 rather than after.
///
/// When the file *does* exist, a replay run reads it into an [`InMemoryStorage`] rather than
/// attaching the `JsonlStorage` itself. One backend serves both directions — it is the replay
/// source *and* the recording sink (`SPECIFICATION.md` §8) — and [`start`] now leaves the
/// recorder enabled in replay mode, so attaching the file would append every stub-served
/// exchange and every 503 miss straight into the committed fixture. Nothing but `Mode::Record`
/// should ever write one of these files, and copying the contents in is how that is enforced
/// rather than merely intended.
async fn snapshot_storage(
    mode: Mode,
    dir: &Path,
    upstream: Upstream,
) -> anyhow::Result<Option<SharedStorage>> {
    let path = dir.join(format!("{}.ndjson", upstream.name()));
    match mode {
        Mode::Record => {
            // `JsonlStorage::open` creates the file but not its parents.
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("create the snapshot directory {}", dir.display()))?;
            let storage = partly_proxy_lib::jsonl::JsonlStorage::open(&path)
                .await
                .with_context(|| format!("open the snapshot backend {}", path.display()))?;
            Ok(Some(Arc::new(storage)))
        }
        Mode::Replay => {
            if !path.exists() {
                tracing::warn!(
                    upstream = upstream.name(),
                    path = %path.display(),
                    "no snapshot file: every request to this upstream will be a replay miss",
                );
                return Ok(None);
            }
            Ok(Some(Arc::new(InMemoryStorage::from(
                read_snapshot(&path).await?,
            ))))
        }
    }
}

/// Read a committed snapshot file into memory, failing on the first unparseable line.
///
/// `SnapshotStorage::load` streams, precisely so that peak memory stays bounded by the largest
/// single exchange rather than by the file (`SPECIFICATION.md` §8.1.1) — and this throws that
/// away by reading the whole thing. Deliberately: draining that stream needs a `StreamExt` in
/// scope, which would mean a `futures` dependency for this one call, and these files are
/// committed fixtures for three JSON APIs whose entire payloads are a couple of kilobytes each.
/// If a snapshot here ever grows to the 10k–100k exchanges §8.1.1 is written for, take the
/// dependency and stream it; the reason for the shortcut will have expired.
///
/// A malformed line is fatal rather than skipped. A fixture is either the traffic it claims to
/// be or it is not, and silently dropping an exchange would surface as a replay miss — a 503
/// the adapter reports as an upstream failure, which is a long way from "line 14 is truncated".
async fn read_snapshot(path: &Path) -> anyhow::Result<Vec<RecordedExchange>> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read the snapshot {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            // 1-based, to match what an editor shows and what the error below quotes.
            let lineno = index + 1;
            partly_proxy_lib::jsonl::parse_ndjson_line(line, lineno)
                .with_context(|| format!("parse {}:{lineno}", path.display()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production defaults, as each `DEFAULT_BASE_URL` in `sure-providers` spells them.
    ///
    /// This crate cannot depend on `sure-providers` — `sure-providers` dev-depends on *it*, and
    /// this one has to keep depending on nothing in the workspace — so the single mapping rule 1
    /// asks for is unavailable across that edge and a copy is the best shape left. Be clear
    /// about which half of the risk the test below covers: it catches [`Upstream`]'s two halves
    /// drifting apart from each other, and cannot catch a real endpoint moving, because that
    /// changes a const this crate does not import.
    ///
    /// `sure-providers`' `tests/proxy_contract.rs` closes that from the side that can see both,
    /// and it is the check that actually keeps a recording and a live request on one host.
    const PRODUCTION_DEFAULTS: [(Upstream, &str); 3] = [
        (Upstream::Frankfurter, "https://api.frankfurter.dev/v1"),
        (
            Upstream::YahooFinance,
            "https://query1.finance.yahoo.com/v8/finance/chart",
        ),
        (Upstream::Akahu, "https://api.akahu.io/v1"),
    ];

    #[test]
    fn target_plus_prefix_reproduces_the_production_default() {
        for (upstream, expected) in PRODUCTION_DEFAULTS {
            let rejoined = format!("{}{}", upstream.target_base_url(), upstream.path_prefix());
            assert_eq!(
                rejoined,
                expected,
                "{}'s split base URL no longer rebuilds the endpoint sure-providers uses",
                upstream.name()
            );
        }
    }

    #[test]
    fn all_names_are_distinct() {
        // A duplicated name is a copy-paste the compiler cannot see, and it fails late and
        // opaquely: `ProxyClusterBuilder::run` rejects the whole cluster with "duplicate
        // upstream name", and one upstream's snapshot file would otherwise be the other's.
        let mut names: Vec<&str> = Upstream::ALL.iter().map(|up| up.name()).collect();
        names.sort_unstable();
        let distinct = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            distinct,
            "two upstreams share a name: {names:?}"
        );
    }

    #[test]
    fn every_name_round_trips_through_from_name() {
        for upstream in Upstream::ALL {
            assert_eq!(Upstream::from_name(upstream.name()), Some(upstream));
        }
        assert_eq!(Upstream::from_name("yahoo"), None, "no prefix matching");
        assert_eq!(Upstream::from_name(""), None);
    }

    /// The determinism invariant of §6.4, at the level a socket cannot reach: same inputs
    /// modulo the clock and the parameter order, byte-identical key.
    #[test]
    fn canonicalising_erases_the_clock_and_the_parameter_order() {
        let canonicalise = |uri: &str| {
            let mut req = ProxyRequest::new(
                http::Method::GET,
                uri.parse().expect("test URI parses"),
                http::HeaderMap::new(),
                Bytes::new(),
            );
            CanonicaliseQuery {
                params: Upstream::YahooFinance.volatile_query_params(),
            }
            .redact_request_for_snapshot(&mut req);
            req.uri.to_string()
        };

        let recorded = canonicalise(
            "/v8/finance/chart/AAPL?period1=1700000000&period2=1700086400&interval=1d",
        );
        let replayed = canonicalise(
            "/v8/finance/chart/AAPL?interval=1d&period2=1999999999&period1=1888888888",
        );
        assert_eq!(recorded, replayed);
        assert_eq!(
            recorded, "/v8/finance/chart/AAPL?interval=1d&period1=CANONICAL&period2=CANONICAL",
            "the surviving parameters must keep their real values",
        );
        // A parameter this upstream does not declare volatile is left exactly as it came, or
        // the key would stop distinguishing two genuinely different requests.
        assert_eq!(canonicalise("/v1/latest?base=NZD"), "/v1/latest?base=NZD");
    }

    #[test]
    fn redaction_strips_every_credential_header_and_nothing_else() {
        let mut req = ProxyRequest::new(
            http::Method::GET,
            "/v1/accounts".parse().expect("test URI parses"),
            http::HeaderMap::new(),
            Bytes::new(),
        );
        // Invented tokens, shaped like the real ones (CLAUDE.md rule 3).
        req.headers
            .insert("authorization", "Bearer user_tok_0000".parse().unwrap());
        req.headers
            .insert("x-akahu-id", "app_token_0000".parse().unwrap());
        req.headers
            .insert("x-akahu-auth", "user_token_0000".parse().unwrap());
        req.headers
            .insert("accept", "application/json".parse().unwrap());

        RedactCredentials.redact_request_for_snapshot(&mut req);

        for header in CREDENTIAL_HEADERS {
            assert!(
                req.headers.get(header).is_none(),
                "{header} would have been written to the snapshot",
            );
        }
        // Non-credential headers stay: they are part of what the recording documents about how
        // `sure` calls the API.
        assert_eq!(
            req.headers.get("accept").and_then(|v| v.to_str().ok()),
            Some("application/json"),
        );
    }

    #[test]
    fn response_redaction_drops_set_cookie() {
        let mut resp = ProxyResponse::new(StatusCode::OK)
            .with_header(SET_COOKIE, Bytes::from_static(b"sid=0000"))
            .with_header(CONTENT_TYPE, Bytes::from_static(b"application/json"));
        RedactCredentials.redact_response_for_snapshot(&mut resp);
        assert!(resp.headers.get(SET_COOKIE).is_none());
        assert!(resp.headers.get(CONTENT_TYPE).is_some());
    }

    /// The end-to-end shape of what a harness gets, and the guarantee underneath it.
    ///
    /// Replay with no snapshot directory at all, which is the strongest form of the promise: no
    /// storage is attached, so there is nothing an answer *could* come from except a stub, and
    /// the upstreams named in `target_base_url` are real hosts that must not be dialled. A 503
    /// with `{}` is the proof that the request stopped here.
    #[tokio::test]
    async fn a_replay_cluster_answers_every_endpoint_with_the_miss_response() {
        let started = start(&ClusterConfig::default())
            .await
            .expect("bind the cluster");

        assert_eq!(started.endpoints.len(), Upstream::ALL.len());
        for upstream in Upstream::ALL {
            let url = started
                .endpoints
                .get(upstream.env_var())
                .expect("every upstream contributes an endpoint");
            let port = started
                .cluster
                .addr(upstream.name())
                .expect("bound listener")
                .port();
            assert_eq!(
                url,
                &format!("http://127.0.0.1:{port}{}", upstream.path_prefix()),
                "the endpoint must carry the path prefix, so no caller has to know one",
            );

            let miss = reqwest::get(format!("{url}/anything?base=NZD"))
                .await
                .expect("the request reaches the proxy");
            assert_eq!(
                miss.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{} answered something other than a replay miss",
                upstream.name(),
            );
            assert_eq!(miss.text().await.expect("read the miss body"), "{}");
        }

        started.cluster.shutdown().await.expect("cluster stops");
    }

    /// Record mode is the only mode allowed to create anything, and replay must leave a fixture
    /// directory exactly as it found it — otherwise every `pnpm test:api` run shows up as three
    /// untracked files.
    #[tokio::test]
    async fn replay_creates_no_snapshot_files() {
        let dir = tempfile::tempdir().expect("create a temp snapshot dir");
        let started = start(&ClusterConfig {
            mode: Mode::Replay,
            snapshot_dir: Some(dir.path().to_path_buf()),
            ..ClusterConfig::default()
        })
        .await
        .expect("bind the cluster");
        started.cluster.shutdown().await.expect("cluster stops");

        let mut entries = std::fs::read_dir(dir.path()).expect("read the snapshot dir");
        assert!(
            entries.next().is_none(),
            "replay wrote into a directory it was only supposed to read",
        );
    }
}
