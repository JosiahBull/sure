//! The bounds `sure_providers::http` puts on every outbound provider request, exercised through
//! a real adapter instead of asserted about a `ClientBuilder`.
//!
//! Until the proxy landed, the only part of that module a test could reach was `enforce_cap` — an
//! integer comparison. Everything else is a property of a built `reqwest::Client` that only shows
//! itself when the server on the other end does something hostile, and the only server available
//! was the live API. A stubbed proxy is a server that will misbehave to order: redirect, overrun
//! the byte ceiling, answer malformed JSON, fail outright, or go quiet.
//!
//! `FrankfurterProvider` is the vehicle here, not the subject. It is the thinnest adapter over
//! `http::client` + `http::json_capped` — one GET, no credentials, no pagination — so a failure
//! in this file indicts the transport rather than the adapter. What that now covers, and did not
//! until the workspace reached reqwest 0.13: `http::client` is the *only* builder in the crate, so
//! the client under test here is the same one `AkahuProvider` hands to `akahu-client` — the three
//! bounds no longer need pinning twice. (They used to: `akahu-client` held that path to a second
//! reqwest major, and its byte-identical builder was unreachable from here.) What is still
//! Akahu-only is what this adapter does not have — credentials, pagination, and a response body
//! read inside `akahu-client` rather than by `json_capped` — and that belongs to the Akahu
//! fixture, which already stands a cluster up.
//!
//! Every cluster below is an `add_stub` cluster: `Mode::Replay`, no snapshot, a dummy upstream
//! target. So even a stub that failed to match could not reach a host — the replay-miss 503 is
//! the floor, and no test here can touch the internet.
//!
//! One property comes along incidentally. The adapters are aimed at `http://127.0.0.1:<port>`
//! with no path at all, which is exactly the case `Endpoint::parse`'s doc comment calls out: it
//! keeps the caller's string verbatim rather than `Url`'s re-serialisation of it, so this does not
//! silently become `http://127.0.0.1:<port>//latest`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http::{Method, StatusCode};
use partly_proxy_lib::{
    ClusterHandle, Command, ProxyClusterBuilder, RecordedExchange, RequestMatcher, StubbedResponse,
};
use sure_app::ports::ExchangeRateProvider;
use sure_providers::{Endpoint, FrankfurterProvider};

mod common;
use common::ephemeral;

/// A well-formed answer, so "the adapter failed" is a statement about the misbehaviour under test
/// rather than about the fixture.
const RATE_TABLE: &str =
    r#"{"amount":1.0,"base":"NZD","date":"2026-07-16","rates":{"AUD":0.92413}}"#;

/// A Frankfurter adapter aimed at a proxy listener. The client is the production one — built by
/// `http::client` from the endpoint — differing only in where it points.
fn frankfurter_at(addr: SocketAddr) -> FrankfurterProvider {
    FrankfurterProvider::with_endpoint(
        Endpoint::parse(&format!("http://{addr}"))
            .expect("a loopback proxy URL is plaintext-legal"),
    )
}

/// Answer `GET /latest` on `upstream` with `response`, for as many requests as arrive.
///
/// One path for every test in this file, because `fetch_rates` only builds one:
/// `{base_url}/latest?base={code}`. A matcher never sees the query (`SPECIFICATION.md` §7.1), so
/// the path is the whole predicate.
async fn stub_latest(cluster: &ClusterHandle, upstream: &str, response: StubbedResponse) {
    cluster
        .command_sender()
        .send(Command::Stub {
            upstream: Some(upstream.to_owned()),
            matcher: RequestMatcher::new().method(Method::GET).path(r"^/latest$"),
            response,
            times: None,
        })
        .await
        .expect("register the /latest stub");
}

/// Every exchange the cluster recorded against `upstream`.
///
/// Read straight off the in-process recorder rather than sent as `Command::QueryTraffic`: the
/// command's reply is a `#[non_exhaustive]` enum, so a test would have to match it with a wildcard
/// arm to get at the same `Vec`. Recording is on by default, and `listener.rs` awaits
/// `record_success_exchange` *before* the response goes back to the client — so once an adapter
/// call has returned there is nothing left to wait for and no flake to sleep around.
async fn exchanges_for(cluster: &ClusterHandle, upstream: &str) -> Vec<RecordedExchange> {
    cluster
        .recorder()
        .exchanges()
        .await
        .into_iter()
        .filter(|exchange| exchange.upstream.as_deref() == Some(upstream))
        .collect()
}

/// A `Location` is a request this app never makes, and the host it names learns nothing.
///
/// This is `http::client`'s redirect rationale stated as a test rather than as a comment. reqwest
/// strips the four headers it recognises as credentials on a cross-host redirect and forwards
/// every custom one; Akahu's app token travels in `X-Akahu-Id`, a custom header. So
/// `Policy::none()` is the only thing between a compromised upstream's `Location:
/// http://elsewhere` and that token going out in the clear to whatever host it named. The empty
/// recording for `redirect_target` is the assertion with teeth — `redirect_target` is stubbed to
/// answer a *valid* rate table, so had the redirect been followed the adapter would have
/// succeeded with plausible-looking data and nothing would have looked wrong.
///
/// The second half is the mechanism, and it is not the one this used to claim: reqwest returns a
/// 3xx as-is under `Policy::none()` (tower-http's `Action::Stop`), and `error_for_status` fails
/// only on 4xx/5xx, so the redirect arrives as `Ok` and its HTML body dies in `json_capped`.
/// Which means a regression that swapped `Policy::none()` for `limited(1)` would not announce
/// itself as a status error at all — it would show up only as the leak above.
#[tokio::test]
async fn a_redirect_is_not_followed_and_the_location_host_is_never_contacted() {
    let cluster = ProxyClusterBuilder::new()
        .add_stub("frankfurter", ephemeral(), Vec::new())
        .add_stub("redirect_target", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the two-listener stub cluster");
    let feed = cluster.addr("frankfurter").expect("frankfurter is bound");
    let sink = cluster
        .addr("redirect_target")
        .expect("redirect_target is bound");

    stub_latest(
        &cluster,
        "redirect_target",
        StubbedResponse::new(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Bytes::from_static(RATE_TABLE.as_bytes())),
    )
    .await;
    stub_latest(
        &cluster,
        "frankfurter",
        StubbedResponse::new(StatusCode::FOUND)
            // Plaintext, cross-port: `https_only` is off for a loopback endpoint, so nothing but
            // the redirect policy stops this being followed.
            .header("location", format!("http://{sink}/latest?base=NZD"))
            .header("content-type", "text/html; charset=utf-8")
            .body(Bytes::from_static(b"<html><body>Moved</body></html>")),
    )
    .await;

    let err = frankfurter_at(feed)
        .fetch_rates("NZD")
        .await
        .expect_err("a 302 is not a rate table");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("decode the JSON body from"),
        "a 3xx reaches `json_capped` as ordinary body bytes; it must surface as the decode \
         failure that actually happens: {chain}"
    );
    assert!(
        err.downcast_ref::<reqwest::Error>().is_none(),
        "`error_for_status` returns Ok on 3xx, so no reqwest error should be in this chain: \
         {chain}"
    );

    assert_eq!(
        exchanges_for(&cluster, "frankfurter").await.len(),
        1,
        "the control: the recorder is on, and it did see the one request the adapter made"
    );
    let followed = exchanges_for(&cluster, "redirect_target").await;
    assert!(
        followed.is_empty(),
        "the host named in `Location` was contacted — an `X-Akahu-Id` would have gone with the \
         request: {followed:#?}"
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// The byte ceiling is a real limit on the wire, not just on the integer comparison `enforce_cap`
/// unit-tests.
///
/// `REQUEST_TIMEOUT` bounds how long an upstream may talk, not how much it may say: six seconds on
/// a gigabit link is ~750MB, and the contiguous copy a JSON decode makes roughly doubles the peak.
/// This is the only thing standing between a compromised or malfunctioning feed and this
/// process's memory.
///
/// `http::MAX_BODY_BYTES` is `pub(crate)`, so an integration test cannot import it and the const
/// below is unavoidably a second copy. Rather than leave the two to drift in silence, the
/// assertions read the ceiling back out of the error message: lower it in `http.rs` and the
/// message names a different number and this fails; raise it and the oversized body is accepted
/// and `expect_err` fails. Both directions are loud, which is the most a duplicated constant can
/// offer.
#[tokio::test]
async fn a_body_over_the_ceiling_is_refused_naming_the_host_and_the_size() {
    /// Must equal `sure_providers::http::MAX_BODY_BYTES` (8 MiB). See the doc comment above for
    /// what happens if it does not.
    const CEILING_BYTES: u64 = 8 * 1024 * 1024;

    let cluster = ProxyClusterBuilder::new()
        .add_stub("frankfurter", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the stub cluster");
    let feed = cluster.addr("frankfurter").expect("frankfurter is bound");

    stub_latest(
        &cluster,
        "frankfurter",
        StubbedResponse::new(StatusCode::OK)
            .header("content-type", "application/json")
            // Not JSON, and it does not need to be: the cap trips before a byte reaches serde.
            .body(Bytes::from(vec![b'x'; CEILING_BYTES as usize + 1])),
    )
    .await;

    let err = frankfurter_at(feed)
        .fetch_rates("NZD")
        .await
        .expect_err("a body past the ceiling must not be buffered");
    let chain = format!("{err:#}");

    // hyper sets `Content-Length` from the stub body's exact size hint, so the cheap guard — the
    // declared length, checked before a single allocation — is the one that fires here. The
    // running-total guard behind it has no reachable trigger through a stub, and its boundary is
    // what `enforce_cap`'s unit tests pin.
    assert!(
        chain.contains(&CEILING_BYTES.to_string()),
        "the error must name the ceiling it enforced, and it must be the one in http.rs: {chain}"
    );
    assert!(
        chain.contains(&(CEILING_BYTES + 1).to_string()),
        "the error must name the size that broke it, so a log line says how far over: {chain}"
    );
    assert!(
        chain.contains(&feed.to_string()),
        "the error must name the host, which is the only clue to which feed misbehaved: {chain}"
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A body that is not JSON fails by naming the URL it came from.
///
/// The URL is the whole diagnostic value of this error: three adapters share `json_capped`, and
/// the message is what tells an operator reading a scheduler log which feed broke and on what
/// query. A truncated table is the realistic shape — a response cut off mid-flight, or an upstream
/// that started answering HTML — and it is the failure a 3xx also arrives as, which is why the
/// message has to carry more than "invalid JSON".
#[tokio::test]
async fn a_malformed_body_surfaces_as_a_decode_error_naming_the_url() {
    let cluster = ProxyClusterBuilder::new()
        .add_stub("frankfurter", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the stub cluster");
    let feed = cluster.addr("frankfurter").expect("frankfurter is bound");

    stub_latest(
        &cluster,
        "frankfurter",
        StubbedResponse::new(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Bytes::from_static(
                br#"{"amount":1.0,"base":"NZD","date":"2026-07-16","rates":{"AUD":0.9241"#,
            )),
    )
    .await;

    let err = frankfurter_at(feed)
        .fetch_rates("NZD")
        .await
        .expect_err("a truncated table is not a rate table");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("decode the JSON body from"),
        "the decode failure must keep its context: {chain}"
    );
    assert!(
        chain.contains("/latest?base=NZD"),
        "the message must carry the path and query, not just the host: {chain}"
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A 5xx surfaces as a status error, which is the contrast that makes the 3xx test worth having.
///
/// `error_for_status` fails on exactly `is_client_error() || is_server_error()`. So a 500 is the
/// case the adapter's error handling was written for and a 3xx is the case it silently is not —
/// two statuses, both non-success to a reader, arriving as two entirely different kinds of error.
/// Asserting the status off the reqwest error rather than grepping its text also pins that the
/// code survives the trip: `sure-app`'s sync bookkeeping records the failure, and a status is what
/// distinguishes "the feed is down, retry next poll" from "we asked wrong".
#[tokio::test]
async fn an_upstream_500_surfaces_as_a_status_error() {
    let cluster = ProxyClusterBuilder::new()
        .add_stub("frankfurter", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the stub cluster");
    let feed = cluster.addr("frankfurter").expect("frankfurter is bound");

    stub_latest(
        &cluster,
        "frankfurter",
        StubbedResponse::new(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "application/json")
            .body(Bytes::from_static(br#"{"message":"upstream unavailable"}"#)),
    )
    .await;

    let err = frankfurter_at(feed)
        .fetch_rates("NZD")
        .await
        .expect_err("a 500 is not a rate table");
    let status = err
        .downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status);
    assert_eq!(
        status.map(|code| code.as_u16()),
        Some(500),
        "a 5xx must arrive as a reqwest status error, not as a decode failure: {err:#}"
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// The one bound whose failure mode is a *hang*, and the only test in this crate that spends
/// wall-clock time on purpose: it waits out `http::REQUEST_TIMEOUT` (6s). Do not shorten the stall
/// below that ceiling and do not delete this to save six seconds — the assertion is that the
/// client gives up before the upstream answers, and there is no way to observe that without
/// letting the clock run. It is last in the file so every cheap test has already reported.
///
/// Why it earns the six seconds: three of these adapters are driven by `sure-scheduler`, which
/// cancels *between* tasks and never during one. `reqwest` sets no timeout of any kind by default,
/// so an upstream that accepts the connection and then goes quiet holds the process open until the
/// shutdown drain deadline expires and the task is abandoned — a dirty shutdown, and a WAL segment
/// left behind, caused by someone else's server. That is the outcome this const prevents, and
/// nothing but a real stall demonstrates it.
///
/// One test, and since the reqwest 0.13 collapse there is not even a second builder to argue
/// about: `http::client` builds the client every adapter uses, from one `REQUEST_TIMEOUT`. A
/// second copy of this test through `AkahuProvider` would re-time the identical client for
/// another six seconds and assert nothing new.
#[tokio::test]
async fn a_stalled_upstream_fails_at_the_request_timeout_instead_of_hanging() {
    /// Well past the 6s ceiling, not just over it. The only assertion that stays meaningful is
    /// "the client gave up sooner than the upstream would have answered", and a tight stall turns
    /// that into a flake on a loaded machine — the timer is 6s, but a runtime competing with a
    /// `cargo build` for a core can deliver it a second or two late. A pass still costs ~6s,
    /// because nothing waits the stall out: see the `shutdown_with_timeout` at the end.
    const STALL: Duration = Duration::from_secs(15);

    let cluster = ProxyClusterBuilder::new()
        .add_stub("frankfurter", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the stub cluster");
    let feed = cluster.addr("frankfurter").expect("frankfurter is bound");

    stub_latest(
        &cluster,
        "frankfurter",
        StubbedResponse::new(StatusCode::OK)
            .header("content-type", "application/json")
            // A valid table behind the delay, so the only reason this call can fail is the clock.
            .body(Bytes::from_static(RATE_TABLE.as_bytes()))
            .delay(STALL),
    )
    .await;

    // Built before the clock starts, and that placement is load-bearing: `client()` builds a
    // rustls client, which loads the platform trust store, and on macOS that alone measured
    // several seconds — long enough to make the timing assertion below fail while the timeout it
    // is about worked perfectly.
    let provider = frankfurter_at(feed);

    let started = Instant::now();
    let err = provider
        .fetch_rates("NZD")
        .await
        .expect_err("a stalled upstream must not be waited out");
    let elapsed = started.elapsed();

    let transport = err
        .downcast_ref::<reqwest::Error>()
        .unwrap_or_else(|| panic!("a stall must fail in the transport, not in parsing: {err:#}"));
    assert!(
        transport.is_timeout(),
        "the client must fail on its own timeout rather than on anything the upstream said: \
         {err:#}"
    );
    assert!(
        elapsed < STALL,
        "the client waited {elapsed:?} — long enough for the upstream to answer — instead of \
         giving up at its own ceiling"
    );

    // Zero drain budget rather than `shutdown()`'s five seconds: the stub is still sitting in the
    // rest of its stall, and the only thing a graceful drain could accomplish here is adding that
    // remainder to every run of the suite.
    cluster
        .shutdown_with_timeout(Duration::ZERO)
        .await
        .expect("stub cluster stops");
}
