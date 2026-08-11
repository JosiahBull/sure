//! The Akahu adapter's *fetch* path: which URL it calls, which headers it sends, which window
//! it asks for, and whether its pagination loop really follows the cursor.
//!
//! None of that has ever been tested. `sure_providers::akahu`'s own unit tests stop at
//! `map_account`/`map_transaction`, hand-fed a JSON literal, because reaching the code above
//! them needed two credentials CI does not have — and Akahu is also the one upstream whose
//! recordings this repo refuses to commit at all (`scripts/pii-scan.mjs`'s
//! `AKAHU_SNAPSHOT_PATH`: a real bank feed's account numbers, balances and payee names cannot be
//! scrubbed back out of a snapshot after the fact). So everything here is stub-served and
//! nothing is recorded to disk: `ProxyClusterBuilder::add_stub` binds a `Mode::Replay` upstream
//! with no snapshot storage, which is what makes `https://api.akahu.io` unreachable from this
//! file even by mistake, and `AkahuProvider::new` taking its credentials as a value is what
//! makes two invented tokens enough (before it, this test would have had to `set_var` and race
//! every other test in the binary).
//!
//! Every identifier, name and amount below is invented — CLAUDE.md rule 3, which bites hardest
//! on this provider of all of them. The bank account numbers are the established fakes from
//! `scripts/pii-scan.mjs`'s allowlist; the merchant brands are the ones that rule names as
//! deliberately safe to keep.
//!
//! One property of the proxy shapes half the assertions here, and it is easy to get wrong: the
//! in-memory recorder is handed the request **after** every middleware's
//! `redact_request_for_snapshot` (`partly-proxy-lib`'s `build_recorded`). So a test that wants
//! to read what actually went on the wire must register *no* middleware — with
//! `CanonicaliseQuery` installed the recorded `?start=` reads `CANONICAL`, and with
//! `RedactCredentials` installed the credential headers are simply gone. That is why
//! [`Fixture::start`] takes the middleware list, why most tests pass an empty one, and why
//! "the credentials reach the wire" and "the credentials never reach a snapshot" cannot be one
//! test.

use bytes::Bytes;
use http::{Method, StatusCode};
use partly_proxy_lib::{
    ClusterHandle, Command, ProxyClusterBuilder, RecordedRequest, RequestMatcher, SharedMiddleware,
    StubbedResponse, shared,
};
use sure_app::ports::{SyncContext, TransactionProvider};
use sure_core::AccountKind;
use sure_providers::{AkahuCredentials, AkahuProvider, Endpoint, MissingToken};
use sure_testproxy::RedactCredentials;

mod common;
use common::ephemeral;

/// The linked account every transaction fixture here belongs to.
const ACCOUNT_ID: &str = "acc_spend01";

/// The two tokens every configured fixture here is built with.
///
/// Invented, and — the point — supplied as a *value*. `AKAHU_APP_TOKEN` is never read, so
/// nothing in this file depends on the developer's shell or fights another test for the
/// process environment.
fn credentials() -> Result<AkahuCredentials, MissingToken> {
    Ok(AkahuCredentials {
        app_token: "app_token_test".to_string(),
        user_token: "user_token_test".to_string(),
    })
}

/// A stub-served stand-in for `api.akahu.io`, and an adapter aimed at it.
struct Fixture {
    cluster: ClusterHandle,
    provider: AkahuProvider,
}

impl Fixture {
    /// `middleware` is a parameter because it decides what the recorder is allowed to see (see
    /// the module comment); all but the last two tests want an empty list.
    async fn start(
        credentials: Result<AkahuCredentials, MissingToken>,
        middleware: Vec<SharedMiddleware>,
    ) -> Self {
        // `add_stub` forces this upstream to `Mode::Replay` with no snapshot storage, so an
        // unstubbed request is answered by the proxy's own 503 rather than forwarded anywhere.
        // Recording still happens: the *builder's* default mode is `Record`, and it is the
        // builder's mode that decides `RecordingConfig` — which is what leaves `recorder()`
        // populated for the request-side assertions below.
        let cluster = ProxyClusterBuilder::new()
            .add_stub("akahu", ephemeral(), middleware)
            .run()
            .await
            .expect("bind the akahu stub listener");
        let addr = cluster.addr("akahu").expect("akahu upstream is bound");
        // `/v1` is the prefix `sure-testproxy` hands to the server in production too, so the
        // paths the stubs match on are the paths Akahu documents.
        let endpoint = Endpoint::parse(&format!("http://{addr}/v1"))
            .expect("loopback plaintext is the one non-TLS endpoint Endpoint will represent");
        Self {
            cluster,
            provider: AkahuProvider::new(endpoint, credentials),
        }
    }

    /// A fixture with credentials and nothing between the wire and the recorder.
    async fn configured() -> Self {
        Self::start(credentials(), Vec::new()).await
    }

    async fn stub_matching(
        &self,
        matcher: RequestMatcher,
        body: impl Into<Bytes>,
        times: Option<u32>,
    ) {
        self.cluster
            .command_sender()
            .send(Command::Stub {
                upstream: Some("akahu".into()),
                matcher,
                response: StubbedResponse::new(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(body),
                times,
            })
            .await
            .expect("register a stub");
    }

    async fn stub(&self, path: &str, body: impl Into<Bytes>, times: Option<u32>) {
        let matcher = RequestMatcher::new().method(Method::GET).path(path);
        self.stub_matching(matcher, body, times).await;
    }

    /// A stub that fires only for a request that really carries `header`.
    ///
    /// The only way to assert anything about the *live* request rather than the recorded copy of
    /// it: the matcher runs on the request as it arrived, before the snapshot boundary rewrites
    /// anything. A constraint that is not met is a replay miss, so the adapter's own call fails.
    async fn stub_requiring(&self, path: &str, header: (&str, &str), body: impl Into<Bytes>) {
        let matcher = RequestMatcher::new()
            .method(Method::GET)
            .path(path)
            .header(header.0, header.1);
        self.stub_matching(matcher, body, None).await;
    }

    /// Register a stub that answers exactly once.
    ///
    /// The only way to give two different answers to two requests a matcher cannot tell apart —
    /// which is every paginated Akahu fetch, because a matcher sees `uri.path()` and never the
    /// `cursor` query parameter. Registration order decides, and each stub retires as it fires
    /// (pinned by `proxy_contract.rs`'s `single_fire_stubs_answer_in_registration_order`).
    async fn stub_once(&self, path: &str, body: impl Into<Bytes>) {
        self.stub(path, body, Some(1)).await;
    }

    /// Every request the proxy saw, in order, as the recorder holds it — i.e. after redaction.
    async fn requests(&self) -> Vec<RecordedRequest> {
        self.cluster
            .recorder()
            .exchanges()
            .await
            .into_iter()
            .map(|exchange| exchange.request)
            .collect()
    }

    async fn stop(self) {
        self.cluster.shutdown().await.expect("cluster stops");
    }
}

/// One query parameter's decoded value out of a recorded origin-form URI.
///
/// Parsed rather than substring-matched, because a raw-query assertion would be pinning two
/// accidents instead of the behaviour: `akahu-client` builds its query from a `HashMap`, so
/// `start` and `cursor` appear in whichever order that iteration happened to produce, and the
/// `url` crate form-encodes the `:` in an RFC-3339 timestamp to `%3A` on the way in.
fn query_param(uri: &str, key: &str) -> Option<String> {
    reqwest::Url::parse("http://akahu.invalid")
        .expect("a literal base URL parses")
        .join(uri)
        .expect("a recorded origin-form URI joins onto a base")
        .query_pairs()
        .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
}

/// One recorded request header, by its lowercased name.
fn header<'a>(request: &'a RecordedRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(candidate, _)| candidate.as_str() == name)
        .map(|(_, value)| value.as_str())
}

/// The provider config a linked Akahu account carries: the account id chosen in the connect
/// dialog, which is the only thing `fetch`/`current_balance` have to go on.
fn config(external_account_id: &str) -> serde_json::Value {
    serde_json::json!({ "external_account_id": external_account_id })
}

fn ctx<'a>(config: &'a serde_json::Value, last_synced_at: Option<&'a str>) -> SyncContext<'a> {
    SyncContext {
        config,
        account_currency: "NZD",
        payload: None,
        last_synced_at,
    }
}

/// One settled transaction as Akahu sends it. A helper rather than six near-identical literals:
/// only the id, date, description, amount and type differ between the rows below, and the full
/// wire shape — including the flattened enrichment — is spelled out in
/// [`a_page_of_transactions_keeps_its_enrichment_across_the_wire`].
fn txn(id: &str, date: &str, description: &str, amount: &str, kind: &str) -> String {
    format!(
        r#"{{
            "_id": "{id}",
            "_account": "{ACCOUNT_ID}",
            "_connection": "conn_bank01",
            "created_at": "2026-01-07T02:00:00.000Z",
            "date": "{date}",
            "description": "{description}",
            "amount": {amount},
            "type": "{kind}"
        }}"#
    )
}

/// Wrap rows in Akahu's paginated envelope. `next` is the cursor the *next* request must carry.
fn page(items: &[String], next: Option<&str>) -> String {
    let cursor = match next {
        Some(cursor) => format!(r#""{cursor}""#),
        None => "null".to_string(),
    };
    format!(
        r#"{{ "success": true, "items": [{}], "cursor": {{ "next": {cursor} }} }}"#,
        items.join(",")
    )
}

/// A page with nothing on it, for the tests that are only interested in the request.
fn empty_page() -> String {
    page(&[], None)
}

/// Anchored, so it cannot also swallow `/v1/accounts/acc_spend01`.
const ACCOUNTS_PATH: &str = r"^/v1/accounts$";

/// The transactions path for [`ACCOUNT_ID`]. A function rather than a `const` so the id is
/// written once: a matcher pattern is a regex, and `const` cannot call `format!`.
fn transactions_path() -> String {
    format!(r"^/v1/accounts/{ACCOUNT_ID}/transactions$")
}

/// Account discovery over the wire, and the one heuristic in this adapter that a user cannot
/// easily correct after the fact.
///
/// Akahu reports a mortgage, a student loan, a revolving-credit facility and a term loan all as
/// the same `"type": "LOAN"`, so `map_kind_hint` reads the account *name* first and falls back to
/// whether `balance.limit` is present — an ongoing limit means a revolving/line-of-credit
/// facility, where a term loan's ceiling is its original principal instead. All four land here
/// as one listing, which is how they actually arrive.
///
/// What a regression costs: the connect dialog pre-fills the wrong kind, and a kind is a whole
/// metadata *profile*. Get it wrong towards `loan` and `credit_limit_minor` has nowhere to land —
/// `sure_dal::accounts::set_credit_limit` writes only into `AccountMetadata::Depository`, so
/// "remaining borrowing" stays empty through every sync (which is what the adapter's own comment
/// records having confirmed against a real facility). Get it wrong towards `revolving_credit` and
/// a term loan lands on the depository profile instead, where `sure_app::forecast`'s `loan_terms`
/// finds no schedule and `set_original_amount` is a no-op: the account is projected as a trend
/// line rather than amortised. Neither is a correction anybody makes twice — the account is
/// already linked, and it looks fine.
#[tokio::test]
async fn discovery_maps_a_listing_and_tells_akahus_four_kinds_of_loan_apart() {
    let fixture = Fixture::configured().await;
    fixture
        .stub(
            ACCOUNTS_PATH,
            r#"{
                "success": true,
                "items": [
                    {
                        "_id": "acc_spend01",
                        "_authorisation": "auth_login01",
                        "connection": { "_id": "conn_bank01", "name": "ASB", "connection_type": "official" },
                        "name": "Everyday Spending",
                        "formatted_account": "12-3456-0000001-51",
                        "status": "ACTIVE",
                        "refreshed": { "balance": "2026-01-07T02:00:00.000Z" },
                        "balance": { "current": 2480.15, "available": 2480.15, "currency": "NZD" },
                        "type": "CHECKING",
                        "attributes": ["TRANSACTIONS", "PAYMENT_FROM"]
                    },
                    {
                        "_id": "acc_home01",
                        "_authorisation": "auth_login01",
                        "connection": { "_id": "conn_bank01", "name": "ASB", "connection_type": "official" },
                        "name": "Housing Lending Table",
                        "status": "ACTIVE",
                        "refreshed": {},
                        "balance": { "current": -401250.00, "currency": "NZD" },
                        "type": "LOAN",
                        "attributes": []
                    },
                    {
                        "_id": "acc_study01",
                        "_authorisation": "auth_login01",
                        "name": "Student Loan",
                        "status": "ACTIVE",
                        "refreshed": {},
                        "balance": { "current": -8140.50, "currency": "NZD" },
                        "type": "LOAN",
                        "attributes": []
                    },
                    {
                        "_id": "acc_flexi01",
                        "_authorisation": "auth_login01",
                        "connection": { "_id": "conn_bank01", "name": "ASB", "connection_type": "official" },
                        "name": "Flexi Facility",
                        "formatted_account": "12-3456-0000002-51",
                        "status": "ACTIVE",
                        "refreshed": {},
                        "balance": { "current": -12300.00, "limit": 25000.00, "currency": "NZD" },
                        "type": "LOAN",
                        "attributes": ["TRANSACTIONS"]
                    },
                    {
                        "_id": "acc_car01",
                        "_authorisation": "auth_login01",
                        "name": "Personal Loan",
                        "status": "ACTIVE",
                        "refreshed": {},
                        "balance": { "current": -6500.00, "currency": "NZD" },
                        "type": "LOAN",
                        "attributes": []
                    }
                ]
            }"#,
            None,
        )
        .await;

    let accounts = fixture
        .provider
        .list_accounts()
        .await
        .expect("a well-formed listing maps");

    let kinds: Vec<(&str, AccountKind)> = accounts
        .iter()
        .map(|a| (a.external_id.as_str(), a.kind_hint))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("acc_spend01", AccountKind::Bank),
            // "housing" in the name — Akahu itself says only "LOAN".
            ("acc_home01", AccountKind::Mortgage),
            ("acc_study01", AccountKind::StudentLoan),
            // No name signal, but an ongoing `balance.limit`: a facility, not a term loan.
            ("acc_flexi01", AccountKind::RevolvingCredit),
            // No name signal and no limit — the honest fallback.
            ("acc_car01", AccountKind::Loan),
        ],
    );

    // The rest of the discovery payload, on the account that carries all of it. `authorisation_id`
    // is per *login* rather than per institution, which is how the connect dialog tells two
    // household members' ASB accounts apart, and `account_number` is what separates two
    // same-named accounts inside one login.
    let spending = &accounts[0];
    assert_eq!(spending.name, "Everyday Spending");
    assert_eq!(spending.currency_code, "NZD");
    assert_eq!(spending.institution.as_deref(), Some("ASB"));
    assert_eq!(spending.authorisation_id.as_deref(), Some("auth_login01"));
    assert_eq!(
        spending.account_number.as_deref(),
        Some("12-3456-0000001-51")
    );
    assert_eq!(spending.balance_minor, 248_015);
    assert!(spending.supports_transactions);

    // A loan Akahu will not give us transactions for must not be offered as if it will: the
    // sync would fetch nothing forever and the balance-only path is the correct one.
    assert!(!accounts[1].supports_transactions);
    assert_eq!(accounts[1].balance_minor, -40_125_000);
    // No `connection` on the student loan fixture: absent, not guessed from a sibling.
    assert_eq!(accounts[2].institution, None);
    assert_eq!(accounts[2].balance_minor, -814_050);

    // One request, to the path Akahu documents — not `/accounts` (the prefix lost) and not
    // `/v1/v1/accounts` (the endpoint concatenated twice).
    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert!(
        requests[0].uri.starts_with("/v1/accounts"),
        "discovery called {}",
        requests[0].uri
    );

    fixture.stop().await;
}

/// The single-account refetch, which is where every supplementary fact about an account comes
/// from after linking: the balance itself, plus a credit limit, an institution name and the
/// original principal borrowed.
///
/// A redraw home loan is the fixture because it is the one shape that carries all five at once —
/// an ongoing facility limit *and* an initial principal. Each gets its own assertion because each
/// feeds a different screen, and a swapped pair — the facility limit read as the original
/// principal, say — stays arithmetically plausible everywhere it lands: the paid-down percentage
/// it produces reads as a data-entry problem rather than as a mapping bug.
///
/// These are all minor units converted from the arbitrary-precision `Decimal` Akahu sends, so
/// the assertions are also the conversion: `-401250.00` has to arrive as `-40_125_000` and not
/// as `-401_250`.
#[tokio::test]
async fn a_single_account_refetch_maps_every_supplementary_amount_it_carries() {
    let fixture = Fixture::configured().await;
    fixture
        .stub(
            r"^/v1/accounts/acc_home01$",
            r#"{
                "success": true,
                "item": {
                    "_id": "acc_home01",
                    "_authorisation": "auth_login01",
                    "connection": { "_id": "conn_bank01", "name": "ASB", "connection_type": "official" },
                    "name": "Housing Lending Table",
                    "status": "ACTIVE",
                    "refreshed": { "balance": "2026-01-07T02:00:00.000Z" },
                    "balance": { "current": -401250.00, "limit": 25000.00, "currency": "NZD" },
                    "meta": {
                        "loan_details": {
                            "purpose": "HOME",
                            "type": "TABLE",
                            "initial_principal": 450000.00
                        }
                    },
                    "type": "LOAN",
                    "attributes": []
                }
            }"#,
            None,
        )
        .await;

    let config = config("acc_home01");
    let balance = fixture
        .provider
        .current_balance(ctx(&config, None))
        .await
        .expect("a well-formed account maps")
        .expect("Akahu always reports a balance");

    assert_eq!(balance.minor, -40_125_000);
    assert_eq!(balance.currency_code, "NZD");
    assert_eq!(balance.limit_minor, Some(2_500_000));
    assert_eq!(balance.initial_principal_minor, Some(45_000_000));
    assert_eq!(balance.institution.as_deref(), Some("ASB"));

    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].uri.starts_with("/v1/accounts/acc_home01"),
        "the refetch asked for {} rather than the linked account",
        requests[0].uri
    );

    fixture.stop().await;
}

/// Cents are the last two digits of an integer here, and Akahu's `Decimal` is not limited to
/// two: a managed-fund balance is units × unit price, so a third decimal place is ordinary.
///
/// Rounding to the nearest cent is the only defensible answer, and the two directions are
/// asserted separately because truncation — which `to_i64` alone would do — is indistinguishable
/// from rounding on exactly half the inputs. A cent per account per sync is invisible until it
/// is a reconciliation nobody can close.
#[tokio::test]
async fn a_balance_finer_than_a_cent_is_rounded_to_the_nearest_one() {
    let fixture = Fixture::configured().await;
    let kiwisaver = |current: &str| {
        format!(
            r#"{{
                "success": true,
                "item": {{
                    "_id": "acc_kiwi01",
                    "_authorisation": "auth_login01",
                    "name": "Kiwi Growth Fund",
                    "status": "ACTIVE",
                    "refreshed": {{}},
                    "balance": {{ "current": {current}, "currency": "NZD" }},
                    "type": "KIWISAVER",
                    "attributes": []
                }}
            }}"#
        )
    };
    let path = r"^/v1/accounts/acc_kiwi01$";
    // Registration order is the only thing separating these two, so they go in the order they
    // are read below.
    fixture.stub_once(path, kiwisaver("31257.896")).await;
    fixture.stub_once(path, kiwisaver("31257.894")).await;

    async fn minor(fixture: &Fixture, config: &serde_json::Value) -> i64 {
        fixture
            .provider
            .current_balance(ctx(config, None))
            .await
            .expect("a well-formed account maps")
            .expect("Akahu always reports a balance")
            .minor
    }

    let config = config("acc_kiwi01");
    // Truncation would answer 3_125_789 to both.
    assert_eq!(minor(&fixture, &config).await, 3_125_790);
    assert_eq!(minor(&fixture, &config).await, 3_125_789);

    fixture.stop().await;
}

/// Pagination, and specifically that the second request carries the cursor the first *response*
/// returned.
///
/// The failure this closes is the quiet one: a cursor that is dropped, or read from the wrong
/// field, re-requests page one. The same hundred rows then come back every time, so a backfill
/// sweep runs to `MAX_PAGES` (or its 60-second budget), dedupes every page after the first, and —
/// on the page cap, which is deliberately a WARN rather than an error — reports success having
/// never seen the rest of the history. Asserting only that "every transaction arrived" would not
/// catch it: the stubs retire in order, so page two's rows are handed over regardless of what the
/// second request asked for. The assertion has to be on the recorded `request.uri`.
///
/// The other half is the exit condition: `cursor.next: null` ends the sweep. There is no third
/// stub, so a loop that asked again would get the proxy's 503 and fail this test loudly rather
/// than spin to `MAX_PAGES`.
#[tokio::test]
async fn a_second_page_is_fetched_with_the_cursor_the_first_page_returned() {
    let fixture = Fixture::configured().await;
    fixture
        .stub_once(
            &transactions_path(),
            page(
                &[
                    txn(
                        "trans_9001",
                        "2026-01-05T09:30:00.000Z",
                        "COUNTDOWN GREENLANE",
                        "-136.55",
                        "EFTPOS",
                    ),
                    txn(
                        "trans_9002",
                        "2026-01-05T18:02:11.000Z",
                        "KMART SYLVIA PARK",
                        "-128.37",
                        "EFTPOS",
                    ),
                ],
                Some("cursor_page_two_0001"),
            ),
        )
        .await;
    fixture
        .stub_once(
            &transactions_path(),
            page(
                &[txn(
                    "trans_9003",
                    "2026-01-06T21:14:03.000Z",
                    "REFUND",
                    "205.63",
                    "DIRECT CREDIT",
                )],
                None,
            ),
        )
        .await;

    let config = config(ACCOUNT_ID);
    let transactions = fixture
        .provider
        .fetch(ctx(&config, None))
        .await
        .expect("two well-formed pages map");

    let rows: Vec<(&str, i64)> = transactions
        .iter()
        .map(|t| (t.external_id.as_str(), t.amount_minor))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("trans_9001", -13_655),
            ("trans_9002", -12_837),
            // From page two: a sweep that stopped after one page would end here.
            ("trans_9003", 20_563),
        ],
    );

    let requests = fixture.requests().await;
    assert_eq!(
        requests.len(),
        2,
        "the sweep must stop when the cursor runs out, not ask for a page that does not exist",
    );
    assert_eq!(
        query_param(&requests[0].uri, "cursor"),
        None,
        "the first page is requested without a cursor",
    );
    assert_eq!(
        query_param(&requests[1].uri, "cursor").as_deref(),
        Some("cursor_page_two_0001"),
        "the second request must carry the cursor page one returned, or it refetches page one",
    );

    fixture.stop().await;
}

/// The incremental window: three days before the last successful sync, not the moment of it.
///
/// The overlap exists because an NZ settlement date moves as bank data trickles in — a
/// transaction posted on Friday can arrive with Thursday's date on Monday. Asking from the
/// watermark exactly would step over it, and because `sure_app::sync` advances the watermark on
/// success, that row is never requested again: a permanent, silent hole in the ledger. Nothing
/// checked this, and the edit that loses it is one character — a `+` where the `-` is — with no
/// symptom until someone reconciles a statement by hand.
///
/// The expected timestamps are written out rather than computed from the adapter's own `OVERLAP`,
/// which would make this test agree with any window at all.
#[tokio::test]
async fn an_incremental_sync_asks_from_three_days_before_the_last_one() {
    let fixture = Fixture::configured().await;
    for _ in 0..3 {
        fixture.stub_once(&transactions_path(), empty_page()).await;
    }

    async fn sync_from(
        fixture: &Fixture,
        config: &serde_json::Value,
        last_synced_at: Option<&str>,
    ) {
        fixture
            .provider
            .fetch(ctx(config, last_synced_at))
            .await
            .expect("an empty page is a successful sync");
    }

    let config = config(ACCOUNT_ID);
    sync_from(&fixture, &config, Some("2026-01-10T00:00:00Z")).await;
    // A watermark written in NZDT (+13:00), which is what a locally-formatted timestamp looks
    // like: the window is UTC, so the offset has to be applied before the subtraction.
    sync_from(&fixture, &config, Some("2026-03-02T08:15:30+13:00")).await;
    // Never synced. The whole history is wanted, so there is no `start` at all — a
    // "three days before now" fallback would import three days and then advance the watermark
    // past everything older.
    sync_from(&fixture, &config, None).await;

    let requests = fixture.requests().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        query_param(&requests[0].uri, "start").as_deref(),
        Some("2026-01-07T00:00:00.000Z"),
    );
    assert_eq!(
        query_param(&requests[1].uri, "start").as_deref(),
        Some("2026-02-26T19:15:30.000Z"),
    );
    assert_eq!(
        query_param(&requests[2].uri, "start"),
        None,
        "a first sync must ask for everything, not for a window",
    );

    fixture.stop().await;
}

/// The wire shape of an enriched row, end to end — the assertion the unit tests make against a
/// literal, made instead against a body that travelled through `akahu-client`'s own decode path.
///
/// `enriched_data` is `#[serde(flatten)]` on an `Option`, historically the pattern that silently
/// yields `None` when the inner fields are present. If that regresses, every imported
/// transaction of every sync loses its merchant and category — permanently, and with no error
/// anywhere, because an unenriched transaction is a legitimate thing to receive.
#[tokio::test]
async fn a_page_of_transactions_keeps_its_enrichment_across_the_wire() {
    let fixture = Fixture::configured().await;
    fixture
        .stub(
            &transactions_path(),
            r#"{
                "success": true,
                "items": [
                    {
                        "_id": "trans_9001",
                        "_account": "acc_spend01",
                        "_connection": "conn_bank01",
                        "created_at": "2026-01-07T02:00:00.000Z",
                        "date": "2026-01-05T09:30:00.000Z",
                        "description": "COUNTDOWN GREENLANE",
                        "amount": -136.55,
                        "type": "EFTPOS",
                        "merchant": { "_id": "merchant_0001", "name": "Countdown" },
                        "category": {
                            "_id": "nzfcc_0001",
                            "name": "Supermarkets and grocery stores",
                            "groups": {
                                "personal_finance": { "_id": "group_0001", "name": "Food" }
                            }
                        }
                    },
                    {
                        "_id": "trans_9002",
                        "_account": "acc_spend01",
                        "_connection": "conn_bank01",
                        "created_at": "2026-01-07T02:00:00.000Z",
                        "date": "2026-01-05T18:02:11.000Z",
                        "description": "KMART SYLVIA PARK",
                        "amount": -128.37,
                        "type": "EFTPOS"
                    }
                ],
                "cursor": { "next": null }
            }"#,
            None,
        )
        .await;

    let config = config(ACCOUNT_ID);
    let transactions = fixture
        .provider
        .fetch(ctx(&config, None))
        .await
        .expect("a mixed page maps");
    assert_eq!(transactions.len(), 2);

    let enriched = &transactions[0];
    assert_eq!(enriched.merchant.as_deref(), Some("Countdown"));
    let category = enriched
        .category
        .as_ref()
        .expect("the enriched row carries a category");
    assert_eq!(category.name, "Supermarkets and grocery stores");
    assert_eq!(category.group.as_deref(), Some("Food"));
    // The full timestamp reaches the ledger, not just the day, and `currency_code` stays absent
    // so the import defers to the local account's own currency.
    assert_eq!(enriched.posted_at, "2026-01-05T09:30:00+00:00");
    assert_eq!(enriched.currency_code, None);

    // And flatten swallowing the fields would show up here too: an unenriched row must stay
    // unenriched rather than pick up an empty merchant.
    assert_eq!(transactions[1].merchant, None);
    assert!(transactions[1].category.is_none());

    fixture.stop().await;
}

/// Why `akahu-client` is pinned at `0.3` — asserted, for the first time, from this repo.
///
/// A page is one deserialised value, so before 0.3 a single `"type"` Akahu had added since the
/// crate was published failed all 100 transactions it arrived with. That error propagates out of
/// `fetch`, and `sure_app::sync` only advances the watermark on success — so the next poll asked
/// for the same window, hit the same value, and imported *nothing* for that account every six
/// hours until the crate was republished. Not one bad poll: a wedge.
///
/// The unit tests make this claim against `serde_json::from_str`. This one makes it through the
/// body `akahu-client` reads off a socket and decodes itself, which is the path production uses
/// and the only one that can fail differently.
#[tokio::test]
async fn a_transaction_page_survives_a_type_this_crate_has_never_seen() {
    let fixture = Fixture::configured().await;
    fixture
        .stub(
            &transactions_path(),
            page(
                &[
                    txn(
                        "trans_9001",
                        "2026-01-05T09:30:00.000Z",
                        "COUNTDOWN GREENLANE",
                        "-136.55",
                        "EFTPOS",
                    ),
                    // Stands in for whatever Akahu adds next; all that matters is that no
                    // version of this crate has heard of it.
                    txn(
                        "trans_9002",
                        "2026-01-05T18:02:11.000Z",
                        "CARBON OFFSET",
                        "-12.34",
                        "CARBON CREDITS",
                    ),
                    txn(
                        "trans_9003",
                        "2026-01-06T21:14:03.000Z",
                        "REFUND",
                        "205.63",
                        "DIRECT CREDIT",
                    ),
                ],
                None,
            ),
            None,
        )
        .await;

    let config = config(ACCOUNT_ID);
    let transactions = fixture
        .provider
        .fetch(ctx(&config, None))
        .await
        .expect("one unrecognised type must not fail the page it arrived in");

    let ids: Vec<&str> = transactions
        .iter()
        .map(|t| t.external_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["trans_9001", "trans_9002", "trans_9003"],
        "the unrecognised row costs one field, not the page",
    );
    // The row itself is intact — `type` is the only thing lost, and this adapter never read it.
    assert_eq!(transactions[1].amount_minor, -1_234);
    assert_eq!(transactions[1].description, "CARBON OFFSET");

    fixture.stop().await;
}

/// The account-listing half of the same pin, and the worse half: an unrecognised value here used
/// to fail `list_accounts` outright, so *no* account could be linked at all — including every
/// ordinary one beside it.
///
/// Four unrecognised values in one account, because each is a separate `#[serde(other)]` fallback
/// that can regress on its own: the account `type`, its `status`, its `connection.connection_type`,
/// and one entry in `attributes`. The last two matter most. `attributes` is a list, so a
/// recognised sibling has to survive beside the unknown one or `supports_transactions` starts
/// answering `false` for an account Akahu will happily give transactions for; and an unknown
/// connection *type* must not take the connection's `name` with it, because that name is the
/// institution the account gets labelled with.
#[tokio::test]
async fn an_account_listing_survives_four_akahu_values_this_crate_has_never_seen() {
    let fixture = Fixture::configured().await;
    fixture
        .stub(
            ACCOUNTS_PATH,
            r#"{
                "success": true,
                "items": [
                    {
                        "_id": "acc_solar01",
                        "_authorisation": "auth_login01",
                        "connection": { "_id": "conn_bank01", "name": "ASB", "connection_type": "syndicated" },
                        "name": "Solar Buyback",
                        "status": "PENDING_CONSENT",
                        "refreshed": {},
                        "balance": { "current": 481.09, "currency": "NZD" },
                        "type": "SOLAR BUYBACK",
                        "attributes": ["TRANSACTIONS", "BENEFICIARY_PAYMENTS"]
                    },
                    {
                        "_id": "acc_spend01",
                        "_authorisation": "auth_login01",
                        "name": "Everyday Spending",
                        "status": "ACTIVE",
                        "refreshed": {},
                        "balance": { "current": 2480.15, "currency": "NZD" },
                        "type": "CHECKING",
                        "attributes": ["TRANSACTIONS"]
                    }
                ]
            }"#,
            None,
        )
        .await;

    let accounts = fixture
        .provider
        .list_accounts()
        .await
        .expect("one unrecognised account must not fail the listing");
    assert_eq!(accounts.len(), 2, "both accounts are still offered");

    let odd = &accounts[0];
    // `Asset` is the deliberate answer for an account whose type we cannot branch on: a generic
    // valued thing, which is all its balance actually tells us, and a prompt for the user to
    // retype the kind rather than a confident wrong guess.
    assert_eq!(odd.kind_hint, AccountKind::Asset);
    assert_eq!(odd.balance_minor, 48_109);
    assert_eq!(odd.institution.as_deref(), Some("ASB"));
    assert!(
        odd.supports_transactions,
        "a recognised attribute must survive beside an unrecognised one",
    );
    assert_eq!(accounts[1].kind_hint, AccountKind::Bank);

    fixture.stop().await;
}

/// The unconfigured install — every CI run, and everyone who does not use Akahu.
///
/// Two properties, and the second is the addition: the error names the environment variable to
/// set (pinned in-process too, and read out of a 422 body by `specs/akahu.spec.ts`), *and*
/// nothing was sent. Checking credentials after opening the socket would still produce the right
/// message while making an unauthenticated request to a real bank aggregator on every poll of a
/// provider nobody configured.
///
/// The recorder is the proof rather than an absence of stubs: an unstubbed request would be
/// answered by the proxy's 503 and recorded, so an empty recorder means no request happened.
#[tokio::test]
async fn an_unconfigured_provider_names_the_missing_variable_and_never_opens_a_socket() {
    let fixture = Fixture::start(Err(MissingToken::AppToken), Vec::new()).await;
    let config = config(ACCOUNT_ID);

    for error in [
        fixture.provider.list_accounts().await.err(),
        fixture.provider.fetch(ctx(&config, None)).await.err(),
        fixture
            .provider
            .current_balance(ctx(&config, None))
            .await
            .err(),
    ] {
        assert_eq!(
            error.expect("no credentials, no request").to_string(),
            "AKAHU_APP_TOKEN is not set",
        );
    }

    assert!(
        fixture.requests().await.is_empty(),
        "an unconfigured provider reached the network before checking its credentials",
    );

    fixture.stop().await;
}

/// Both credentials, on the wire, in the two headers Akahu actually reads.
///
/// `akahu-client` splits them — the app token in the custom `X-Akahu-Id`, the user token as the
/// bearer — and neither is visible from this side of the crate. A swap, or a missing one, is a
/// 401 from Akahu on every sync and nothing else: no local error names the header, and the
/// message that comes back is the upstream's.
///
/// This fixture registers **no** middleware on purpose. The recorder is handed the request after
/// `redact_request_for_snapshot`, so with `RedactCredentials` installed — as it is in every
/// snapshot-taking configuration — these two headers are already gone by the time anything can
/// read them. The companion test below pins that half.
#[tokio::test]
async fn both_credentials_reach_the_wire_in_the_headers_akahu_expects() {
    let fixture = Fixture::configured().await;
    fixture
        .stub(ACCOUNTS_PATH, r#"{"success":true,"items":[]}"#, None)
        .await;
    fixture
        .provider
        .list_accounts()
        .await
        .expect("an empty listing is a successful call");

    let requests = fixture.requests().await;
    let request = requests.first().expect("the listing reached the proxy");
    assert_eq!(header(request, "x-akahu-id"), Some("app_token_test"));
    assert_eq!(
        header(request, "authorization"),
        Some("Bearer user_token_test"),
    );
    assert_eq!(header(request, "accept"), Some("application/json"));

    fixture.stop().await;
}

/// …and the same call, recorded through `RedactCredentials`, carries neither.
///
/// This is what makes a recording safe to keep and safe to replay: the strip happens before the
/// bytes reach the recorder, so a token never lands anywhere to be noticed later, and a snapshot
/// taken with one developer's token still matches a lookup made with another's (the same hook
/// runs on the replay side). The `accept` header staying put is the control — the middleware
/// removes credentials, not headers.
///
/// The other half is that the *live* request still carries what Akahu will not answer without —
/// redaction is a snapshot-boundary concern, and if `handle` ever started stripping too, every
/// stub-backed test would stay green while production 401'd. "The call succeeded" is not evidence
/// of that on its own, so the stub is matched on the app-token header: it can only fire for a
/// request that still had one, which makes the absences below demonstrably the recorder's.
#[tokio::test]
async fn the_snapshot_boundary_strips_both_credentials_from_what_a_recording_would_hold() {
    let fixture = Fixture::start(credentials(), vec![shared(RedactCredentials)]).await;
    fixture
        .stub_requiring(
            ACCOUNTS_PATH,
            ("x-akahu-id", "app_token_test"),
            r#"{"success":true,"items":[]}"#,
        )
        .await;
    fixture
        .provider
        .list_accounts()
        .await
        .expect("the live request still carries the credentials it needs");

    let requests = fixture.requests().await;
    let request = requests.first().expect("the listing reached the proxy");
    assert_eq!(header(request, "x-akahu-id"), None);
    assert_eq!(header(request, "authorization"), None);
    assert_eq!(header(request, "accept"), Some("application/json"));

    fixture.stop().await;
}
