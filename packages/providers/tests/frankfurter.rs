//! `FrankfurterProvider`'s fetch path — the half of the adapter no unit test can reach.
//!
//! Two cheaper tiers run first and nothing here repeats them: `frankfurter-client` unit-tests the
//! wire format (`parses_a_typical_response`, `decodes_the_body_bytes_the_capped_reader_
//! accumulates`), and `src/frankfurter.rs` unit-tests the mapping onto a quote. What is left once
//! both have run is everything between `fetch_rates` being called and `parse_quotes` being handed
//! a `LatestRates`: the URL the request is built from, the client it goes out on, and the client
//! reading the body back off the socket. All three were previously exercisable only against the
//! live API.
//!
//! The first test drives `sure_testproxy::start()` rather than a hand-built cluster, because the
//! wiring is itself worth exercising: `start()` binds every upstream with the production
//! middleware, joins `Upstream::path_prefix` onto each address, and hands back the
//! `FRANKFURTER_BASE_URL` a real `sure-server` would be given — so the URL under test is assembled
//! the same way here as in the Playwright suites. Its `Mode::Replay` default is also the strongest
//! form of the no-internet guarantee: the configured target is the real
//! `https://api.frankfurter.dev`, and Replay never dials it, stub or no stub.
//!
//! The other two build a cluster inline, for two different reasons, and both are worth knowing
//! before writing another fixture:
//!
//! - **No middleware.** Not the recorder: `ProxyClusterBuilder::default_mode(Mode::Replay)` does
//!   swap the recording config for `RecordingConfig::disabled()`, but `start()` sets
//!   `.recording(..)` back afterwards — deliberately, since the Playwright suites have no other
//!   way to ask what the app sent — so a `start()` cluster keeps its traffic and a test wanting a
//!   recorder no longer has to build one. What an inline `add_stub` cluster still buys
//!   [`the_fetch_asks_latest_for_the_base_it_was_given`] is that it carries no middleware, so the
//!   recorded URI is the request exactly as it arrived rather than the canonicalised copy
//!   `start()` would record. For Frankfurter alone those two are the same string; that test says
//!   why it does not rely on the coincidence.
//! - **Record mode.** The round-trip test needs `Mode::Record` to write a snapshot, and in Record
//!   mode a stub that failed to match falls through to whatever the target names — which under
//!   `start()` is the real API. So it names `UpstreamTarget::new("http://unreachable.invalid")`, as
//!   `proxy_contract.rs` does, and a missed stub is a connection error instead of live traffic.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use frankfurter_client::FrankfurterError;
use http::{Method, StatusCode};
use partly_proxy_lib::{
    ClusterHandle, Command, Mode, ProxyClusterBuilder, ProxyConfig, RequestMatcher,
    StubbedResponse, UpstreamTarget, shared,
};
use sure_app::ports::{ExchangeRateProvider, ExchangeRateQuote};
use sure_providers::{Endpoint, FrankfurterProvider, Pacing};
use sure_testproxy::{CanonicaliseQuery, ClusterConfig, RedactCredentials, Started, Upstream};

mod common;
use common::ephemeral;

/// A rate table shaped like Frankfurter's answer to `?base=NZD`: ECB reference rates, one per
/// currency, quoted per unit of the base.
///
/// The values are chosen for their *binary* expansions, because that is what the fetch has to
/// carry intact. `parse_quotes` uses `Decimal::from_f64` — deliberately, not `from_f64_retain` — so
/// each rate comes back as the shortest decimal that round-trips to the same `f64`. The nearest
/// `f64` to `88.213` is `88.21299999999999386091…` and to `0.60147` is `0.60146999999999994912…`,
/// so a quote that survives as `88.213` is evidence of that choice. A fixture of values like `1.5`
/// would be evidence of nothing: they are exactly representable and both functions agree on them.
///
/// Invented, and byte-grepped against `data/sure.db` to prove it (rule 3): all four are absent.
/// The first choice for the JPY leg, `88.407`, was *not* — it collides with the prefix of a public
/// AAPL close already in `stock_prices`, which is harmless but makes the next person re-derive that
/// conclusion. A value with no hit at all costs nothing and says what it means.
const RATE_TABLE: &str = r#"{"amount":1.0,"base":"NZD","date":"2026-07-16","rates":{"AUD":0.92413,"EUR":0.51203,"JPY":88.213,"USD":0.60147}}"#;

/// [`RATE_TABLE`] as the adapter should hand it back, in the order `parse_quotes`' `BTreeMap`
/// iterates.
const EXPECTED: [&str; 4] = [
    "AUD=0.92413@2026-07-16",
    "EUR=0.51203@2026-07-16",
    "JPY=88.213@2026-07-16",
    "USD=0.60147@2026-07-16",
];

/// One line per quote, so a failure prints the three fields that matter side by side instead of a
/// `Decimal`'s `Debug` and a diff nobody can read.
fn summarise(quotes: &[ExchangeRateQuote]) -> Vec<String> {
    quotes
        .iter()
        .map(|quote| format!("{}={}@{}", quote.quote_code, quote.rate, quote.as_of))
        .collect()
}

/// Answer `GET /v1/latest` on the Frankfurter listener with `body`, for as many requests as arrive.
///
/// A matcher sees `uri.path()` and never the query (`SPECIFICATION.md` §7.1), which is what makes
/// one stub enough to serve two different `?base=` values below.
async fn stub_rate_table(cluster: &ClusterHandle, body: &'static str) {
    cluster
        .command_sender()
        .send(Command::Stub {
            upstream: Some(Upstream::Frankfurter.name().to_owned()),
            matcher: RequestMatcher::new()
                .method(Method::GET)
                .path(format!(r"^{}/latest$", Upstream::Frankfurter.path_prefix())),
            response: StubbedResponse::new(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Bytes::from_static(body.as_bytes())),
            times: None,
        })
        .await
        .expect("register the rate-table stub");
}

/// The adapter, pointed at a `start()` cluster through exactly the environment value
/// `sure-server` reads — path prefix already joined on, so nothing here has to know one.
fn frankfurter_from(started: &Started) -> FrankfurterProvider {
    let url = started
        .endpoints
        .get(Upstream::Frankfurter.env_var())
        .expect("the cluster reports a Frankfurter endpoint");
    FrankfurterProvider::with_endpoint(
        Endpoint::parse(url).expect("a loopback proxy URL is plaintext-legal"),
        // Not what this file is about, and every request here is to a proxy on this machine.
        Pacing::unpaced(),
    )
}

/// The same, for the hand-built single-listener cluster the recording test uses.
fn frankfurter_at(addr: SocketAddr) -> FrankfurterProvider {
    let url = format!("http://{addr}{}", Upstream::Frankfurter.path_prefix());
    FrankfurterProvider::with_endpoint(
        Endpoint::parse(&url).expect("a loopback proxy URL is plaintext-legal"),
        Pacing::unpaced(),
    )
}

/// The rate table is fetched, read back off the socket, and parsed — with the decimals intact.
///
/// The parse is unit-tested; the *fetch* is not, and it is where the bytes actually come from a
/// socket in chunks rather than from a `&str` literal. Asserting the rendered decimals at the end
/// of that path is what makes the `Decimal::from_f64` choice load-bearing rather than incidental:
/// swap it for `from_f64_retain` and every rate here arrives as its full binary expansion, which is
/// what a currency-conversion figure would then be computed from and displayed as.
#[tokio::test]
async fn a_fetched_rate_table_keeps_the_shortest_decimal_that_round_trips() {
    let started = sure_testproxy::start(&ClusterConfig::default())
        .await
        .expect("bind the replay cluster");
    stub_rate_table(&started.cluster, RATE_TABLE).await;

    let quotes = frankfurter_from(&started)
        .fetch_rates("NZD")
        .await
        .expect("the stubbed rate table is fetched and parsed");

    assert_eq!(
        summarise(&quotes),
        EXPECTED,
        "a quote must carry the code, the shortest round-tripping rate, and the upstream's own \
         reference date — not the date it was fetched",
    );

    started.cluster.shutdown().await.expect("cluster stops");
}

/// The request production actually makes, as the proxy saw it.
///
/// A refactor that reshaped the URL — `/latest/NZD`, `?from=NZD`, a trailing slash — keeps every
/// parsing test green and quietly invalidates every recorded snapshot, because the replay key is
/// `(method, path + query verbatim, sha256(body))`. Asserting the inbound origin-form URI is
/// asserting that key. The suite would not fail; it would go on replaying answers to requests
/// nothing makes any more, which is the one failure no later debugging recovers.
///
/// Two bases, not one, so the assertion covers the parameter being *threaded* rather than a
/// constant that happens to read `NZD`.
///
/// This cluster carries no middleware, so the recorded URI is the request exactly as it arrived on
/// the listener. For Frankfurter that is also the URI the replay key is computed over: it declares
/// no volatile query parameters, so `CanonicaliseQuery` rewrites nothing and the redacted and raw
/// strings are the same one. That equivalence is what lets this test assert the key without
/// standing up the redaction chain to do it.
#[tokio::test]
async fn the_fetch_asks_latest_for_the_base_it_was_given() {
    let cluster = ProxyClusterBuilder::new()
        .add_stub(Upstream::Frankfurter.name(), ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the frankfurter stub cluster");
    let addr = cluster
        .addr(Upstream::Frankfurter.name())
        .expect("frankfurter is bound");
    stub_rate_table(&cluster, RATE_TABLE).await;

    let provider = frankfurter_at(addr);
    for base in ["NZD", "AUD"] {
        provider
            .fetch_rates(base)
            .await
            .unwrap_or_else(|e| panic!("fetch for base {base}: {e:#}"));
    }

    // `listener.rs` awaits `record_success_exchange` before the response goes back to the client, so
    // once both calls have returned there is nothing left to wait for and no flake to sleep around.
    let exchanges = cluster.recorder().exchanges().await;
    assert_eq!(
        exchanges.len(),
        2,
        "one exchange per fetch and no others — a retry or a second request would be its own bug: \
         {exchanges:#?}"
    );
    let prefix = Upstream::Frankfurter.path_prefix();
    for exchange in &exchanges {
        assert_eq!(
            exchange.upstream.as_deref(),
            Some(Upstream::Frankfurter.name())
        );
        assert_eq!(exchange.request.method, "GET");
    }
    assert_eq!(
        exchanges
            .iter()
            .map(|exchange| exchange.request.uri.clone())
            .collect::<Vec<_>>(),
        [
            format!("{prefix}/latest?base=NZD"),
            format!("{prefix}/latest?base=AUD"),
        ],
        "the prefix itself comes from `Upstream::path_prefix`, which `proxy_contract.rs` pins \
         against the adapter's own default; what this asserts is the `/latest` path and the \
         `base` parameter `fetch_rates` builds on top of it",
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// Record a rate table to a snapshot file, then serve the same fetch from that file with the
/// upstream unreachable.
///
/// `proxy_contract.rs` proves the proxy can do this with a bare `reqwest::get`. What is left, and
/// what this covers, is that the *adapter* goes through it: the URL it builds in production is the
/// key it later looks up under, and the bytes that come back off disk survive the client's capped
/// read and `parse_quotes` unchanged. Frankfurter is the right adapter to prove the round-trip on
/// because its query carries no clock reading — nothing is canonicalised away, so a failure here is
/// about the round-trip and not about `CanonicaliseQuery`.
#[tokio::test]
async fn a_recorded_rate_table_replays_from_its_snapshot_file() {
    let dir = tempfile::tempdir().expect("create a temp dir for the snapshot");
    let path = dir
        .path()
        .join(format!("{}.ndjson", Upstream::Frankfurter.name()));

    // --- record -----------------------------------------------------------------------------
    let recording = frankfurter_cluster(Mode::Record, &path).await;
    let record_addr = recording
        .addr(Upstream::Frankfurter.name())
        .expect("frankfurter is bound");
    stub_rate_table(&recording, RATE_TABLE).await;

    let recorded = frankfurter_at(record_addr)
        .fetch_rates("NZD")
        .await
        .expect("the recording pass fetches through the proxy");
    assert_eq!(summarise(&recorded), EXPECTED);
    // Flushes the NDJSON backend, so the replay pass below reads a complete file.
    recording.shutdown().await.expect("recording cluster stops");

    // --- replay -----------------------------------------------------------------------------
    // No stubs this time: every byte the adapter sees below came off disk or from nowhere.
    let replaying = frankfurter_cluster(Mode::Replay, &path).await;
    let replay_addr = replaying
        .addr(Upstream::Frankfurter.name())
        .expect("frankfurter is bound");
    let provider = frankfurter_at(replay_addr);

    let replayed = provider
        .fetch_rates("NZD")
        .await
        .expect("the snapshot answers the fetch that wrote it");
    assert_eq!(
        summarise(&replayed),
        EXPECTED,
        "a replayed table must be indistinguishable from the recorded one, decimals included",
    );

    // The control: replay matches a *key*, it does not answer everything. `?base=AUD` was never
    // recorded, so it gets the replay-miss 503 — and because that is a 5xx, the client turns it
    // into a status error. A missing fixture therefore fails loudly instead of falling through to
    // the internet.
    //
    // Specifically a `503` with no `Retry-After` on it, which is why this is `Http` and not
    // `RateLimited`: the client only reads a `503` as a refusal-on-volume when the upstream names
    // how long to wait. A replay miss naming one would stand the adapter down for a minute and
    // make the *next* test in the file fail instead of this one.
    let miss = provider
        .fetch_rates("AUD")
        .await
        .expect_err("a base nobody recorded must not answer");
    // `if let` rather than a catch-all match: `FrankfurterError` is `#[non_exhaustive]`, so this
    // way there is no wildcard arm to justify (CLAUDE.md rule 2).
    let status = if let Some(FrankfurterError::Http { status, .. }) = miss.downcast_ref() {
        Some(*status)
    } else {
        None
    };
    assert_eq!(
        status,
        Some(503),
        "an unrecorded request must arrive as the replay-miss status: {miss:#}"
    );

    replaying.shutdown().await.expect("replay cluster stops");
}

/// One Frankfurter listener over `snapshot`, carrying the middleware `sure_testproxy::start`
/// attaches in production.
///
/// The middleware list is not decoration. `redact_request_for_snapshot` runs on the record side
/// *and* on the replay-lookup side, so a snapshot written under one list and read under another
/// simply misses; using the production pair is what makes this temp file the same kind of artefact
/// as a committed fixture rather than a lookalike.
///
/// The target is deliberately unresolvable — see this file's header for why that matters in
/// `Mode::Record` and nowhere else.
async fn frankfurter_cluster(mode: Mode, snapshot: &Path) -> ClusterHandle {
    let storage = Arc::new(
        partly_proxy_lib::jsonl::JsonlStorage::open(snapshot)
            .await
            .expect("open the NDJSON snapshot backend"),
    );
    ProxyClusterBuilder::new()
        .default_mode(mode)
        .add_upstream_with(
            Upstream::Frankfurter.name(),
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            vec![
                shared(RedactCredentials),
                shared(CanonicaliseQuery {
                    params: Upstream::Frankfurter.volatile_query_params(),
                }),
            ],
            Some(storage),
        )
        .run()
        .await
        .expect("bind the frankfurter cluster")
}
