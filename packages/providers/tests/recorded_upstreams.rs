//! The two adapters that read *public* data, against a recording of what their real API said.
//!
//! Every other fixture in this directory hand-writes the response it wants, which is the right
//! shape for pinning behaviour: a 404 for a delisted symbol, a body over the ceiling, a page
//! whose cursor advances. But a hand-written body only ever contains the fields the person
//! writing it knew to include, so it cannot catch the failure that actually happens to a
//! keyless, undocumented feed — the upstream quietly changing what it sends. `yahoo_finance.rs`
//! says as much in its own header: the endpoint "is undocumented and could change without
//! notice".
//!
//! So these two replay a real capture. `snapshots/frankfurter.ndjson` and
//! `snapshots/yahoo_finance.ndjson` are committed, and were produced by the `#[ignore]`d
//! recorder at the bottom of this file talking to the live hosts. Yahoo's real chart response
//! carries roughly forty `meta` fields the adapter never reads, a `currentTradingPeriod`
//! object, and pre/post-market flags; Frankfurter's is a table of thirty-odd currencies. The
//! assertions below are deliberately *structural* — a currency, a count, a sign, a scale —
//! because the value of this file is "the parser still copes with the whole real document",
//! and pinning today's closing price would only mean a re-record churns the test.
//!
//! **Why only these two.** Both are public market data, so a recording carries no personal
//! information and can be committed (CLAUDE.md rule 3). Akahu is the opposite — real account
//! numbers, balances, payee names — and is never recorded into this repo at all;
//! `scripts/pii-scan.mjs` fails a commit that tries. Its fixtures are hand-authored with
//! invented identifiers, in `akahu.rs`.
//!
//! **Why the middleware chain here matches `sure_testproxy::start`'s exactly.** A snapshot is
//! keyed on the request *after* every middleware's `redact_request_for_snapshot`, so a capture
//! taken under one chain only replays under the same one. Recording through
//! `[RedactCredentials, CanonicaliseQuery]` — what `start` installs — is what keeps these files
//! usable from anywhere in the repo rather than only from this file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::NaiveDate;
use partly_proxy_lib::{
    ClusterHandle, Mode, ProxyClusterBuilder, ProxyConfig, RecordingConfig, SharedMiddleware,
    SharedStorage, UpstreamTarget, shared,
};
use rust_decimal::Decimal;
use sure_app::ports::{ExchangeRateProvider, StockPriceProvider};
use sure_providers::{Endpoint, FrankfurterProvider, YahooFinanceProvider};
use sure_testproxy::{CanonicaliseQuery, RedactCredentials, Upstream};

mod common;
use common::ephemeral;

/// The window the recorder asked Yahoo for, and the one the replays below ask for.
///
/// It has to be a fixed past range rather than "the last ten days": the capture is a file, and a
/// moving window would mean the recorded closes drift out of whatever the test asserts about
/// their dates. Canonicalisation makes the *lookup* indifferent to the range (that is its whole
/// job — see [`CanonicaliseQuery`]), so a replay could ask for anything; asking for the recorded
/// range is what lets these tests say something about which calendar day each close landed on.
const WINDOW: (&str, &str) = ("2026-07-01", "2026-07-10");

fn day(iso: &str) -> NaiveDate {
    NaiveDate::parse_from_str(iso, "%Y-%m-%d").expect("a literal ISO date in this file parses")
}

/// `packages/providers/tests/snapshots`, resolved from the manifest rather than the process's
/// working directory — `cargo test` does not promise what that is.
fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
}

fn snapshot_path(upstream: Upstream) -> PathBuf {
    snapshot_dir().join(format!("{}.ndjson", upstream.name()))
}

/// The chain `sure_testproxy::start` installs, rebuilt here so a capture taken by this file and
/// a replay driven from anywhere else agree on the key. See the module docs.
fn production_middleware(upstream: Upstream) -> Vec<SharedMiddleware> {
    vec![
        shared(RedactCredentials),
        shared(CanonicaliseQuery {
            params: upstream.volatile_query_params(),
        }),
    ]
}

async fn jsonl(path: &Path) -> SharedStorage {
    Arc::new(
        partly_proxy_lib::jsonl::JsonlStorage::open(path)
            .await
            .expect("open the committed NDJSON snapshot"),
    )
}

/// A cluster serving one upstream from its committed capture, and nothing else.
///
/// `Mode::Replay` is the guarantee that matters: the real host is named in the config and can
/// never be dialled, so a test that passes here passed on the recording. A request the capture
/// does not answer gets `503 {}`, which the adapter reports as a failure — loudly, rather than
/// by silently reaching the internet.
async fn replaying(upstream: Upstream) -> ClusterHandle {
    let path = snapshot_path(upstream);
    assert!(
        path.exists(),
        "missing capture {} — regenerate with `pnpm fixtures:record`",
        path.display()
    );
    ProxyClusterBuilder::new()
        .default_mode(Mode::Replay)
        .add_upstream_with(
            upstream.name(),
            ProxyConfig::http(ephemeral(), UpstreamTarget::new(upstream.target_base_url())),
            production_middleware(upstream),
            Some(jsonl(&path).await),
        )
        .run()
        .await
        .expect("bind a replay cluster")
}

/// The base URL an adapter is handed: the bound listener plus the upstream's real path prefix.
fn endpoint(cluster: &ClusterHandle, upstream: Upstream) -> Endpoint {
    let addr = cluster
        .addr(upstream.name())
        .expect("the upstream is bound");
    Endpoint::parse(&format!("http://{addr}{}", upstream.path_prefix()))
        .expect("a loopback http endpoint is accepted")
}

/// Frankfurter's real rate table parses, and every rate in it is usable.
///
/// The count is the point. A hand-written fixture carries the two or three currencies its author
/// needed; the real table carries every currency the ECB publishes, and `parse_quotes` runs
/// `Decimal::from_f64` over all of them, dropping any that will not convert. A regression there
/// would show up as a table that is *shorter* than it should be, which no small fixture can see.
#[tokio::test]
async fn the_real_frankfurter_table_parses_in_full() {
    let cluster = replaying(Upstream::Frankfurter).await;
    let provider = FrankfurterProvider::with_endpoint(endpoint(&cluster, Upstream::Frankfurter));

    let quotes = provider
        .fetch_rates("NZD")
        .await
        .expect("the recorded rate table parses");

    assert!(
        quotes.len() >= 25,
        "the ECB reference table is thirty-odd currencies; got {}",
        quotes.len()
    );
    for expected in ["AUD", "EUR", "GBP", "JPY", "USD"] {
        assert!(
            quotes.iter().any(|q| q.quote_code == expected),
            "{expected} missing from {} parsed quotes",
            quotes.len()
        );
    }
    for quote in &quotes {
        assert!(
            quote.rate > Decimal::ZERO,
            "{} parsed as {}",
            quote.quote_code,
            quote.rate
        );
        // The upstream's own reference date, not the day the capture was taken — every quote in
        // one response shares it, and a report renders it as "rates as of".
        assert_eq!(
            quote.as_of.len(),
            "2026-08-04".len(),
            "as_of is not an ISO date: {}",
            quote.as_of
        );
        assert_eq!(quote.as_of, quotes[0].as_of);
    }

    // `from_f64` rather than `from_f64_retain`, checked against real values: the shortest decimal
    // that round-trips, not the exact binary expansion. `0.51104` stays five digits instead of
    // becoming `0.51103999999999998248...`, which is what would land in the database and then in
    // every converted figure on the page.
    for quote in &quotes {
        assert!(
            quote.rate.scale() <= 8,
            "{} kept {} decimal places — from_f64_retain?",
            quote.quote_code,
            quote.rate.scale()
        );
    }

    cluster.shutdown().await.expect("cluster stops");
}

/// Yahoo's real chart response parses, for both symbol conventions.
///
/// Two captures in one file, distinguished by path: `VOO` (a US listing, no suffix) and `MEL.NZ`
/// (NZX, which the adapter suffixes). The NZ one is the interesting half — its `gmtoffset` is
/// +12/+13, so the adapter's "add the offset, then take the calendar date" step is what decides
/// whether a close is filed under the right trading day, and a fixture with `gmtoffset: 0` would
/// never exercise it.
#[tokio::test]
async fn the_real_yahoo_chart_parses_for_both_symbol_conventions() {
    let cluster = replaying(Upstream::YahooFinance).await;
    let provider = YahooFinanceProvider::with_endpoint(endpoint(&cluster, Upstream::YahooFinance));
    let (from, to) = (day(WINDOW.0), day(WINDOW.1));

    for (ticker, exchange, currency) in [("VOO", "NYSE Arca", "USD"), ("MEL", "NZX", "NZD")] {
        let quotes = provider
            .fetch_daily_prices(ticker, Some(exchange), from, to)
            .await
            .unwrap_or_else(|err| panic!("the recorded {ticker} chart parses: {err}"));

        assert!(
            quotes.len() >= 5,
            "{ticker}: a business week and a half should be ~8 bars; got {}",
            quotes.len()
        );
        for quote in &quotes {
            assert_eq!(quote.currency_code, currency, "{ticker} currency");
            assert!(
                quote.close > Decimal::ZERO,
                "{ticker} close on {} is {}",
                quote.as_of,
                quote.close
            );
            // Same `from_f64` property as the rate table above, and the one that matters more:
            // this value is persisted as TEXT and read back as a `Decimal`.
            assert!(
                quote.close.scale() <= 8,
                "{ticker} close {} kept {} decimal places",
                quote.close,
                quote.close.scale()
            );
            // Inside the window the adapter padded by a day at each end — which is what proves
            // the timestamp-plus-gmtoffset arithmetic put each bar on a real trading day rather
            // than one either side of the range.
            assert!(
                quote.as_of >= from - chrono::Duration::days(1)
                    && quote.as_of <= to + chrono::Duration::days(1),
                "{ticker}: {} is outside the requested window",
                quote.as_of
            );
        }

        // Strictly ascending: one bar per trading day, none repeated. A duplicate would upsert
        // over itself and silently lose a day.
        let mut days: Vec<NaiveDate> = quotes.iter().map(|q| q.as_of).collect();
        let before = days.len();
        days.dedup();
        assert_eq!(before, days.len(), "{ticker} returned a day twice");
        assert!(
            days.windows(2).all(|w| w[0] < w[1]),
            "{ticker} out of order"
        );
    }

    cluster.shutdown().await.expect("cluster stops");
}

/// A capture taken in July still answers a request made years later.
///
/// The reason to state this as its own test rather than trust it: it is the property that decides
/// whether these files are fixtures or landmines. `yahoo_finance.rs` derives
/// `?period1=<epoch>&period2=<epoch>` from the dates it is given, the replay index compares the
/// query verbatim, and so a capture keyed on real epochs would answer exactly one window and
/// `503` every other — for a *scheduled* task, that means the day after recording.
/// [`CanonicaliseQuery`] is what removes the date from the key, and the committed files carry
/// `period1=CANONICAL` because they were recorded through it.
///
/// The window below is deliberately absurd. A plausible one would leave the test passing for a
/// while by luck.
#[tokio::test]
async fn a_capture_answers_a_window_nowhere_near_the_one_it_was_taken_for() {
    let cluster = replaying(Upstream::YahooFinance).await;
    let provider = YahooFinanceProvider::with_endpoint(endpoint(&cluster, Upstream::YahooFinance));

    let quotes = provider
        .fetch_daily_prices(
            "VOO",
            Some("NYSE Arca"),
            day("2031-02-17"),
            day("2031-02-28"),
        )
        .await
        .expect("a snapshot must not expire with the window it was taken for");

    // The closes are July 2026's, because that is what is in the file — the point is only that
    // the lookup hit at all. Asserting the dates here would assert the recording, which the test
    // above already does.
    assert!(!quotes.is_empty(), "the capture answered with no bars");

    cluster.shutdown().await.expect("cluster stops");
}

// ---- the recorder ------------------------------------------------------------------------

/// Re-capture both snapshots from the live APIs. **Reaches the real internet**, hence `#[ignore]`.
///
/// ```sh
/// pnpm fixtures:record          # or: cargo test -p sure-providers --test recorded_upstreams \
///                               #       -- --ignored --nocapture
/// ```
///
/// Deletes each file first, on purpose. In `Mode::Record` an attached snapshot is a
/// *deduplicating cache* (`SPECIFICATION.md` §8.3): a request already in the file is served from
/// it and never re-fetched, so recording over an existing capture would refresh nothing and
/// quietly report success. "Record" has to mean record.
///
/// Review the diff before committing it. The point of re-recording is to find out whether the
/// upstream changed its document, and the answer is in the diff — a couple of new `meta` fields
/// is routine, a renamed or vanished one is the thing this whole file exists to catch.
///
/// Running this is also the *only* check that the captures still describe the live APIs. Nothing
/// in CI re-records: an automated version was written and dropped, because it needs two third
/// parties to answer a GitHub runner and a failure nobody can act on is worse than no job. So a
/// green suite means "the adapters parse the document we captured", not "…that the API sends
/// today" — which is the right guarantee for a fixture, as long as nobody reads it as the other
/// one. Reach for this when a price or FX path misbehaves against the real app but not here, and
/// after any change to `ChartResponse` or Frankfurter's response structs.
#[tokio::test]
#[ignore = "reaches the real Frankfurter and Yahoo APIs; run explicitly to re-record"]
async fn record_the_public_upstreams() {
    let dir = snapshot_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .expect("create the snapshot directory");

    for upstream in [Upstream::Frankfurter, Upstream::YahooFinance] {
        let path = snapshot_path(upstream);
        let _ = tokio::fs::remove_file(&path).await;

        let cluster = ProxyClusterBuilder::new()
            .default_mode(Mode::Record)
            // `default_mode` sets the recording config as a side effect, so this restates the
            // one we want after it — same reason `sure_testproxy::start` does.
            .recording(RecordingConfig::default())
            .add_upstream_with(
                upstream.name(),
                ProxyConfig::http(ephemeral(), UpstreamTarget::new(upstream.target_base_url())),
                production_middleware(upstream),
                Some(jsonl(&path).await),
            )
            .run()
            .await
            .expect("bind a recording cluster");

        match upstream {
            Upstream::Frankfurter => {
                let provider =
                    FrankfurterProvider::with_endpoint(endpoint(&cluster, Upstream::Frankfurter));
                let quotes = provider
                    .fetch_rates("NZD")
                    .await
                    .expect("the live Frankfurter API answers");
                println!("recorded {} rates from Frankfurter", quotes.len());
            }
            Upstream::YahooFinance => {
                let provider =
                    YahooFinanceProvider::with_endpoint(endpoint(&cluster, Upstream::YahooFinance));
                let (from, to) = (day(WINDOW.0), day(WINDOW.1));
                for (ticker, exchange) in [("VOO", "NYSE Arca"), ("MEL", "NZX")] {
                    let quotes = provider
                        .fetch_daily_prices(ticker, Some(exchange), from, to)
                        .await
                        .unwrap_or_else(|err| {
                            panic!("the live Yahoo API answers for {ticker}: {err}")
                        });
                    println!("recorded {} bars for {ticker}", quotes.len());
                }
            }
            // Not recorded, ever: Akahu carries real bank data. See the module docs, and
            // `scripts/pii-scan.mjs`, which fails a commit that tries.
            Upstream::Akahu => unreachable!("the loop above lists the two public upstreams"),
        }

        // Flushes the NDJSON backend, so the file is complete before the next iteration.
        cluster.shutdown().await.expect("recording cluster stops");
        println!("wrote {}", path.display());
    }
}
