//! The `partly-proxy-lib` behaviours every provider fixture in this crate is built on.
//!
//! Almost nothing here tests `sure-providers` (the last test is the exception, and says why).
//! It tests the *proxy*, because three of its properties are load-bearing for the fixtures and
//! none of them is guaranteed by a version number: the crate is pinned by git `rev` (see the
//! root manifest), so the only thing standing between a bumped rev and a fleet of
//! mysteriously-failing provider tests is a file that fails first and says which property went
//! away.
//!
//! The three:
//!
//! 1. **A volatile query string can be canonicalised into a stable replay key.** The
//!    replay index is keyed on `(method, path + query verbatim, sha256(body))`, and two of
//!    our three upstreams put a *clock reading* in the query — Yahoo sends
//!    `?period1=<epoch>&period2=<epoch>` derived from today's date
//!    (`yahoo_finance.rs`), and Akahu sends `?start=<rfc3339>` derived from the last
//!    successful sync (`akahu.rs`). Recorded verbatim, both snapshots stop matching the
//!    day after they are taken. `redact_request_for_snapshot` runs on the record side
//!    *and* on the replay-lookup side, so rewriting the URI there — and only there — is
//!    what makes a recording outlive the clock. If this breaks, record/replay is only
//!    usable for Frankfurter and everything else has to be stubbed.
//! 2. **Stubs fire in registration order, and `times` retires them.** This is the only
//!    way to give two *different* answers to two requests that a matcher cannot tell
//!    apart — which is every paginated fetch we have, because a matcher sees the path and
//!    never the query string, and Akahu's page cursor is a query parameter.
//! 3. **A stub-served exchange is recorded.** Which means a fixture can be *authored* as
//!    a stub and *materialised* into a snapshot file without a real upstream ever being
//!    contacted — the escape hatch that lets Akahu fixtures be hand-built (invented
//!    identifiers, CLAUDE.md rule 3) yet still exercise the replay path the other
//!    upstreams use.

use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use partly_proxy_lib::{
    Command, Mode, ProxyClusterBuilder, ProxyConfig, RequestMatcher, SharedMiddleware,
    SharedStorage, StubbedResponse, UpstreamTarget, shared,
};
// The middleware under test is `sure-testproxy`'s, not a copy of it: this file is what proves
// the implementation the Playwright suites and the in-process fixtures actually run.
use sure_testproxy::{CanonicaliseQuery, Upstream};

mod common;
use common::ephemeral;

fn canonicalising(params: &'static [&'static str]) -> Vec<SharedMiddleware> {
    vec![shared(CanonicaliseQuery { params })]
}

/// A snapshot file that outlives the cluster that wrote it.
fn snapshot_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create a temp dir for the snapshot")
}

async fn jsonl(path: &std::path::Path) -> SharedStorage {
    Arc::new(
        partly_proxy_lib::jsonl::JsonlStorage::open(path)
            .await
            .expect("open the NDJSON snapshot backend"),
    )
}

/// Yahoo's shape: a chart path per symbol, with the requested window in the query.
const CHART_PATH: &str = "/v8/finance/chart/AAPL";
const CHART_BODY: &str = r#"{"chart":{"result":[{"meta":{"symbol":"AAPL"}}]}}"#;

/// Record one exchange through a stub, then replay it against a *different* query.
///
/// The recording pass and the replay pass are separate clusters over the same NDJSON file,
/// which is what a real suite does: one developer records, CI replays.
#[tokio::test]
async fn a_canonicalised_query_replays_after_the_clock_moves() {
    let dir = snapshot_dir();
    let path = dir.path().join("yahoo.ndjson");

    // --- record ---------------------------------------------------------------------
    // `Mode::Record` with a stub standing in for Yahoo. The stub answers, and the
    // exchange is recorded anyway (property 3, asserted on its own below), so no live
    // upstream is contacted and the recording is still a legitimate snapshot.
    let recording = ProxyClusterBuilder::new()
        .add_upstream_with(
            "yahoo",
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            canonicalising(&["period1", "period2"]),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind the recording cluster");
    let record_addr = recording.addr("yahoo").expect("yahoo upstream is bound");

    recording
        .command_sender()
        .send(Command::Stub {
            upstream: Some("yahoo".into()),
            matcher: RequestMatcher::new()
                .method(Method::GET)
                .path(r"^/v8/finance/chart/AAPL$"),
            response: StubbedResponse::new(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Bytes::from_static(CHART_BODY.as_bytes())),
            times: None,
        })
        .await
        .expect("register the chart stub");

    let recorded = reqwest::get(format!(
        "http://{record_addr}{CHART_PATH}?period1=1700000000&period2=1700086400&interval=1d"
    ))
    .await
    .expect("the recording pass reaches the proxy");
    assert_eq!(recorded.status(), StatusCode::OK);
    assert_eq!(
        recorded.text().await.expect("read the recorded body"),
        CHART_BODY
    );

    // Flushes the NDJSON backend, so the replay pass below sees a complete file.
    recording.shutdown().await.expect("recording cluster stops");

    // --- replay ---------------------------------------------------------------------
    // No stubs this time, and `Mode::Replay` so the (invalid) upstream can never be
    // dialled: every 200 below came from the snapshot or from nowhere.
    let replaying = ProxyClusterBuilder::new()
        .default_mode(Mode::Replay)
        .add_upstream_with(
            "yahoo",
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            canonicalising(&["period1", "period2"]),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind the replay cluster");
    let replay_addr = replaying.addr("yahoo").expect("yahoo upstream is bound");

    // Different epochs, different order, same canonical key. This is the whole point: a
    // year from now `yahoo_finance.rs` will ask for a different window, and the snapshot
    // must still answer.
    let hit = reqwest::get(format!(
        "http://{replay_addr}{CHART_PATH}?interval=1d&period2=1999999999&period1=1888888888"
    ))
    .await
    .expect("the replay pass reaches the proxy");
    assert_eq!(
        hit.status(),
        StatusCode::OK,
        "a canonicalised query must replay against a snapshot taken with different epochs"
    );
    assert_eq!(
        hit.text().await.expect("read the replayed body"),
        CHART_BODY
    );

    // The control: replay is matching a *key*, not answering everything. A path that was
    // never recorded gets the replay-miss response — 503 with `{}` — which is what makes a
    // missing fixture fail loudly instead of silently reaching the internet.
    let miss = reqwest::get(format!(
        "http://{replay_addr}/v8/finance/chart/MSFT?interval=1d"
    ))
    .await
    .expect("the miss request reaches the proxy");
    assert_eq!(miss.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(miss.text().await.expect("read the miss body"), "{}");

    replaying.shutdown().await.expect("replay cluster stops");
}

/// The negative control for the test above, and the reason the middleware has to exist.
///
/// Same snapshot, same differing query, *no* canonicalisation — and the lookup misses. If
/// this test ever starts passing, the proxy has grown query normalisation of its own and
/// `CanonicaliseQuery` is dead weight worth deleting.
#[tokio::test]
async fn without_canonicalisation_a_moved_clock_misses_the_snapshot() {
    let dir = snapshot_dir();
    let path = dir.path().join("yahoo.ndjson");

    let recording = ProxyClusterBuilder::new()
        .add_upstream_with(
            "yahoo",
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            Vec::new(),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind the recording cluster");
    let record_addr = recording.addr("yahoo").expect("yahoo upstream is bound");

    recording
        .command_sender()
        .send(Command::Stub {
            upstream: Some("yahoo".into()),
            matcher: RequestMatcher::new().path(r"^/v8/finance/chart/AAPL$"),
            response: StubbedResponse::new(StatusCode::OK)
                .body(Bytes::from_static(CHART_BODY.as_bytes())),
            times: None,
        })
        .await
        .expect("register the chart stub");

    let recorded = reqwest::get(format!(
        "http://{record_addr}{CHART_PATH}?period1=1700000000"
    ))
    .await;
    assert_eq!(
        recorded.expect("recording request").status(),
        StatusCode::OK
    );
    recording.shutdown().await.expect("recording cluster stops");

    let replaying = ProxyClusterBuilder::new()
        .default_mode(Mode::Replay)
        .add_upstream_with(
            "yahoo",
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            Vec::new(),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind the replay cluster");
    let replay_addr = replaying.addr("yahoo").expect("yahoo upstream is bound");

    // The identical query still replays — proving the snapshot itself is sound and the
    // miss below is about the key, not about an empty file.
    let same = reqwest::get(format!(
        "http://{replay_addr}{CHART_PATH}?period1=1700000000"
    ))
    .await
    .expect("the identical-query request reaches the proxy");
    assert_eq!(same.status(), StatusCode::OK);

    let moved = reqwest::get(format!(
        "http://{replay_addr}{CHART_PATH}?period1=1799999999"
    ))
    .await
    .expect("the moved-clock request reaches the proxy");
    assert_eq!(
        moved.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the replay key compares the query verbatim, so an un-canonicalised epoch must miss"
    );

    replaying.shutdown().await.expect("replay cluster stops");
}

/// Two answers for two requests a matcher cannot distinguish.
///
/// Akahu paginates with a `cursor` query parameter, and a stub matcher only ever sees
/// `uri.path()` — so "page one, then page two" cannot be expressed as two matchers. It is
/// expressed as two *stubs*: insertion order decides which fires, and `times: Some(1)`
/// retires each one as it goes.
#[tokio::test]
async fn single_fire_stubs_answer_in_registration_order() {
    // `add_stub` is the upstream-less form: Replay mode with no snapshot, so anything the
    // stubs do not answer falls through to the replay-miss response.
    let cluster = ProxyClusterBuilder::new()
        .add_stub("akahu", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the stub cluster");
    let addr = cluster.addr("akahu").expect("akahu upstream is bound");

    for page in ["page-one", "page-two"] {
        cluster
            .command_sender()
            .send(Command::Stub {
                upstream: Some("akahu".into()),
                matcher: RequestMatcher::new()
                    .method(Method::GET)
                    .path(r"^/v1/accounts/acc_1/transactions$"),
                response: StubbedResponse::new(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(Bytes::from(format!(r#"{{"page":"{page}"}}"#))),
                times: Some(1),
            })
            .await
            .expect("register a page stub");
    }

    let url = format!("http://{addr}/v1/accounts/acc_1/transactions");
    // Note the differing cursors: irrelevant to the matcher, which is exactly why the
    // ordering property is the one being relied on.
    let first = reqwest::get(format!("{url}?cursor="))
        .await
        .expect("first page request")
        .text()
        .await
        .expect("first page body");
    let second = reqwest::get(format!("{url}?cursor=abc"))
        .await
        .expect("second page request")
        .text()
        .await
        .expect("second page body");

    assert_eq!(first, r#"{"page":"page-one"}"#);
    assert_eq!(
        second, r#"{"page":"page-two"}"#,
        "the first stub must retire after one fire so the second can answer"
    );

    // Both retired: a third call has nothing left to match.
    let exhausted = reqwest::get(&url).await.expect("third page request");
    assert_eq!(exhausted.status(), StatusCode::SERVICE_UNAVAILABLE);

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A stub-served exchange lands in the recording.
///
/// This is what makes hand-authored fixtures and recorded ones the same kind of thing: a
/// fixture written as a stub can be replayed from a snapshot file, so an Akahu fixture full
/// of invented identifiers exercises the same code path as a Frankfurter one recorded from
/// the real API. Without it, hand-built fixtures would have to be stubbed at every use and
/// could never be checked in as a snapshot.
#[tokio::test]
async fn a_stubbed_exchange_is_recorded_and_replayable() {
    let dir = snapshot_dir();
    let path = dir.path().join("akahu.ndjson");

    let recording = ProxyClusterBuilder::new()
        .add_upstream_with(
            "akahu",
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            Vec::new(),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind the recording cluster");
    let addr = recording.addr("akahu").expect("akahu upstream is bound");

    recording
        .command_sender()
        .send(Command::Stub {
            upstream: Some("akahu".into()),
            matcher: RequestMatcher::new().path(r"^/v1/accounts$"),
            response: StubbedResponse::new(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Bytes::from_static(br#"{"success":true,"items":[]}"#)),
            times: None,
        })
        .await
        .expect("register the accounts stub");

    assert_eq!(
        reqwest::get(format!("http://{addr}/v1/accounts"))
            .await
            .expect("accounts request")
            .status(),
        StatusCode::OK
    );
    recording.shutdown().await.expect("recording cluster stops");

    // The file is the proof: reload it as a replay-only cluster with no stubs at all.
    let replaying = ProxyClusterBuilder::new()
        .default_mode(Mode::Replay)
        .add_upstream_with(
            "akahu",
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            Vec::new(),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind the replay cluster");
    let replay_addr = replaying.addr("akahu").expect("akahu upstream is bound");

    let replayed = reqwest::get(format!("http://{replay_addr}/v1/accounts"))
        .await
        .expect("replayed accounts request");
    assert_eq!(
        replayed.status(),
        StatusCode::OK,
        "an exchange answered by a stub must still be written to the snapshot backend"
    );
    assert_eq!(
        replayed.text().await.expect("read the replayed body"),
        r#"{"success":true,"items":[]}"#
    );

    replaying.shutdown().await.expect("replay cluster stops");
}

/// The one test in this file about `sure-providers`: that every upstream the proxy stands in
/// for forwards to the host the adapter would have called on its own.
///
/// `Upstream::target_base_url` + `Upstream::path_prefix` is a second spelling of a
/// `DEFAULT_BASE_URL`, and has to be: `sure-testproxy` depends on nothing in this workspace —
/// this crate dev-depends on *it* — so the single mapping rule 1 asks for is unavailable across
/// that edge. The assertion has to live here because this is the only crate that can see both
/// consts. `sure-testproxy`'s own `target_plus_prefix_reproduces_the_production_default` pins
/// its two halves against each other and would stay green through the failure below.
///
/// That failure: an endpoint moves, `sure-providers` is updated, and the proxy keeps recording
/// from the old host — a snapshot that replays perfectly for requests nothing in production
/// makes any more. The suite stays green and stops meaning anything, which is the one outcome
/// no amount of later debugging recovers.
#[test]
fn every_upstream_forwards_to_the_endpoint_its_adapter_defaults_to() {
    let pairs = [
        (
            Upstream::Frankfurter,
            sure_providers::frankfurter::DEFAULT_BASE_URL,
        ),
        (
            Upstream::YahooFinance,
            sure_providers::yahoo_finance::DEFAULT_BASE_URL,
        ),
        (Upstream::Akahu, sure_providers::akahu::DEFAULT_BASE_URL),
    ];
    // A new upstream has to be paired here, not silently left uncovered — the compiler cannot
    // see that this array is meant to be exhaustive over `Upstream::ALL`.
    assert_eq!(
        pairs.len(),
        Upstream::ALL.len(),
        "an upstream was added without pairing it to the adapter default it proxies",
    );

    for (upstream, default) in pairs {
        assert_eq!(
            format!("{}{}", upstream.target_base_url(), upstream.path_prefix()),
            default,
            "sure-testproxy forwards {} somewhere sure-providers no longer asks",
            upstream.name(),
        );
    }
}
