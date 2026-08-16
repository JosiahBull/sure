//! The Yahoo Finance fetch path, end to end over a loopback proxy.
//!
//! Until the [`Endpoint`] constructors landed, `yahoo_finance.rs` could only be tested from
//! `parse_quotes` inwards: everything between "here is a ticker and a date range" and "here is a
//! parsed chart" was reachable only by calling an undocumented endpoint that could change without
//! notice. That is the half of the adapter carrying the four behaviours below, and every one of
//! them is there because of something that actually happened:
//!
//! - the requested window is **padded by a day on each side**, because Yahoo buckets bars by
//!   the exchange's local trading day and a UTC-midnight boundary clips the edge day;
//! - `symbol_for` resolves an exchange hint to a suffix, and the *resolved* symbol has to be
//!   what the URL is built from — the in-crate unit test pins the mapping and cannot see the
//!   wire;
//! - a **404 is `Ok(vec![])`**, not an error, because a delisted or expired symbol legitimately
//!   has no prices and a normal portfolio contains several;
//! - a **500ms throttle** spaces consecutive requests, because hammering this endpoint risks
//!   an IP block, and it holds its lock across the sleep so concurrent callers queue rather
//!   than all firing at once.
//!
//! And then the one that decides whether any of the above stays testable: Yahoo's query
//! carries a *clock reading*, so a recorded snapshot goes stale the day after it is taken
//! unless `period1`/`period2` are canonicalised out of the replay key.
//! `tests/proxy_contract.rs` proves that mechanism against a hand-written URL; the last two
//! tests here prove it against the URL `fetch_daily_prices` actually builds, using the
//! parameter list [`Upstream::YahooFinance`] actually declares.
//!
//! Nothing here can reach the internet. Every cluster either has no upstream at all
//! (`add_stub`) or points at `http://unreachable.invalid`, and the recording passes are
//! answered by stubs before the forward is ever attempted.
//!
//! [`Endpoint`]: sure_providers::Endpoint

use std::sync::Arc;

use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, Utc};
use http::{Method, StatusCode};
use partly_proxy_lib::{
    ClusterHandle, Command, Mode, ProxyClusterBuilder, ProxyConfig, RequestMatcher,
    SharedMiddleware, SharedStorage, StubbedResponse, UpstreamTarget, shared,
};
use sure_app::ports::{StockPriceProvider, StockPriceQuote};
use sure_providers::{Endpoint, Pacing, YahooFinanceProvider};
use sure_testproxy::{CanonicaliseQuery, Upstream};

mod common;
use common::ephemeral;

/// Three days of NZX closes in the shape `yahoo-finance-client` deserialises.
///
/// The timestamps are the load-bearing part. Yahoo stamps a daily bar at the exchange's local
/// market open — NZX opens 10:00, and early March is inside NZDT (UTC+13, hence
/// `gmtoffset: 46800`) — so a bar for Monday 2026-03-02 sits at 2026-03-01T21:00:00Z, the day
/// *before* on the UTC clock. A fixture built from midnight-UTC timestamps would deserialise
/// and produce dates just fine while proving nothing about either `gmtoffset` or the padding.
///
/// `5.630000114440918` is the float32-origin noise Yahoo's own JSON carries; it is here so the
/// `round_dp(4)` in `parse_quotes` is exercised on the fetch path and not only in the unit
/// test. `error: null`, the extra `meta` fields, and the one sibling `volume` array are not
/// read by the parser and are here because the real payload has them — a fixture trimmed to
/// exactly what serde needs would stop showing that an unread field is tolerated rather than
/// fatal. One sibling array rather than all four (`open`/`high`/`low`/`volume`): the point is
/// the tolerance, not how much ignored data can be piled up to demonstrate it.
///
/// Prices and tickers are public market data, so rule 3 does not reach them — `MEL` is
/// Meridian Energy and these are plausible closes for it.
const CHART_BODY: &str = r#"{"chart":{"result":[{"meta":{"currency":"NZD","symbol":"MEL.NZ","exchangeName":"NZE","instrumentType":"EQUITY","gmtoffset":46800,"timezone":"NZDT","exchangeTimezoneName":"Pacific/Auckland"},"timestamp":[1772398800,1772485200,1772571600],"indicators":{"quote":[{"close":[5.6,5.630000114440918,5.58],"volume":[1834021,2201884,1596330]}]}}],"error":null}}"#;

/// What Yahoo answers a 404 with. Restaurant Brands NZ was taken over in 2019 and its ticker
/// stopped resolving; the adapter's comment names it, so the fixture uses it.
///
/// The body is deliberately one the parser *would* choke on (`result: null` is
/// `YahooFinanceError::NoChartData`, "no chart data returned"), because that is what
/// distinguishes the two possible implementations: the client's early return on 404 becomes the
/// adapter's `Ok(vec![])`, and the same code with that check moved below the status check gives
/// an error instead. The two are separate variants for exactly this reason.
const DELISTED_BODY: &str = r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#;

/// A 5xx from Yahoo. Same `result: null` shape as a 404 body, on purpose: the only thing
/// separating "no prices" from "the upstream broke" is the status, so a test that pinned the
/// difference while also varying the body would not have pinned much.
const BROKEN_UPSTREAM_BODY: &str = r#"{"chart":{"result":null,"error":{"code":"Internal Server Error","description":"Internal Server Error"}}}"#;

/// A 429 from Yahoo, in the same `result: null` shape and for the same reason: what separates
/// a refusal-on-volume from any other failure is the status, not the body.
///
/// Deliberately **no** `Retry-After` header on the stub that carries it — Yahoo's undocumented
/// endpoint sends none, which is exactly the case `DEFAULT_BACKOFF` exists for. A test that
/// supplied one would be checking the header parser (`http.rs`'s unit tests do that) instead of
/// the path production takes.
const RATE_LIMITED_BODY: &str = r#"{"chart":{"result":null,"error":{"code":"Too Many Requests","description":"Too Many Requests"}}}"#;

/// Stub matcher for Meridian on the NZX, as `symbol_for("mel", Some("NZX"))` resolves it.
///
/// A matcher pattern is a regex, so the `\.` is not decoration: an unescaped dot would also
/// match `MELXNZ` and quietly accept a symbol the adapter had no business building.
const MEL_PATH: &str = r"^/v8/finance/chart/MEL\.NZ$";
/// Restaurant Brands, for the delisted case.
const RBD_PATH: &str = r"^/v8/finance/chart/RBD\.NZ$";

/// `MIN_REQUEST_INTERVAL` (500ms) less a 50ms grace, in the units the recorder can answer in.
///
/// The grace exists because both halves of the measurement are approximations of "when did the
/// adapter send this": the throttle stamps `last_request` *before* the request goes out, and
/// the proxy's arrival time is recovered as `timestamp - duration`. The first request of a run
/// also pays a TCP connect the second one does not, which shortens the observed gap by however
/// long that took. 50ms absorbs all of it and is still two orders of magnitude away from the
/// failure being watched for, which looks like a gap under 5ms.
const THROTTLE_FLOOR_MS: i64 = 450;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("test date is a real date")
}

/// Monday to Friday of a real NZX trading week, and the week the three bars in [`CHART_BODY`]
/// sit inside. 2026-03-01 is a Sunday, so the padding lands on non-trading days at both ends —
/// which is exactly the shape the padding is for.
fn trading_week() -> (NaiveDate, NaiveDate) {
    (date(2026, 3, 2), date(2026, 3, 6))
}

/// A window ten weeks later: what the adapter asks for the *next* time the poller runs.
/// Nothing is ever recorded for it, so a replay hit against it can only have come from the
/// query being canonicalised.
fn a_later_week() -> (NaiveDate, NaiveDate) {
    (date(2026, 5, 11), date(2026, 5, 15))
}

/// A cluster with one stub-only Yahoo listener: no upstream target to dial and no snapshot to
/// read, so a request the stubs do not answer can only become a replay miss.
///
/// Deliberately **no** [`CanonicaliseQuery`]: `redact_request_for_snapshot` runs before the
/// recorder is handed the exchange, so with the middleware installed every recorded URI would
/// read `period1=CANONICAL` and the padding assertion below would have nothing to look at.
/// Only the two replay tests, which assert on what the adapter got back rather than on what
/// the recorder saw, install it.
async fn stub_cluster() -> ClusterHandle {
    ProxyClusterBuilder::new()
        .add_stub(Upstream::YahooFinance.name(), ephemeral(), Vec::new())
        .run()
        .await
        .expect("bind the stub cluster")
}

/// The middleware a recording has to be taken through for it to outlive the clock that took
/// it. `volatile_query_params()` rather than a literal `["period1", "period2"]`: pairing the
/// list `sure-testproxy` declares with the URL the adapter builds is the whole point of the
/// replay tests, and a hand-written list here would quietly agree with itself.
fn canonicalising() -> Vec<SharedMiddleware> {
    vec![shared(CanonicaliseQuery {
        params: Upstream::YahooFinance.volatile_query_params(),
    })]
}

async fn jsonl(path: &std::path::Path) -> SharedStorage {
    Arc::new(
        partly_proxy_lib::jsonl::JsonlStorage::open(path)
            .await
            .expect("open the NDJSON snapshot backend"),
    )
}

/// A recording cluster over `path`, with a stub standing in for Yahoo.
///
/// `http://unreachable.invalid` is the upstream target and it matters: `Mode::Record` forwards
/// on a stub miss, so a typo in a matcher must fail at DNS rather than reach the real
/// query1.finance.yahoo.com.
async fn recording_cluster(
    path: &std::path::Path,
    middleware: Vec<SharedMiddleware>,
) -> ClusterHandle {
    ProxyClusterBuilder::new()
        .add_upstream_with(
            Upstream::YahooFinance.name(),
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            middleware,
            Some(jsonl(path).await),
        )
        .run()
        .await
        .expect("bind the recording cluster")
}

/// A replay cluster over `path`, with no stubs at all: every answer it gives came from the
/// snapshot, and `Mode::Replay` means the upstream cannot be dialled even in principle.
async fn replay_cluster(
    path: &std::path::Path,
    middleware: Vec<SharedMiddleware>,
) -> ClusterHandle {
    ProxyClusterBuilder::new()
        .default_mode(Mode::Replay)
        .add_upstream_with(
            Upstream::YahooFinance.name(),
            ProxyConfig::http(
                ephemeral(),
                UpstreamTarget::new("http://unreachable.invalid"),
            ),
            middleware,
            Some(jsonl(path).await),
        )
        .run()
        .await
        .expect("bind the replay cluster")
}

async fn stub_chart(cluster: &ClusterHandle, path: &str, status: StatusCode, body: &'static str) {
    cluster
        .command_sender()
        .send(Command::Stub {
            upstream: Some(Upstream::YahooFinance.name().to_owned()),
            matcher: RequestMatcher::new().method(Method::GET).path(path),
            response: StubbedResponse::new(status)
                .header("content-type", "application/json")
                .body(Bytes::from_static(body.as_bytes())),
            // Unlimited: the throttle tests repeat one identical request, and every repeat has
            // to be answered by this one stub, so the gaps they measure are the adapter's
            // throttle and not a retired stub's 503.
            times: None,
        })
        .await
        .expect("register a chart stub");
}

/// Point a provider at a cluster's Yahoo listener, exactly as [`sure_testproxy::start`] hands
/// the URL to `sure-server`: bound address plus the upstream's path prefix, so the inbound
/// path the proxy sees is the one Yahoo's own documentation uses.
fn provider_at(cluster: &ClusterHandle) -> YahooFinanceProvider {
    let addr = cluster
        .addr(Upstream::YahooFinance.name())
        .expect("the yahoo_finance listener is bound");
    let url = format!("http://{addr}{}", Upstream::YahooFinance.path_prefix());
    YahooFinanceProvider::with_endpoint(
        Endpoint::parse(&url).expect("a loopback http:// proxy URL is the one plaintext case"),
        // The real numbers, not `Pacing::unpaced()`: two tests below measure the interval, and
        // a fixture that quietly turned it off would let a regression in it pass here.
        Pacing::default(),
    )
}

/// Quotes as `"<date> <close> <currency>"`, which is what a failure needs to read like.
///
/// `normalize()` is not cosmetic: `round_dp(4)` leaves `5.63` at scale four, so `Display`
/// renders it `5.6300` even though `Decimal`'s own equality would call the two equal. Without
/// it this helper would pin a scale nobody decided on.
fn describe(quotes: &[StockPriceQuote]) -> Vec<String> {
    quotes
        .iter()
        .map(|q| format!("{} {} {}", q.as_of, q.close.normalize(), q.currency_code))
        .collect()
}

/// The three quotes [`CHART_BODY`] must decode to, dates included: 21:00Z the previous day
/// plus a `gmtoffset` of +13h is the local trading day, and getting that arithmetic wrong is
/// how a week of closes lands one day early in the database.
fn expected_quotes() -> [&'static str; 3] {
    [
        "2026-03-02 5.6 NZD",
        "2026-03-03 5.63 NZD",
        "2026-03-04 5.58 NZD",
    ]
}

/// Gaps in milliseconds between consecutive requests *arriving* at the proxy.
///
/// `RecordedExchange::timestamp` is stamped when the exchange reaches the recorder — after the
/// response has been built — so comparing two of them directly folds each request's own
/// round-trip into the gap between them. `duration` is measured from the moment the listener
/// has the request line, so `timestamp - duration` is the arrival, and it is the closest thing
/// the recorder can offer to "when did the adapter send this". Wall clock in the test would be
/// measuring the test's own scheduling instead of what the upstream saw.
/// How many requests actually reached the proxy.
///
/// The only way to tell a request that was answered from one that was never made: both the
/// delisted-symbol memo and the rate-limit cooldown produce exactly what the request they
/// replace would have produced, so the return value cannot distinguish them.
async fn requests_seen(cluster: &ClusterHandle) -> usize {
    cluster.recorder().exchanges().await.len()
}

async fn arrival_gaps_ms(cluster: &ClusterHandle) -> Vec<i64> {
    let mut arrivals: Vec<DateTime<Utc>> = cluster
        .recorder()
        .exchanges()
        .await
        .into_iter()
        .map(|e| {
            e.timestamp
                - ChronoDuration::from_std(e.duration)
                    .expect("a stubbed exchange lasts milliseconds, not centuries")
        })
        .collect();
    // The ring is in *completion* order and the question is about arrival order. Identical
    // stubbed responses make the two the same in practice; sorting means no test turns on that
    // happening to hold.
    arrivals.sort_unstable();
    arrivals
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).num_milliseconds())
        .collect()
}

/// The padded window, byte for byte, as Yahoo receives it.
///
/// Yahoo buckets bars by the exchange's local trading day, so the requested range is widened by
/// a day at each end before it becomes `period1`/`period2`. Without it, the edge day of every
/// range silently goes missing — for a UTC+13 exchange the first day, for a US one the last —
/// and the failure looks like "Yahoo didn't have Monday", which is indistinguishable from a
/// public holiday until someone checks a whole year of them.
///
/// Asserting the entire URI rather than "some epochs were sent" is the point: the padding is
/// two `- ChronoDuration::days(1)` / `+ ChronoDuration::days(1)` calls that a refactor can
/// drop without changing anything else about the request.
#[tokio::test]
async fn the_requested_window_reaches_yahoo_padded_by_a_day_at_each_end() {
    // 2026-03-01T00:00:00Z and 2026-03-07T00:00:00Z — one day either side of the Mon–Fri week
    // asked for below. `date -u -r 1772323200` if you would rather not take that on trust.
    const PADDED_PERIOD1: i64 = 1_772_323_200;
    const PADDED_PERIOD2: i64 = 1_772_841_600;

    let cluster = stub_cluster().await;
    stub_chart(&cluster, MEL_PATH, StatusCode::OK, CHART_BODY).await;

    let (from, to) = trading_week();
    let quotes = provider_at(&cluster)
        .fetch_daily_prices("mel", Some("NZX"), from, to)
        .await
        .expect("the stub answers with a chart");
    assert_eq!(describe(&quotes), expected_quotes());

    let exchanges = cluster.recorder().exchanges().await;
    assert_eq!(exchanges.len(), 1, "one fetch is one request");
    assert_eq!(
        exchanges[0].request.uri,
        format!(
            "/v8/finance/chart/MEL.NZ?period1={PADDED_PERIOD1}&period2={PADDED_PERIOD2}&interval=1d"
        ),
        "the window on the wire is not the requested week padded by a day at each end",
    );

    // Why a day, and not just "some slack": the fixture's Monday bar is stamped
    // 2026-03-01T21:00:00Z, because NZX opens at 10:00 and March is UTC+13. That instant is
    // *before* midnight UTC on the requested first day, so an unpadded `period1` would have
    // asked for a window starting after the bar the caller wanted. `unpadded` is derived from
    // the same `from` the fetch used rather than written down, so this stays a statement about
    // the clipping instead of a restatement of the literals above.
    const MONDAY_BAR: i64 = 1_772_398_800;
    let unpadded = from
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists on every date")
        .and_utc()
        .timestamp();
    assert!(
        PADDED_PERIOD1 < MONDAY_BAR && MONDAY_BAR < unpadded,
        "the fixture no longer demonstrates the clipping the padding exists to prevent: \
         bar {MONDAY_BAR}, padded {PADDED_PERIOD1}, unpadded {unpadded}",
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// The symbol `symbol_for` resolved is the symbol the URL asks for.
///
/// The mapping itself is already covered by `maps_known_exchange_suffixes` inside the crate.
/// What a unit test cannot see is the step after it — that `fetch_daily_prices` builds its path
/// from the resolved symbol rather than from the raw ticker it was handed. The stub matches
/// `MEL.NZ` and nothing else, so an adapter that sent `mel`, `MEL`, or `mel.nz` gets a replay
/// miss and this test fails on the error rather than on the assertion; both halves of
/// `symbol_for` (uppercasing and the exchange suffix) are therefore load-bearing here, which is
/// why the call passes both in lower case.
///
/// If it regresses, every NZX holding is priced against whatever US instrument happens to share
/// its three letters — silently, because Yahoo answers with a perfectly valid chart.
#[tokio::test]
async fn the_exchange_resolved_symbol_is_what_the_url_asks_for() {
    let cluster = stub_cluster().await;
    stub_chart(&cluster, MEL_PATH, StatusCode::OK, CHART_BODY).await;

    let (from, to) = trading_week();
    let quotes = provider_at(&cluster)
        .fetch_daily_prices("mel", Some("nzx"), from, to)
        .await
        .expect("only a request for MEL.NZ can be answered by this cluster");
    assert_eq!(describe(&quotes), expected_quotes());

    let exchanges = cluster.recorder().exchanges().await;
    assert_eq!(exchanges.len(), 1);
    assert!(
        exchanges[0]
            .request
            .uri
            .starts_with("/v8/finance/chart/MEL.NZ?"),
        "the resolved symbol is not what reached the wire: {}",
        exchanges[0].request.uri,
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A delisted symbol is "no prices"; a broken upstream is a failure. The caller has to be able
/// to tell them apart.
///
/// An account's historical holdings routinely include symbols that have stopped resolving — a
/// company taken over (Restaurant Brands, 2019), a lapsed rights issue — and a poller that
/// treats each one as an error turns an ordinary portfolio into a hard failure and a warning
/// per ticker per run. So a 404 becomes `UnknownSymbol` *before* the client's status check, the
/// adapter turns that one variant into an empty vector, and this test is the thing that notices
/// if either half is ever reordered or collapsed: the 404 body here is one the parser would
/// reject, so a 404 that reached the parser would surface as "no chart data returned" rather
/// than as an empty result.
///
/// The other direction is the reason the two cases are one test. Widening the empty-vector
/// return to cover any non-success status would make a Yahoo outage look exactly like a
/// portfolio of delisted stocks, and nothing downstream would ever ask again.
#[tokio::test]
async fn a_delisted_symbol_yields_no_prices_while_a_broken_upstream_yields_an_error() {
    let cluster = stub_cluster().await;
    stub_chart(&cluster, RBD_PATH, StatusCode::NOT_FOUND, DELISTED_BODY).await;
    stub_chart(
        &cluster,
        MEL_PATH,
        StatusCode::INTERNAL_SERVER_ERROR,
        BROKEN_UPSTREAM_BODY,
    )
    .await;

    let provider = provider_at(&cluster);
    let (from, to) = trading_week();

    let delisted = provider
        .fetch_daily_prices("rbd", Some("NZX"), from, to)
        .await
        .expect("a 404 is 'this symbol has no prices', not a failure");
    assert!(
        delisted.is_empty(),
        "a delisted symbol must come back with no quotes, not {delisted:?}",
    );

    let err = provider
        .fetch_daily_prices("mel", Some("NZX"), from, to)
        .await
        .expect_err("a 5xx must not be reported as 'this symbol has no prices'")
        .to_string();
    assert!(
        err.contains("500"),
        "the error has to name the status, or a Yahoo outage is indistinguishable from a \
         delisted holding in the logs: {err}",
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// Two fetches from one provider instance arrive at least `MIN_REQUEST_INTERVAL` apart.
///
/// Yahoo publishes no rate limit for this endpoint, which is not the same as not having one:
/// what it has is an IP block, and it is applied to the whole machine rather than to the
/// request that earned it. The backfill walks a decade of closes one symbol at a time, so
/// "consecutive requests" is the normal case and not an edge one.
#[tokio::test]
async fn two_sequential_fetches_arrive_a_throttle_interval_apart() {
    let cluster = stub_cluster().await;
    stub_chart(&cluster, MEL_PATH, StatusCode::OK, CHART_BODY).await;

    let provider = provider_at(&cluster);
    let (from, to) = trading_week();
    for _ in 0..2 {
        provider
            .fetch_daily_prices("mel", Some("NZX"), from, to)
            .await
            .expect("the stub answers every chart request");
    }

    let gaps = arrival_gaps_ms(&cluster).await;
    assert_eq!(gaps.len(), 1, "two requests, one gap: {gaps:?}");
    assert!(
        gaps[0] >= THROTTLE_FLOOR_MS,
        "two sequential fetches arrived {}ms apart; the throttle is 500ms",
        gaps[0],
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// Concurrent fetches from one shared provider *queue*. This is the property
/// `throttle`'s "holding the lock across the sleep serializes concurrent callers" comment
/// claims, and the one nothing else checks.
///
/// The shape is production's: the scheduler's price poll and an on-demand lookup hold the same
/// `Arc<YahooFinanceProvider>`, and nothing coordinates them. `tokio::join!` drives the futures
/// on a single task, which is enough — every step of `throttle` is an await point (the mutex
/// acquire and the sleep both yield), so the futures interleave exactly where the serialisation
/// has to happen.
///
/// **Three callers, not two**, because two do not discriminate. Reading `last_request`,
/// releasing the guard, stamping it, and only then sleeping would still space *two* callers
/// 500ms apart — the second one sees the first one's stamp. It would not space the third:
/// callers two and three would both compute their wait from the same stamp and fire together
/// at +500ms. Only a guard held across the sleep makes the arrivals 0 / +500 / +1000, so the
/// second gap is what actually pins the comment.
///
/// The failure this prevents: a cron tick landing on top of a user opening the holdings page
/// puts two — or, with a multi-symbol backfill behind it, twenty — requests on the wire in the
/// same millisecond, which is the exact traffic shape that earns the IP block.
#[tokio::test]
async fn concurrent_fetches_queue_behind_the_throttle_rather_than_firing_together() {
    let cluster = stub_cluster().await;
    stub_chart(&cluster, MEL_PATH, StatusCode::OK, CHART_BODY).await;

    let provider = Arc::new(provider_at(&cluster));
    let (from, to) = trading_week();
    let (first, second, third) = tokio::join!(
        provider.fetch_daily_prices("mel", Some("NZX"), from, to),
        provider.fetch_daily_prices("mel", Some("NZX"), from, to),
        provider.fetch_daily_prices("mel", Some("NZX"), from, to),
    );
    for result in [first, second, third] {
        assert_eq!(
            describe(&result.expect("the stub answers every chart request")),
            expected_quotes(),
        );
    }

    let gaps = arrival_gaps_ms(&cluster).await;
    assert_eq!(gaps.len(), 2, "three requests, two gaps: {gaps:?}");
    for (index, gap) in gaps.iter().enumerate() {
        assert!(
            *gap >= THROTTLE_FLOOR_MS,
            "concurrent fetches {} and {} arrived {gap}ms apart (all gaps: {gaps:?}); they were \
             not serialised by the throttle",
            index + 1,
            index + 2,
        );
    }

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A symbol Yahoo has 404'd for is not asked about again inside the TTL.
///
/// The loop this closes is a request-per-page-render, forever. A 404 makes `fetch_daily_prices`
/// return an empty vec, so `sure_app::stock_prices::price_at` writes nothing to the
/// `stock_prices` cache, so its next call misses again and asks again — and an account holding
/// one delisted or mistyped ticker reaches Yahoo every single time the page showing it renders.
/// The throttle spaced those requests out; it never removed them, and this endpoint answers a
/// sustained trickle with an IP block on the whole machine.
///
/// Counted at the proxy rather than inferred from the return value, because the two agree by
/// construction: the memo returns the same empty vec the request would have. The count is the
/// only thing that can tell them apart.
#[tokio::test]
async fn a_delisted_symbol_is_remembered_rather_than_asked_about_again() {
    let cluster = stub_cluster().await;
    stub_chart(&cluster, RBD_PATH, StatusCode::NOT_FOUND, DELISTED_BODY).await;

    let provider = provider_at(&cluster);
    let (from, to) = trading_week();
    for attempt in 0..3 {
        let quotes = provider
            .fetch_daily_prices("rbd", Some("NZX"), from, to)
            .await
            .unwrap_or_else(|e| panic!("attempt {attempt} must report 'no prices', not fail: {e}"));
        assert!(
            quotes.is_empty(),
            "attempt {attempt} came back with quotes for a delisted symbol: {quotes:?}",
        );
    }

    assert_eq!(
        requests_seen(&cluster).await,
        1,
        "three lookups of one delisted symbol must reach Yahoo once",
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A 429 stands the adapter down: the next call fails without a request going out.
///
/// The behaviour that distinguishes a rate limit from every other error, and the reason it
/// cannot just be an error: retrying a 429 immediately is what turns a temporary throttle into
/// the IP block. Before this, `error_for_status` flattened it into the same generic status
/// error as any 4xx, and the next caller — a page render, a multi-symbol backfill loop — went
/// straight back out.
///
/// The stub is `times: None`, so a second request would be *answered*, not refused. A count of
/// one is therefore the adapter's own doing rather than a retired stub's 503.
#[tokio::test]
async fn a_rate_limit_stands_the_adapter_down_without_a_second_request() {
    let cluster = stub_cluster().await;
    stub_chart(
        &cluster,
        MEL_PATH,
        StatusCode::TOO_MANY_REQUESTS,
        RATE_LIMITED_BODY,
    )
    .await;

    let provider = provider_at(&cluster);
    let (from, to) = trading_week();

    let refused = provider
        .fetch_daily_prices("mel", Some("NZX"), from, to)
        .await
        .expect_err("a 429 is a failure, not an empty result")
        .to_string();
    assert!(
        refused.contains("429"),
        "the error has to name the status: {refused}",
    );

    // The one that matters: this call never reaches the wire at all.
    let stood_down = provider
        .fetch_daily_prices("mel", Some("NZX"), from, to)
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
        "the second lookup must not have reached Yahoo",
    );

    cluster.shutdown().await.expect("stub cluster stops");
}

/// A snapshot recorded for one date range answers a fetch for a different one.
///
/// This is the test that decides whether this adapter can have committed fixtures at all.
/// `fetch_daily_prices` derives `period1`/`period2` from the dates it is handed, which in
/// production come from "what is missing between the last stored close and today" — so the
/// query is a clock reading, the replay index compares the query verbatim, and a snapshot taken
/// today stops matching tomorrow unless those two parameters are canonicalised out of the key.
///
/// `proxy_contract.rs` already proves that mechanism generically, against a URL written by
/// hand. What it cannot prove is that this adapter's *real* URL shape survives it — that the
/// parameters `Upstream::YahooFinance::volatile_query_params` declares are the parameters
/// `fetch_daily_prices` varies. This test pairs the two sides: the middleware takes its list
/// from the enum, the URL comes from the adapter, and the recorded window (March) and the
/// replayed one (May, ten weeks later) have nothing in common.
///
/// If a future `period3` or an `&events=div` joins the query without being declared volatile,
/// this fails on the day it lands rather than the day after the next recording.
#[tokio::test]
async fn a_chart_recorded_for_one_window_replays_for_another() {
    let dir = tempfile::tempdir().expect("create a temp snapshot dir");
    let path = dir
        .path()
        .join(format!("{}.ndjson", Upstream::YahooFinance.name()));

    // --- record ------------------------------------------------------------------------
    let recording = recording_cluster(&path, canonicalising()).await;
    stub_chart(&recording, MEL_PATH, StatusCode::OK, CHART_BODY).await;
    let (from, to) = trading_week();
    let recorded = provider_at(&recording)
        .fetch_daily_prices("mel", Some("NZX"), from, to)
        .await
        .expect("the stub answers the recording pass");
    assert_eq!(describe(&recorded), expected_quotes());
    // Flushes the NDJSON backend, so the replay pass below reads a complete file.
    recording.shutdown().await.expect("recording cluster stops");

    // --- replay ------------------------------------------------------------------------
    let replaying = replay_cluster(&path, canonicalising()).await;
    let provider = provider_at(&replaying);
    let (later_from, later_to) = a_later_week();
    let replayed = provider
        .fetch_daily_prices("mel", Some("NZX"), later_from, later_to)
        .await
        .expect("a canonicalised query must replay a snapshot taken for a different window");
    assert_eq!(
        describe(&replayed),
        expected_quotes(),
        "the snapshot answered, but not with what was recorded",
    );

    // The control: replay is matching a key, not answering everything. A symbol nobody recorded
    // is a 503, which the adapter surfaces as an error — so a fixture that does not exist fails
    // loudly instead of quietly becoming a request to the real Yahoo.
    let err = provider
        .fetch_daily_prices("spk", Some("NZX"), later_from, later_to)
        .await
        .expect_err("an unrecorded symbol has no snapshot to answer from")
        .to_string();
    assert!(err.contains("503"), "{err}");

    replaying.shutdown().await.expect("replay cluster stops");
}

/// The negative control: the same recording, the same moved window, no canonicalisation — and
/// the adapter cannot get its quotes.
///
/// This is what makes the test above mean something. `proxy_contract.rs` carries the same
/// control for a hand-written URL; this one runs it through `fetch_daily_prices`, so it is
/// specifically the *adapter's* query that is shown to need the middleware. The identical-window
/// fetch first is what separates "the key missed" from "the snapshot file was empty".
///
/// Two ways it can start failing, and both are worth being told about. If the `expect_err`
/// stops holding, either `partly-proxy-lib` has grown query normalisation of its own — making
/// `CanonicaliseQuery` dead weight worth deleting — or `fetch_daily_prices` has stopped putting
/// a clock reading in its query, in which case `volatile_query_params` should drop `period1` and
/// `period2` and this file should lose its last two tests.
#[tokio::test]
async fn without_canonicalisation_the_adapters_own_query_misses_the_snapshot() {
    let dir = tempfile::tempdir().expect("create a temp snapshot dir");
    let path = dir
        .path()
        .join(format!("{}.ndjson", Upstream::YahooFinance.name()));

    let recording = recording_cluster(&path, Vec::new()).await;
    stub_chart(&recording, MEL_PATH, StatusCode::OK, CHART_BODY).await;
    let (from, to) = trading_week();
    provider_at(&recording)
        .fetch_daily_prices("mel", Some("NZX"), from, to)
        .await
        .expect("the stub answers the recording pass");
    recording.shutdown().await.expect("recording cluster stops");

    let replaying = replay_cluster(&path, Vec::new()).await;
    let provider = provider_at(&replaying);

    // The same window still replays: the snapshot is sound, so the miss below is about the key.
    let same = provider
        .fetch_daily_prices("mel", Some("NZX"), from, to)
        .await
        .expect("an identical window replays even with the query recorded verbatim");
    assert_eq!(describe(&same), expected_quotes());

    let (later_from, later_to) = a_later_week();
    let err = provider
        .fetch_daily_prices("mel", Some("NZX"), later_from, later_to)
        .await
        .expect_err("an un-canonicalised epoch must miss the snapshot it was recorded into")
        .to_string();
    assert!(
        err.contains("503"),
        "the replay key compares the query verbatim, so a moved window has to miss: {err}",
    );

    replaying.shutdown().await.expect("replay cluster stops");
}
