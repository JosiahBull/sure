//! The House Pricer adapter's outbound behaviour: that it paces itself, and that a refusal
//! stands it down instead of being retried straight back into.
//!
//! Stub-served, and never recorded. This is the second upstream (after Akahu) whose traffic this
//! repository will not keep: a `/match` response is a dossier on one dwelling — street address,
//! GPS centroid, title boundary, legal description — for wherever the person running Sure lives.
//! `scripts/pii-scan.mjs` refuses a `house_pricer` recording by path *and* by content, so every
//! fixture below is hand-authored with an invented address (CLAUDE.md rule 3).
//!
//! What is *not* here: the JSON contract. That moved to `house-pricer-client` and is tested next
//! to the struct that would have to change if the upstream renamed a field. This file is about
//! the half `sure-providers` kept — the throttle, the stand-down, and the mapping into a
//! `PropertyEstimate`.

// Money is written in minor units with a `dollars_cents` grouping (`650_000_00` == $650,000.00),
// the convention the whole workspace uses. Repeated here rather than inherited because a file
// under `tests/` is its own crate root, so `src/lib.rs`'s crate-level allow does not reach it —
// same allow, same reason, as `sure-providers`/`sure-app`/`sure-dal`.
#![allow(clippy::inconsistent_digit_grouping)]

use bytes::Bytes;
use http::{Method, StatusCode};
use partly_proxy_lib::{
    ClusterHandle, Command, ProxyClusterBuilder, RequestMatcher, StubbedResponse,
};
use sure_app::ports::PropertyEstimateProvider;
use sure_providers::{Endpoint, HousePricerProvider, Pacing};

mod common;
use common::ephemeral;

/// A match, with an invented address and id. `unitOfPropertyId` is a nil-ish UUID that could not
/// be a real one; the address is the placeholder the account form already suggests.
const MATCH_BODY: &str = r#"{
    "unitOfPropertyId": "00000000-0000-4000-8000-000000000001",
    "streetAddress": "123 kowhai street, riccarton",
    "grossSalePricePredictedModelA": 650000.0,
    "grossSalePricePredictedModelB": 598000.0
}"#;

/// What the upstream sends when it is refusing on volume. Shape only — House Pricer publishes no
/// rate limit, which is rather the point of standing down when it does start refusing.
const REFUSED_BODY: &str = r#"{"_embedded":{"errors":[{"message":"Too many requests"}]}}"#;

async fn stub_cluster() -> ClusterHandle {
    ProxyClusterBuilder::new()
        .add_stub("house_pricer", ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the house_pricer stub listener")
}

/// Register one answer for `GET /match`. `times: None`, so it answers *every* request — which is
/// what makes a request count meaningful below: a second call that does not arrive was stopped
/// by the adapter, not by a retired stub's 503.
async fn stub_match(cluster: &ClusterHandle, status: StatusCode, body: impl Into<Bytes>) {
    cluster
        .command_sender()
        .send(Command::Stub {
            upstream: Some("house_pricer".into()),
            matcher: RequestMatcher::new().method(Method::GET).path(r"^/match$"),
            response: StubbedResponse::new(status)
                .header("content-type", "application/json")
                .body(body),
            times: None,
        })
        .await
        .expect("register a stub");
}

fn provider_at(cluster: &ClusterHandle) -> HousePricerProvider {
    let addr = cluster
        .addr("house_pricer")
        .expect("house_pricer upstream is bound");
    let endpoint = Endpoint::parse(&format!("http://{addr}"))
        .expect("loopback plaintext is the one non-TLS endpoint Endpoint will represent");
    // `unpaced`: these tests are about the *stand-down*, not the pacing interval, and paying a
    // real interval between two calls would only make them slower. `Pacing::unpaced` leaves the
    // cooldown machinery fully armed — see its doc comment.
    HousePricerProvider::with_endpoint(endpoint, Pacing::unpaced())
}

async fn requests_seen(cluster: &ClusterHandle) -> usize {
    cluster.recorder().exchanges().await.len()
}

/// The ordinary path still works, which is what makes the refusal tests below mean something:
/// a provider that never reached the upstream at all would pass them vacuously.
#[tokio::test]
async fn a_match_becomes_an_estimate() {
    let cluster = stub_cluster().await;
    stub_match(&cluster, StatusCode::OK, MATCH_BODY).await;

    let estimate = provider_at(&cluster)
        .fetch_estimate("123 kowhai street")
        .await
        .expect("a stubbed match is an estimate")
        .expect("a match is Some");

    assert_eq!(estimate.value_minor, 650_000_00);
    assert_eq!(estimate.currency_code, "NZD");
    assert_eq!(estimate.model_note, "model A 650000, model B 598000");
    assert_eq!(requests_seen(&cluster).await, 1);

    cluster.shutdown().await.expect("cluster stops");
}

/// A 429 stands the adapter down: the next call fails without a request going out.
///
/// The behaviour that distinguishes a rate limit from every other error, and the reason it
/// cannot just be an error. This adapter was the last of the four without a throttle, and the
/// gap showed up in telemetry before anywhere else — a `provider=house_pricer` series on request
/// duration with no matching `provider.throttle.wait.duration`, i.e. a feed that looked
/// infinitely fast to pace because nothing was pacing it.
///
/// It matters here despite the traffic being tiny: the endpoint is undocumented, belongs to a
/// small operator, and publishes no limit — so the first sign of one will be it refusing, and
/// the wrong response to that is a monthly poll and a page render both going straight back out.
#[tokio::test]
async fn a_refusal_stands_the_adapter_down_without_a_second_request() {
    let cluster = stub_cluster().await;
    stub_match(&cluster, StatusCode::TOO_MANY_REQUESTS, REFUSED_BODY).await;

    let provider = provider_at(&cluster);

    let refused = provider
        .fetch_estimate("123 kowhai street")
        .await
        .expect_err("a 429 is a failure, not an empty match")
        .to_string();
    assert!(
        refused.contains("429"),
        "the error has to name the status: {refused}",
    );

    // The one that matters: this call never reaches the wire at all.
    let stood_down = provider
        .fetch_estimate("123 kowhai street")
        .await
        .expect_err("the cooldown is still in force")
        .to_string();
    assert!(
        stood_down.contains("not sent"),
        "the second call must be refused locally, not sent and refused again: {stood_down}",
    );

    assert_eq!(
        requests_seen(&cluster).await,
        1,
        "the stand-down must stop the second request, not merely fail it again",
    );

    cluster.shutdown().await.expect("cluster stops");
}

/// A 404 is *not* a refusal, and must not arm the cooldown.
///
/// House Pricer covers one city, so a 404 is the ordinary answer for any address outside
/// Christchurch and for one with a typo. Standing down on it would mean a single mistyped
/// address blocked the next real lookup — the failure mode that makes it worth having a variant
/// per outcome rather than one "the request failed".
#[tokio::test]
async fn a_no_match_leaves_the_adapter_available() {
    let cluster = stub_cluster().await;
    stub_match(&cluster, StatusCode::NOT_FOUND, REFUSED_BODY).await;

    let provider = provider_at(&cluster);
    assert!(
        provider
            .fetch_estimate("1 nowhere street")
            .await
            .expect("a 404 is Ok(None), not an error")
            .is_none()
    );

    // Still reachable: a second lookup goes out rather than being refused locally.
    assert!(
        provider
            .fetch_estimate("2 nowhere street")
            .await
            .expect("no cooldown was armed")
            .is_none()
    );
    assert_eq!(requests_seen(&cluster).await, 2);

    cluster.shutdown().await.expect("cluster stops");
}
