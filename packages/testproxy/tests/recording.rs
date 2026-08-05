//! What a *replay* cluster records, and what it must not write.
//!
//! Both properties come from one decision in [`sure_testproxy::start`]: it re-enables the
//! traffic ring that `ProxyClusterBuilder::default_mode(Mode::Replay)` turns off. Neither is
//! visible from the Rust fixtures in `sure-providers` — those read
//! `ClusterHandle::recorder()` on clusters they build inline — so without this file the
//! regression surfaces a long way from its cause: every `assertCount` / `queryTraffic` in the
//! Playwright suites quietly answering zero, which reads as "the app never made the call" rather
//! than "nothing was recorded".
//!
//! The second property is the corollary of the first. A snapshot backend is both the replay
//! source and the recording sink (`SPECIFICATION.md` §8), so recording plus an attached
//! `JsonlStorage` would mean a replay run appending every stub-served exchange and every 503
//! miss into a committed fixture — growing it a little on each CI run, and answering next
//! week's requests with last week's mistakes.

use std::net::Ipv4Addr;
use std::net::SocketAddr;

use bytes::Bytes;
use http::{Method, StatusCode};
use partly_proxy_lib::{
    ClusterHandle, Command, CommandResponse, Mode, RequestMatcher, StubbedResponse, TrafficFilter,
};
use sure_testproxy::{ClusterConfig, Started, Upstream};

fn ephemeral() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}

/// A rate table, in the shape Frankfurter sends one.
const RATES: &str = r#"{"amount":1.0,"base":"NZD","date":"2026-08-03","rates":{"AUD":0.92413}}"#;

/// Answer `GET /v1/latest` on the Frankfurter listener, whatever its query.
///
/// A matcher is never shown a query string (`SPECIFICATION.md` §7.1), which is the same reason
/// the recorded URI below is the only place `?base=` can be asserted at all.
async fn stub_latest(cluster: &ClusterHandle) {
    stub(
        cluster,
        Upstream::Frankfurter,
        r"^/v1/latest$",
        Bytes::from_static(RATES.as_bytes()),
    )
    .await;
}

/// Register a 200 JSON stub on one upstream's listener.
async fn stub(cluster: &ClusterHandle, upstream: Upstream, path: &str, body: Bytes) {
    let response = cluster
        .command_sender()
        .send(Command::Stub {
            upstream: Some(upstream.name().to_string()),
            matcher: RequestMatcher::new().method(Method::GET).path(path),
            response: StubbedResponse::new(StatusCode::OK)
                .header("content-type", "application/json")
                .body(body),
            times: None,
        })
        .await
        .expect("the command plane accepts a stub");
    assert!(
        matches!(response, CommandResponse::Ok),
        "registering a stub: {response:?}"
    );
}

async fn query_traffic(cluster: &ClusterHandle, filter: TrafficFilter) -> Vec<String> {
    let response = cluster
        .command_sender()
        .send(Command::QueryTraffic { filter })
        .await
        .expect("the command plane answers a traffic query");
    // A `let … else` over a borrow, not a `match` with a `_ =>` arm: `CommandResponse` is
    // `#[non_exhaustive]` upstream, so a wildcard would be rule 2's legitimate escape hatch and
    // would need the `#[allow]` — this shape needs no exemption and still names the variant that
    // arrived instead.
    let CommandResponse::Exchanges(exchanges) = &response else {
        panic!("expected recorded exchanges, got {response:?}");
    };
    exchanges
        .iter()
        .map(|exchange| exchange.request.uri.clone())
        .collect()
}

/// The base URL a provider adapter would be handed, prefix already joined on.
fn base_url(started: &Started, upstream: Upstream) -> &str {
    started
        .endpoints
        .get(upstream.env_var())
        .expect("the handshake carries every upstream's base URL")
}

/// Replay mode records at all, and a stable query parameter survives into the ring.
///
/// The recording itself is what the Playwright suites' every `assertCount` / `queryTraffic` rests
/// on; without it they answer zero and read as "the app never called".
/// Frankfurter is the upstream to prove it on because its
/// [`Upstream::volatile_query_params`] is empty, so `?base=` reaches the recorder untouched and
/// the assertion below is on a real value rather than a placeholder.
///
/// That is also this test's limit, and why the one after it exists: for the two feeds that put
/// a clock reading in the query, the ring does *not* carry it.
#[tokio::test]
async fn a_replay_cluster_records_the_query_the_app_sent() {
    let started = sure_testproxy::start(&ClusterConfig {
        mode: Mode::Replay,
        snapshot_dir: None,
        control_bind: ephemeral(),
    })
    .await
    .expect("bind a replay cluster");

    stub_latest(&started.cluster).await;

    let base = base_url(&started, Upstream::Frankfurter);
    let answered = reqwest::get(format!("{base}/latest?base=NZD"))
        .await
        .expect("the request reaches the proxy");
    assert_eq!(answered.status(), StatusCode::OK);

    let uris = query_traffic(
        &started.cluster,
        TrafficFilter::new().upstream(Upstream::Frankfurter.name()),
    )
    .await;
    assert_eq!(
        uris,
        vec!["/v1/latest?base=NZD".to_string()],
        "a replay cluster must record what the app sent, query included"
    );

    // Scoping works, which is what lets one worker's proxy serve assertions per upstream: the
    // ring is cluster-wide, and the filter is the only thing separating three feeds in it.
    assert!(
        query_traffic(
            &started.cluster,
            TrafficFilter::new().upstream(Upstream::Akahu.name()),
        )
        .await
        .is_empty(),
        "an upstream nothing touched must have no traffic"
    );

    started.cluster.shutdown().await.expect("cluster stops");
}

/// A clock reading in the query does **not** reach the ring — it is [`sure_testproxy::CANONICAL`] by the time the
/// recorder sees it.
///
/// This is the half of the contract that is easy to assume the other way round, and assuming it
/// costs real time: `partly-proxy-lib` hands the recorder the request *after* every middleware's
/// `redact_request_for_snapshot` (`build_recorded`, its `listener.rs`), and
/// [`sure_testproxy::start`] installs [`sure_testproxy::CanonicaliseQuery`] on every upstream. So Yahoo's
/// `?period1=`/`?period2=` and Akahu's `?start=` are unreadable from any test driving a spawned
/// `sure-testproxy`, however the recording is configured. Two consequences worth having pinned:
///
/// - `stock-prices.spec.ts` reads the backfill epochs off the *backend's* log instead, and
///   `akahu.spec.ts` asserts only that `?start=` is present on a re-sync and absent on a first
///   one. Those are workarounds for this line, not oversights.
/// - The widths themselves — Yahoo's padded window, Akahu's three-day overlap — are pinned in
///   `sure-providers`' `tests/{yahoo_finance,akahu}.rs`, against clusters those tests build
///   without middleware. A spawned binary cannot do that, which is what makes the split
///   necessary rather than untidy.
///
/// If someone later makes canonicalisation conditional on an attached snapshot directory, this
/// test is what fails and says so — and the two specs above become tightenable.
#[tokio::test]
async fn a_clock_reading_in_the_query_never_reaches_the_ring() {
    let started = sure_testproxy::start(&ClusterConfig::default())
        .await
        .expect("bind a replay cluster");

    stub(
        &started.cluster,
        Upstream::YahooFinance,
        r"^/v8/finance/chart/VOO$",
        Bytes::from_static(b"{}"),
    )
    .await;

    // The shape `yahoo_finance.rs` builds: two epochs and one fixed parameter.
    let base = base_url(&started, Upstream::YahooFinance);
    assert_eq!(
        reqwest::get(format!(
            "{base}/VOO?period1=1782864000&period2=1783900800&interval=1d"
        ))
        .await
        .expect("the request reaches the proxy")
        .status(),
        StatusCode::OK
    );

    let uris = query_traffic(
        &started.cluster,
        TrafficFilter::new().upstream(Upstream::YahooFinance.name()),
    )
    .await;
    assert_eq!(
        uris,
        // Sorted, because `CanonicaliseQuery` orders the surviving pairs; `interval` keeps its
        // real value, which is what says the substitution is targeted and not a blanket wipe.
        vec!["/v8/finance/chart/VOO?interval=1d&period1=CANONICAL&period2=CANONICAL".to_string()],
        "the recorder must see the canonicalised query, so no test can read the real epochs"
    );

    started.cluster.shutdown().await.expect("cluster stops");
}

/// A replay run leaves the file it read byte-for-byte alone.
///
/// Recorded first through a `Mode::Record` cluster whose only request is stubbed, so this test
/// never dials `api.frankfurter.dev` — and so the file under test is one the real recording
/// path produced rather than a hand-written approximation of it.
#[tokio::test]
async fn a_replay_run_never_writes_to_the_snapshot_it_reads() {
    let dir = tempfile::tempdir().expect("create a snapshot directory");

    let recording = sure_testproxy::start(&ClusterConfig {
        mode: Mode::Record,
        snapshot_dir: Some(dir.path().to_path_buf()),
        control_bind: ephemeral(),
    })
    .await
    .expect("bind a recording cluster");
    stub_latest(&recording.cluster).await;
    let record_base = base_url(&recording, Upstream::Frankfurter).to_string();
    assert_eq!(
        reqwest::get(format!("{record_base}/latest?base=NZD"))
            .await
            .expect("the recording request reaches the proxy")
            .status(),
        StatusCode::OK
    );
    // Flushes the NDJSON backend, so what is on disk below is complete.
    recording.cluster.shutdown().await.expect("cluster stops");

    let snapshot = dir
        .path()
        .join(format!("{}.ndjson", Upstream::Frankfurter.name()));
    let before = std::fs::read(&snapshot).expect("the recording pass wrote a snapshot");
    assert!(!before.is_empty(), "the snapshot should carry one exchange");

    let replaying = sure_testproxy::start(&ClusterConfig {
        mode: Mode::Replay,
        snapshot_dir: Some(dir.path().to_path_buf()),
        control_bind: ephemeral(),
    })
    .await
    .expect("bind a replay cluster");
    let replay_base = base_url(&replaying, Upstream::Frankfurter).to_string();

    // A hit: served from the snapshot, and read back through `read_snapshot`'s parse.
    let hit = reqwest::get(format!("{replay_base}/latest?base=NZD"))
        .await
        .expect("the replayed request reaches the proxy");
    assert_eq!(hit.status(), StatusCode::OK);
    assert_eq!(hit.text().await.expect("read the replayed body"), RATES);

    // And a miss, which is the exchange that would actually corrupt the file: a 503 the
    // recorder sees as an ordinary outcome, with a request nobody recorded.
    assert_eq!(
        reqwest::get(format!("{replay_base}/latest?base=CHF"))
            .await
            .expect("the miss request reaches the proxy")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    // And here is the asymmetry, which is worth knowing before it is discovered the hard way: a
    // request answered *from a snapshot* is deliberately not recorded, because the exchange is
    // already on record and re-recording it would multiply entries for one request
    // (`listener.rs`'s `record_success_exchange` returns early on `ResponseSource::Snapshot`;
    // §8.3, §20.1). A stub-served exchange, by contrast, *is* recorded — which is what
    // `sure-providers`' `proxy_contract.rs` relies on to materialise a hand-written fixture into
    // a snapshot file.
    //
    // So the ring holds the miss and not the hit. Consequence for whoever adds committed
    // snapshots to the Playwright suite: `assertCount` will not see a call that a snapshot
    // answered. Today every fixture there is a stub, so every call is visible; a spec
    // that replays from a file and then counts upstream calls would need to assert on the
    // observable result instead.
    let recorded = query_traffic(
        &replaying.cluster,
        TrafficFilter::new().upstream(Upstream::Frankfurter.name()),
    )
    .await;
    assert_eq!(
        recorded,
        vec!["/v1/latest?base=CHF".to_string()],
        "only the replay miss should be in the ring; the snapshot hit is already on record"
    );

    replaying.cluster.shutdown().await.expect("cluster stops");

    assert_eq!(
        std::fs::read(&snapshot).expect("the snapshot still exists"),
        before,
        "a replay run must not append to the fixture it replays from"
    );
}
