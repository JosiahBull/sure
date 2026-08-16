//! [`TransactionProvider`] backed by [Akahu](https://akahu.nz), a NZ open-banking data
//! aggregator, via the `akahu-client` crate. Unlike the keyless CSV/Frankfurter
//! providers, this one needs credentials — an app token identifying this app and a user
//! token identifying whose accounts to read — read from `AKAHU_APP_TOKEN` /
//! `AKAHU_USER_TOKEN` by [`AkahuCredentials::from_env`] and *injected* (no in-app OAuth flow;
//! Akahu's personal-app model issues a static user token directly, and `AppSecret` is only
//! needed for app-scoped endpoints we don't use here). Also implements account discovery,
//! since one set of credentials can surface many bank accounts — see
//! [`TransactionProvider::list_accounts`].
//!
//! Injected rather than read here, even though this is the only file that wants them, for two
//! reasons. The composition root is documented as the only place the environment is read
//! (`sure-server`'s `config.rs`), and `client` below used to break that on *every* request;
//! and an in-process test cannot hand this provider credentials without `std::env::set_var`,
//! which mutates state shared with every other test in the binary. What must **not** move is
//! *when* a missing token is reported: see [`AkahuProvider::credentials`].

use std::time::{Duration, Instant};

use akahu_client::{AccountId, AkahuClient, AkahuError, Attribute, BankAccountKind, UserToken};
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde_json::Value;
use sure_app::ports::{
    AccountDisconnected, ProviderBalance, ProviderCategory, ProviderTransaction, SyncContext,
    TransactionProvider,
};
use sure_core::{AccountKind, IsoDate, ProviderAccount};

use crate::http::Endpoint;

/// The real API. `pub` because the composition root owns the decision of where this provider
/// points (it is the only place configuration is read) and needs a default to fall back to.
pub const DEFAULT_BASE_URL: &str = "https://api.akahu.io/v1";
/// Re-fetch a small window before the last successful sync, since a transaction's
/// settlement date can shift slightly as NZ bank data trickles in.
const OVERLAP: chrono::Duration = chrono::Duration::days(3);
/// Defensive cap on pagination so a cursor bug can't spin forever; 100 txns/page per the API,
/// so this bounds one sweep at 10,000 transactions — several years of a busy household
/// account, and about two orders of magnitude more than an incremental poll fetches.
///
/// It used to be 1_000 (100k transactions), which was a page cap standing in for the *time*
/// and *memory* caps it cannot express: 100k `ProviderTransaction`s is tens of MB held in one
/// `Vec` before a single row is written, and at the 6s-per-page ceiling those pages are 100
/// minutes of one scheduler task. [`SWEEP_BUDGET`] is the time cap; this is now only the
/// "the cursor is looping" backstop it was always described as.
const MAX_PAGES: usize = 100;
/// Wall-clock ceiling on the *whole* paginated sweep.
///
/// `crate::http`'s `REQUEST_TIMEOUT` (6s) bounds **one page**; nothing bounded the operation.
/// The failure that closes: an upstream that is slow but up — the common real one — answering
/// each page in 5s is inside every per-request limit and still spends up to
/// `MAX_PAGES` × 5s inside a single [`TransactionProvider::fetch`]. `sure-scheduler` awaits its
/// tasks sequentially, so for that whole time the exchange-rate, stock-price, balance-delta and
/// transfer-link tasks do not run at all; and on `SIGTERM` the drain gets
/// `SHUTDOWN_DRAIN_GRACE_SECS` (10s, `docs/HTTP.md`) before the task is abandoned mid-write.
///
/// Sized deliberately against that grace: it cannot be *under* it (one page alone may take
/// 6s, and a first sync legitimately needs several) so it is a small multiple instead — long
/// enough that only a genuinely sick upstream hits it, short enough that a poll which does
/// hit it costs one drain-grace overrun rather than an hour of dead background work. The
/// residual gap — a sweep already in flight when the signal lands — needs the cancellation
/// token to reach in here, which `SyncContext` does not yet carry.
const SWEEP_BUDGET: Duration = Duration::from_secs(60);

/// The two tokens Akahu needs.
pub struct AkahuCredentials {
    /// Identifies this app. Sent as `X-Akahu-Id`, a *custom* header — which is why
    /// `crate::http`'s redirect policy is `none()` (reqwest strips only the headers it
    /// recognises as credentials on a cross-host redirect) and why [`Endpoint`] will not
    /// represent a plaintext non-loopback URL.
    pub app_token: String,
    /// Identifies whose accounts to read. Sent as the bearer token.
    pub user_token: String,
}

/// Which token is missing — kept rather than discarded so the error a sync surfaces can still
/// name the variable the user has to set.
///
/// The obvious smaller shape — `Option<AkahuCredentials>` — loses exactly the piece of
/// information the message needs. "Akahu is not configured" sends someone to the README;
/// "AKAHU_APP_TOKEN is not set" *is* the fix, and `packages/api-tests/specs/akahu.spec.ts`
/// asserts on that literal substring coming back through a 422.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingToken {
    AppToken,
    UserToken,
}

impl MissingToken {
    /// The env var whose absence this is.
    pub fn env_var(self) -> &'static str {
        match self {
            MissingToken::AppToken => "AKAHU_APP_TOKEN",
            MissingToken::UserToken => "AKAHU_USER_TOKEN",
        }
    }
}

impl AkahuCredentials {
    /// Read both tokens from the environment. Called once, by the composition root.
    ///
    /// `AKAHU_APP_TOKEN` is checked **first**, deliberately: with neither set — the state of
    /// every CI run and of anyone who does not use Akahu — the reported variable is whichever
    /// is checked first, and that string is pinned by `specs/akahu.spec.ts`. Swapping these two
    /// lines is a passing build and a failing e2e suite.
    pub fn from_env() -> Result<Self, MissingToken> {
        let app_token = std::env::var("AKAHU_APP_TOKEN").map_err(|_| MissingToken::AppToken)?;
        let user_token = std::env::var("AKAHU_USER_TOKEN").map_err(|_| MissingToken::UserToken)?;
        Ok(Self {
            app_token,
            user_token,
        })
    }
}

pub struct AkahuProvider {
    endpoint: Endpoint,
    /// The composition root's result, kept unresolved rather than unwrapped there.
    ///
    /// Akahu unconfigured is the *normal* state — it is one optional integration of several —
    /// so the server has to boot without it, which rules out failing at startup. But the
    /// failure still has to arrive with a name attached when someone asks for a sync or an
    /// account listing, which rules out dropping the error. Holding the `Result` is what
    /// satisfies both: nothing looks at it until [`AkahuProvider::client`] does, and by then
    /// there is a request to fail and a message to fail it with.
    credentials: Result<AkahuCredentials, MissingToken>,
}

#[async_trait]
impl TransactionProvider for AkahuProvider {
    fn kind(&self) -> &'static str {
        "akahu"
    }

    fn description(&self) -> &'static str {
        "New Zealand bank accounts, balances and transactions, connected through Akahu"
    }

    fn supports_account_discovery(&self) -> bool {
        true
    }

    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>> {
        let account_id = external_account_id(ctx.config)?;
        let (client, user_token) = self.client()?;
        let start = ctx
            .last_synced_at
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc) - OVERLAP);

        let started = Instant::now();
        let mut out = Vec::new();
        let mut cursor = None;
        let mut pages = 0usize;
        loop {
            match next_step(started.elapsed(), pages) {
                SweepStep::Fetch => {}
                // Deliberately an error, not the partial result. `sure_app::sync` only reaches
                // `update_last_synced` when `fetch` returns `Ok`, so failing here leaves the
                // watermark where it was and the next poll asks for exactly the same window —
                // the truncation is self-healing, and the WARN/error sync row says why.
                // Returning `Ok(out)` instead would advance the watermark past history this
                // sweep never reached, leaving a permanent hole nothing would ever re-request.
                SweepStep::OutOfTime => {
                    anyhow::bail!(
                        "Akahu transaction sync for account {account_id} exceeded its \
                         {SWEEP_BUDGET:?} budget after {pages} page(s); no transactions were \
                         imported, so the next poll retries the same window"
                    );
                }
                // The page cap is the opposite case and gets the opposite answer: it is
                // *deterministic*, so failing would mean this account never imports anything
                // again, where the time budget only fails while the upstream is unwell. Keep
                // the 10,000 transactions we have (and say so loudly) — the gap it can leave
                // is only reachable on a first/backfill sync, since an incremental one asks
                // for three days.
                SweepStep::OutOfPages => {
                    tracing::warn!(
                        account = %account_id,
                        pages,
                        "Akahu transaction sync hit the page cap; some history may be missing from this sync"
                    );
                    break;
                }
            }

            let page = client
                .get_account_transactions(&user_token, &account_id, start, None, cursor)
                .await
                .map_err(|e| classify(&account_id, e))?;
            pages += 1;
            out.extend(page.items.into_iter().filter_map(map_transaction));
            match page.cursor.next {
                Some(next) => cursor = Some(next),
                // The one complete exit: the upstream says there is nothing after this page.
                None => break,
            }
        }
        Ok(out)
    }

    /// Every account these credentials can see, minus any this side of the boundary cannot
    /// represent.
    ///
    /// Dropping rather than propagating is the whole behaviour worth naming here. One account
    /// whose balance will not fit in minor units used to fail the *listing*, which is the only
    /// way into the connect dialog — so a single absurd figure on an account the user has no
    /// interest in meant no account at all could be linked, and the message said nothing about
    /// which one. Each survivor is independent of the others, so each is offered on its own
    /// merits and the ones that failed are named in the log.
    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> {
        let (client, user_token) = self.client()?;
        let accounts = client.get_accounts(&user_token).await?;
        Ok(accounts
            .items
            .into_iter()
            .filter_map(|a| {
                let id = a.id.clone();
                match map_account(a) {
                    Ok(account) => Some(account),
                    Err(e) => {
                        tracing::warn!(
                            account = %id,
                            error = %e,
                            "skipping an Akahu account this build cannot represent; the rest of \
                             the listing is still offered"
                        );
                        None
                    }
                }
            })
            .collect())
    }

    async fn current_balance(
        &self,
        ctx: SyncContext<'_>,
    ) -> anyhow::Result<Option<ProviderBalance>> {
        let account_id = external_account_id(ctx.config)?;
        let (client, user_token) = self.client()?;
        let resp = client
            .get_account(&user_token, &account_id)
            .await
            .map_err(|e| classify(&account_id, e))?;
        Ok(Some(map_balance(&resp.item)?))
    }
}

/// Turn one `akahu-client` failure into the shape [`sure_app::sync`] can act on.
///
/// Akahu answers `404` for an account it will not serve any more, and that is not a bad minute
/// at the bank: the connection behind it has been removed or has expired, or the household has
/// re-authorised the bank — and a re-authorisation mints a *new* `_id`, so the one stored in
/// this provider's `config` is gone permanently. Left as an ordinary error it recorded a failed
/// sync every six hours forever, indistinguishable in the history from a timeout, with nothing
/// anywhere saying the account had to be re-linked.
///
/// Only the message is kept from the upstream error, and it is one Akahu wrote about a resource
/// rather than about its holder — but it is still third-party text, so what reaches a durable
/// column is bounded by `sure_app::sync::sync_detail` like every other provider message.
///
/// An `if let` rather than a `match`: exactly one variant is special, so there is no wildcard
/// arm here to justify (CLAUDE.md rule 2) even though `AkahuError` is not `#[non_exhaustive]`.
fn classify(account_id: &AccountId, e: AkahuError) -> anyhow::Error {
    if let AkahuError::NotFound { message } = &e {
        return anyhow::Error::new(AccountDisconnected::new(format!(
            "Akahu no longer has account {account_id}. The bank connection behind it has been \
             removed, has expired, or has been re-authorised — a re-authorisation issues a new \
             account id, so this link cannot be repaired. Reconnect the bank in Akahu, then link \
             the account here again. Akahu said: {message}"
        )));
    }
    anyhow::Error::new(e)
}

/// What the paginated sweep does next.
///
/// A closed three-value decision rather than two `if`s inside the loop, so both ceilings are
/// unit-testable without an upstream (the alternative — a fake Akahu answering 100 slow pages —
/// is minutes of test wall-clock to check two comparisons) and so [`TransactionProvider::fetch`]
/// has to answer every case exhaustively (CLAUDE.md rule 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SweepStep {
    /// Within both ceilings: ask for the next page.
    Fetch,
    /// [`SWEEP_BUDGET`] is spent. Fatal, so the sync watermark does not advance.
    OutOfTime,
    /// [`MAX_PAGES`] pages already fetched and the cursor still has more.
    OutOfPages,
}

/// Decide from the clock and the page count alone.
///
/// Time is checked *before* pages: both being exhausted at once means the upstream was slow
/// enough to matter, and "ran out of time" is the diagnosis an operator can act on — retry —
/// where "hit the page cap" would send them looking for 10,000 missing transactions.
fn next_step(elapsed: Duration, pages_fetched: usize) -> SweepStep {
    if elapsed >= SWEEP_BUDGET {
        SweepStep::OutOfTime
    } else if pages_fetched >= MAX_PAGES {
        SweepStep::OutOfPages
    } else {
        SweepStep::Fetch
    }
}

fn map_balance(a: &akahu_client::Account) -> anyhow::Result<ProviderBalance> {
    Ok(ProviderBalance {
        minor: required_balance_minor(a)?,
        currency_code: a.balance.currency.code().to_string(),
        limit_minor: optional_minor(a.balance.limit, "balance.limit", &a.id),
        institution: a.connection.as_ref().map(|c| c.name.clone()),
        initial_principal_minor: optional_minor(
            a.meta
                .as_ref()
                .and_then(|m| m.loan_details.as_ref())
                .and_then(|l| l.initial_principal),
            "meta.loan_details.initial_principal",
            &a.id,
        ),
    })
}

/// Read and validate the Akahu account id stashed in a provider's `config` at link time.
fn external_account_id(config: &Value) -> anyhow::Result<AccountId> {
    let external_id = config
        .get("external_account_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing 'external_account_id' in provider config"))?;
    AccountId::new(external_id)
        .map_err(|e| anyhow::anyhow!("invalid Akahu account id '{external_id}': {e}"))
}

impl AkahuProvider {
    /// `endpoint` is where the API lives ([`DEFAULT_BASE_URL`] in production, a loopback proxy
    /// in a fixture); `credentials` is whatever the composition root got from the environment,
    /// error and all — see the field.
    pub fn new(endpoint: Endpoint, credentials: Result<AkahuCredentials, MissingToken>) -> Self {
        Self {
            endpoint,
            credentials,
        }
    }

    /// Build an authenticated client. Returns a clear error naming the missing var rather
    /// than panicking, since misconfiguration is expected until the user provides their env
    /// file.
    ///
    /// The message is `"{VAR} is not set"` and has to stay exactly that shape:
    /// `specs/akahu.spec.ts` asserts a discover-accounts call on an unconfigured install
    /// answers 422 with a body containing `AKAHU_APP_TOKEN`, which is how a user finds out
    /// what to do. It travels there as an `anyhow` error through `sure_app::sync`.
    fn client(&self) -> anyhow::Result<(AkahuClient, UserToken)> {
        let credentials = self
            .credentials
            .as_ref()
            .map_err(|missing| anyhow::anyhow!("{} is not set", missing.env_var()))?;
        let client = AkahuClient::new(
            crate::http::client(&self.endpoint),
            credentials.app_token.clone(),
            Some(self.endpoint.url().to_string()),
        )
        // The one bound `crate::http` cannot apply from the outside: `akahu-client` reads the
        // response body itself, so `json_capped` never sees a `Response` to cut short. Passing
        // `MAX_BODY_BYTES` explicitly — rather than accepting the crate's own default, which
        // happens to be the same 8MiB today — is what keeps the two providers on one number
        // when either side changes its mind.
        .with_max_response_bytes(crate::http::MAX_BODY_BYTES);
        Ok((client, UserToken::new(credentials.user_token.clone())))
    }
}

/// Best-effort suggestion only — the user confirms/edits the local account's `kind` when
/// linking, so this doesn't need to be exact.
// CLAUDE.md rule 2's escape hatch: `BankAccountKind` is `#[non_exhaustive]` upstream
// (`akahu-client` 0.3), so the compiler *requires* a wildcard arm here no matter how many
// variants are named — see the `Unknown` arm at the bottom for what an unrecognised type is
// taken to mean, and why that is the right answer for one we haven't seen yet either.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_kind_hint(kind: &BankAccountKind, name: &str, has_credit_limit: bool) -> AccountKind {
    match kind {
        BankAccountKind::Checking => AccountKind::Bank,
        BankAccountKind::Savings | BankAccountKind::TermDeposit => AccountKind::Savings,
        BankAccountKind::CreditCard => AccountKind::CreditCard,
        // Akahu has no Mortgage/StudentLoan/RevolvingCredit distinction — all three
        // report as plain "LOAN". The account's own name usually says exactly what a
        // mortgage or student loan is (e.g. "Prime Housing Lending", "Student Loan"), so
        // check those first. Otherwise, an ongoing `balance.limit` (checked via
        // `has_credit_limit`) is what actually distinguishes a revolving/line-of-credit
        // facility from a fixed-term loan — a term loan's ceiling is its original
        // principal, not an ongoing limit Akahu reports separately. Confirmed against a
        // real account: linking as `revolving_credit` (rather than the fallback `loan`)
        // is exactly what let `credit_limit_minor` get auto-populated on sync. Still just
        // a suggestion the user confirms when linking.
        BankAccountKind::Loan => {
            let n = name.to_lowercase();
            if n.contains("mortgage") || n.contains("housing") || n.contains("home loan") {
                AccountKind::Mortgage
            } else if n.contains("student") {
                AccountKind::StudentLoan
            } else if has_credit_limit {
                AccountKind::RevolvingCredit
            } else {
                AccountKind::Loan
            }
        }
        // A brokerage/investment platform (e.g. Sharesies) holds many tickers plus cash
        // wallets, so it maps to the multi-holding `Brokerage` kind — linking one creates
        // a Brokerage account ready for a bulk holdings import. `Wallet` is Akahu's
        // "available cash for investment or withdrawal from an investment provider" — i.e.
        // the per-currency cash wallet of a brokerage account (Sharesies exposes one per
        // currency), so it hints Brokerage too and gets grouped with its siblings by
        // institution into a single account. KiwiSaver is a single managed-fund balance
        // (no per-ticker lots to import), so it stays a plain valued-holding `shares_nz`.
        BankAccountKind::Investment | BankAccountKind::Wallet => AccountKind::Brokerage,
        BankAccountKind::Kiwisaver => AccountKind::SharesNz,
        // An IRD tax account is a running position with the department, not spendable cash:
        // it sits at zero most of the year and goes negative when provisional or terminal
        // tax falls due. Hinting `Cash` put that debt in the Cash group as a negative
        // balance — arithmetically fine (net worth buckets purely by sign) but wrong on
        // every screen that groups by class. `Liability` reads correctly in the common case,
        // and a credit balance still totals correctly from its sign.
        BankAccountKind::Tax => AccountKind::Liability,
        BankAccountKind::Foreign | BankAccountKind::Rewards => AccountKind::Cash,
        // An account type Akahu has added since `akahu-client` was published. Since 0.3 that
        // costs one field instead of the whole listing — the account still arrives with its
        // name, balance, currency and attributes, and only `type` is lost — so there is a real
        // account here to suggest a kind for.
        //
        // `Asset` is the honest suggestion: `Profile::Generic`, no required metadata, and it
        // asserts only that the thing has a value. Every alternative claims something we have
        // no evidence for — `Bank`/`Cash` says the balance is spendable and (being
        // `AccountClass::Cash`) that it is the sum of transactions we may not even be allowed
        // to fetch; `Liability` says it is owed; `Brokerage` demands a broker before it can be
        // saved. A negative balance still subtracts from net worth from its sign alone, and the
        // user retypes the kind in the connect dialog anyway — which is exactly the prompt an
        // unrecognised account should produce, rather than a confident wrong guess.
        BankAccountKind::Unknown => AccountKind::Asset,
        // Unreachable via `Deserialize` today — `#[serde(other)]` funnels everything
        // unrecognised into `Unknown` above — but `#[non_exhaustive]` means a *named* variant
        // can also appear in a future 0.3.x without a major bump, and then it lands here. Same
        // answer for the same reason: we know nothing about it beyond its balance.
        _ => AccountKind::Asset,
    }
}

/// Convert a decimal dollar amount to minor units (cents), rounding to the nearest cent.
/// `None` if it doesn't fit.
///
/// Both halves have to be checked. `Decimal`'s `Mul` **panics** on overflow
/// (`panic!("Multiplication overflowed")` — `checked_mul` is the non-panicking form), and
/// every balance, credit limit, initial principal and transaction amount here is a
/// `Decimal` deserialized straight off the wire at arbitrary precision, so a single absurd
/// value would take down whatever is driving the sync — the scheduler's provider poll
/// included. The `to_i64` then catches the values that scale without overflowing `Decimal`
/// but still don't fit an `i64` of cents.
///
/// Returning `Option` rather than an error is deliberate: this function has no idea *which*
/// account or transaction it is converting, so the caller — which does — owns the message
/// and the decision. What no caller may do is substitute a zero (as this used to, via
/// `to_i64().unwrap_or(0)`): a balance silently reported as $0.00 is indistinguishable from
/// a real one and lands straight in net worth.
fn decimal_to_minor(amount: Decimal) -> Option<i64> {
    amount.checked_mul(Decimal::from(100))?.round().to_i64()
}

/// The `balance.current` of an account, which is load-bearing: every net-worth and
/// allocation figure downstream is a sum of these, so an unrepresentable one is a hard
/// error that fails the sync (or the account listing) rather than a number nobody can tell
/// apart from a real balance.
fn required_balance_minor(a: &akahu_client::Account) -> anyhow::Result<i64> {
    decimal_to_minor(a.balance.current).ok_or_else(|| {
        anyhow::anyhow!(
            "Akahu account {} reported a balance of {} that does not fit in minor units",
            a.id,
            a.balance.current
        )
    })
}

/// A supplementary optional amount (`balance.limit`, `meta.loan_details.initial_principal`)
/// that doesn't fit is dropped with a WARN instead of failing the sync: these are already
/// `Option`, "Akahu didn't report one" is a case every caller handles, and losing a credit
/// limit is not worth losing the balance and transactions that came with it.
fn optional_minor(
    amount: Option<Decimal>,
    field: &'static str,
    account: &AccountId,
) -> Option<i64> {
    let amount = amount?;
    let minor = decimal_to_minor(amount);
    if minor.is_none() {
        tracing::warn!(
            account = %account,
            field,
            amount = %amount,
            "Akahu amount does not fit in minor units; ignoring this field"
        );
    }
    minor
}

fn map_account(a: akahu_client::Account) -> anyhow::Result<ProviderAccount> {
    let kind_hint = map_kind_hint(&a.kind, &a.name, a.balance.limit.is_some());
    let institution = a.connection.as_ref().map(|c| c.name.clone());
    // Before `a.id` is consumed below, and fatal for the same reason as in `map_balance`:
    // an account offered for linking with a bogus balance is worse than one not offered.
    let balance_minor = required_balance_minor(&a)?;
    Ok(ProviderAccount {
        external_id: a.id.into_inner(),
        name: a.name,
        currency_code: a.balance.currency.code().to_string(),
        institution,
        // Akahu's `_authorisation` is per *login*, not per institution: two people who each
        // connect their own ASB accounts share a `connection._id` and differ here. That
        // makes it the grouping the connect dialog needs to tell whose accounts are whose.
        authorisation_id: Some(a.authorisation.into_inner()),
        account_number: a.formatted_account,
        kind_hint,
        balance_minor,
        supports_transactions: a
            .attributes
            .iter()
            .any(|attr| matches!(attr, Attribute::Transactions)),
        // Neither of these is this layer's to answer. Whether a second login also reports this
        // account is a question about the whole household's connections, and the drawdown is a
        // question about the account's transaction history — not about the one account object
        // being mapped here. `sure_app::sync::SyncService` fills both in.
        joint: false,
        original_amount_hint_minor: None,
    })
}

/// `None` for a transaction this side of the boundary cannot represent: one bad row out of a
/// 10,000-transaction sweep is dropped with a WARN naming it, rather than sinking the whole
/// sync (and, because a failed sync isn't recorded, re-fetching the same bad row on every
/// check from then on).
///
/// Two such checks, and both are *upstream* values arriving unvalidated:
///
/// 1. the amount, which has to fit in minor units (see [`decimal_to_minor`]);
/// 2. the date, which has to be a plausible calendar date. Provider import does not go
///    through the API's DTOs, so [`sure_core::IsoDate`]'s range check — the one that keeps a
///    year-9999 row out at the HTTP wire edge — never saw an Akahu row. The same window is
///    applied here, at this boundary, so the two entrances agree: a single absurd `date`
///    stretches every chart's x-axis over a millennium and smears the useful part of the
///    series into a line at the left edge, which is silent data loss wearing a graph.
fn map_transaction(t: akahu_client::Transaction) -> Option<ProviderTransaction> {
    let Some(amount_minor) = decimal_to_minor(t.amount) else {
        tracing::warn!(
            transaction = %t.id,
            amount = %t.amount,
            "skipping Akahu transaction whose amount does not fit in minor units"
        );
        return None;
    };

    // `date_naive()` because the window is a calendar one: the row keeps its full RFC-3339
    // `posted_at` below (the ledger shows the time of day), and only the day is range-checked.
    if let Err(err) = IsoDate::from_date(t.date.date_naive()) {
        tracing::warn!(
            transaction = %t.id,
            date = %t.date.to_rfc3339(),
            error = %err,
            "skipping Akahu transaction whose date is outside the supported range"
        );
        return None;
    }

    // `category` and `merchant` always arrive together from Akahu's enrichment engine
    // (both fields of the same flattened `enriched_data`), so pull both from one match.
    let (merchant, category) = match t.enriched_data {
        Some(e) => (
            Some(e.merchant.name),
            Some(ProviderCategory {
                name: e.category.name.to_string(),
                group: Some(e.category.groups.personal_finance.name.to_string()),
                kind: None, // bank-feed enrichment is spending — defaults to expense
            }),
        ),
        None => (None, None),
    };

    Some(ProviderTransaction {
        external_id: t.id.into_inner(),
        posted_at: t.date.to_rfc3339(),
        amount_minor,
        // Akahu doesn't expose a distinct per-transaction currency (`amount` is already in
        // the account's own currency); let the import defer to the local account's
        // configured currency, same as the CSV provider's no-currency-column case.
        currency_code: None,
        description: t.description,
        merchant,
        category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider aimed at a port nothing is listening on: every test below that uses it
    /// asserts on a failure that happens *before* a socket is opened.
    fn unconfigured(missing: MissingToken) -> AkahuProvider {
        AkahuProvider::new(
            Endpoint::parse("http://127.0.0.1:1/v1").expect("loopback plaintext is allowed"),
            Err(missing),
        )
    }

    /// The message `specs/akahu.spec.ts` reads through a 422 body, pinned in-process where it
    /// costs microseconds instead of a Playwright run.
    ///
    /// This test is only possible *because* the credentials are injected: asserting it before
    /// meant `std::env::remove_var` inside a test binary that runs in parallel, which is a
    /// race against every other test rather than a check.
    #[test]
    fn an_unconfigured_provider_names_the_variable_the_user_must_set() {
        let err = unconfigured(MissingToken::AppToken)
            .client()
            // `err()` rather than `expect_err`, which would need `AkahuClient: Debug`.
            .err()
            .expect("no credentials, no client")
            .to_string();
        assert_eq!(err, "AKAHU_APP_TOKEN is not set");

        let err = unconfigured(MissingToken::UserToken)
            .client()
            .err()
            .expect("no credentials, no client")
            .to_string();
        assert_eq!(err, "AKAHU_USER_TOKEN is not set");
    }

    /// Fully configured, no environment touched, no upstream contacted: proof that supplying
    /// credentials is now a value a caller constructs. The client it builds is aimed at the
    /// loopback endpoint it was given, which is what a fixture needs.
    #[test]
    fn injected_credentials_build_a_client_without_the_environment() {
        let provider = AkahuProvider::new(
            Endpoint::parse("http://127.0.0.1:1/v1").expect("loopback plaintext is allowed"),
            Ok(AkahuCredentials {
                app_token: "app_token_test".to_string(),
                user_token: "user_token_test".to_string(),
            }),
        );
        assert!(provider.client().is_ok());
    }

    #[test]
    fn each_missing_token_maps_to_its_own_variable() {
        assert_eq!(MissingToken::AppToken.env_var(), "AKAHU_APP_TOKEN");
        assert_eq!(MissingToken::UserToken.env_var(), "AKAHU_USER_TOKEN");
    }

    fn fixture_account() -> akahu_client::Account {
        serde_json::from_str(
            r#"{
                "_id": "acc_123",
                "_authorisation": "auth_456",
                "name": "Spending Account",
                "status": "ACTIVE",
                "refreshed": {},
                "balance": { "current": 1234.56, "currency": "NZD" },
                "type": "CHECKING",
                "attributes": ["TRANSACTIONS", "PAYMENT_FROM"]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn maps_a_typical_account() {
        let acc = map_account(fixture_account()).expect("a representable balance maps");
        assert_eq!(acc.external_id, "acc_123");
        assert_eq!(acc.name, "Spending Account");
        assert_eq!(acc.currency_code, "NZD");
        assert_eq!(acc.kind_hint, AccountKind::Bank);
        assert_eq!(acc.balance_minor, 123_456);
        assert!(acc.supports_transactions);
        // No `connection` in this fixture — institution should be absent, not guessed.
        assert_eq!(acc.institution, None);
        assert_eq!(acc.authorisation_id, Some("auth_456".to_string()));
        // No `formatted_account` in this fixture either.
        assert_eq!(acc.account_number, None);
    }

    /// What the connect dialog leans on to tell one household member's accounts from the
    /// other's: two logins at the same bank share a `connection`, and differ only by
    /// `_authorisation`. The account number is what tells two same-named accounts apart
    /// within one login.
    #[test]
    fn two_logins_at_one_bank_are_distinguishable() {
        let account = |id: &str, auth: &str, name: &str, number: &str| {
            let json = format!(
                r#"{{
                    "_id": "{id}",
                    "_authorisation": "{auth}",
                    "connection": {{ "_id": "conn_asb", "name": "ASB", "connection_type": "official" }},
                    "name": "{name}",
                    "formatted_account": "{number}",
                    "status": "ACTIVE",
                    "refreshed": {{}},
                    "balance": {{ "current": 10.00, "currency": "NZD" }},
                    "type": "SAVINGS",
                    "attributes": []
                }}"#
            );
            map_account(serde_json::from_str::<akahu_client::Account>(&json).unwrap()).unwrap()
        };

        let mine = account("acc_1", "auth_mine", "Emergency Fund", "12-3456-0000001-51");
        let theirs = account(
            "acc_2",
            "auth_theirs",
            "Emergency Fund",
            "12-3456-0000002-51",
        );

        // Same institution, same name, same kind — the two fields added for this are the
        // only things separating them.
        assert_eq!(mine.institution, theirs.institution);
        assert_eq!(mine.name, theirs.name);
        assert_ne!(mine.authorisation_id, theirs.authorisation_id);
        assert_eq!(mine.account_number, Some("12-3456-0000001-51".to_string()));
        assert_ne!(mine.account_number, theirs.account_number);
    }

    #[test]
    fn maps_an_accounts_institution_from_its_connection() {
        let json = r#"{
            "_id": "acc_124",
            "_authorisation": "auth_456",
            "connection": {
                "_id": "conn_789",
                "name": "ASB",
                "connection_type": "official"
            },
            "name": "Everyday",
            "status": "ACTIVE",
            "refreshed": {},
            "balance": { "current": 100.00, "currency": "NZD" },
            "type": "CHECKING",
            "attributes": []
        }"#;
        let a: akahu_client::Account = serde_json::from_str(json).unwrap();
        assert_eq!(map_account(a).unwrap().institution, Some("ASB".to_string()));
    }

    #[test]
    fn maps_a_mortgages_balance_including_initial_principal_and_institution() {
        // A real mortgage response shape: a negative current balance (owed), a
        // `meta.loan_details.initial_principal` (the original amount borrowed, which the
        // crate previously dropped as an unknown field entirely), and a `connection`
        // for the institution name.
        let json = r#"{
            "_id": "acc_125",
            "_authorisation": "auth_456",
            "connection": { "_id": "conn_1", "name": "ASB", "connection_type": "official" },
            "name": "Prime Housing Lending",
            "status": "ACTIVE",
            "refreshed": {},
            "balance": { "current": -479214.83, "currency": "NZD" },
            "meta": {
                "loan_details": {
                    "purpose": "HOME",
                    "type": "TABLE",
                    "initial_principal": 485000.00
                }
            },
            "type": "LOAN",
            "attributes": []
        }"#;
        let a: akahu_client::Account = serde_json::from_str(json).unwrap();
        let bal = map_balance(&a).expect("a representable balance maps");
        assert_eq!(bal.minor, -47_921_483);
        assert_eq!(bal.currency_code, "NZD");
        assert_eq!(bal.limit_minor, None);
        assert_eq!(bal.institution, Some("ASB".to_string()));
        assert_eq!(bal.initial_principal_minor, Some(48_500_000));
    }

    #[test]
    fn maps_a_transaction_without_enrichment() {
        let json = r#"{
            "_id": "trans_790",
            "_account": "acc_123",
            "_connection": "conn_1",
            "created_at": "2026-01-06T10:00:00.000Z",
            "date": "2026-01-06T09:30:00.000Z",
            "description": "Salary",
            "amount": 2500.00,
            "type": "CREDIT"
        }"#;
        let t: akahu_client::Transaction = serde_json::from_str(json).unwrap();
        let txn = map_transaction(t).expect("a representable amount maps");
        assert_eq!(txn.external_id, "trans_790");
        assert_eq!(txn.posted_at, "2026-01-06T09:30:00+00:00");
        assert_eq!(txn.amount_minor, 250_000);
        assert_eq!(txn.currency_code, None);
        assert_eq!(txn.description, "Salary");
        assert_eq!(txn.merchant, None);
        assert!(txn.category.is_none());
    }

    #[test]
    fn maps_an_enriched_transaction_to_a_merchant_and_category() {
        // Real Akahu wire format: `category`/`merchant` are top-level siblings of the
        // transaction (flattened from `enriched_data`). `NzfccCode`/`CategoryGroup`
        // (de)serialize as their human display name (e.g. "Cafes and restaurants"), not
        // the PascalCase variant name — confirmed from the `nzfcc` crate's generated
        // Deserialize impl, not guessed.
        let json = r#"{
            "_id": "trans_792",
            "_account": "acc_123",
            "_connection": "conn_1",
            "created_at": "2026-01-05T10:00:00.000Z",
            "date": "2026-01-05T09:30:00.000Z",
            "description": "FLAT WHITE THE ROASTERY",
            "amount": -5.50,
            "type": "DEBIT",
            "merchant": { "_id": "_merchant_1", "name": "The Roastery" },
            "category": {
                "_id": "nzfcc_test1",
                "name": "Cafes and restaurants",
                "groups": {
                    "personal_finance": { "_id": "group_test1", "name": "Lifestyle" }
                }
            }
        }"#;
        let t: akahu_client::Transaction = serde_json::from_str(json).unwrap();
        let txn = map_transaction(t).expect("a representable amount maps");
        assert_eq!(txn.merchant, Some("The Roastery".to_string()));
        let category = txn
            .category
            .expect("enriched transaction should carry a category");
        assert_eq!(category.name, "Cafes and restaurants");
        assert_eq!(category.group.as_deref(), Some("Lifestyle"));
    }

    #[test]
    fn maps_a_negative_debit_amount() {
        let json = r#"{
            "_id": "trans_791",
            "_account": "acc_123",
            "_connection": "conn_1",
            "created_at": "2026-01-05T10:00:00.000Z",
            "date": "2026-01-05T09:30:00.000Z",
            "description": "Coffee",
            "amount": -4.50,
            "type": "DEBIT"
        }"#;
        let t: akahu_client::Transaction = serde_json::from_str(json).unwrap();
        assert_eq!(
            map_transaction(t)
                .expect("a representable amount maps")
                .amount_minor,
            -450
        );
    }

    /// The wedge `akahu-client` 0.3 exists to remove, seen from this side of the boundary.
    ///
    /// A page is deserialised as one value, so a single `"type"` Akahu had added since the
    /// crate was published used to fail all 100 transactions it arrived with. That failure
    /// propagates out of [`TransactionProvider::fetch`], and `sure_app::sync` only reaches
    /// `update_last_synced` on success — so the next poll asked for the same window, hit the
    /// same value, and imported nothing for that account every six hours until the crate was
    /// republished. The unrecognised type now costs exactly one field: the transaction it is
    /// attached to still maps, and `map_transaction` never looked at `type` in the first place.
    #[test]
    fn a_transaction_page_survives_one_unrecognised_type() {
        let txn = |id: &str, kind: &str, amount: &str, description: &str| {
            format!(
                r#"{{
                    "_id": "{id}",
                    "_account": "acc_123",
                    "_connection": "conn_1",
                    "created_at": "2026-01-06T10:00:00.000Z",
                    "date": "2026-01-06T09:30:00.000Z",
                    "description": "{description}",
                    "amount": {amount},
                    "type": "{kind}"
                }}"#
            )
        };
        // "CARBON CREDITS" stands in for whatever Akahu adds next; the point is only that this
        // crate has never heard of it.
        let json = format!(
            r#"{{ "success": true, "items": [{}, {}, {}], "cursor": {{ "next": null }} }}"#,
            txn("trans_794", "CREDIT", "2500.00", "Salary"),
            txn("trans_795", "CARBON CREDITS", "-12.34", "Offset purchase"),
            txn("trans_796", "DEBIT", "-4.50", "Coffee"),
        );

        let page: akahu_client::PaginatedResponse<akahu_client::Transaction> =
            serde_json::from_str(&json).expect("an unrecognised type must not fail the page");
        assert_eq!(page.items.len(), 3, "the whole page has to survive");

        // Exactly what `fetch` does with a page, so the assertion covers the mapping too.
        let mapped: Vec<_> = page.items.into_iter().filter_map(map_transaction).collect();
        assert_eq!(mapped.len(), 3, "every transaction on the page maps");
        let odd = &mapped[1];
        assert_eq!(odd.external_id, "trans_795");
        assert_eq!(odd.amount_minor, -1_234);
        assert_eq!(odd.description, "Offset purchase");
    }

    /// The account-listing half of the same problem, end to end through [`map_account`]: one
    /// account of a type Akahu added after the crate was published used to fail
    /// `list_accounts` outright, so *no* account could be linked. It now arrives with a
    /// deliberately neutral kind hint for the user to correct in the connect dialog.
    #[test]
    fn an_account_of_an_unrecognised_type_is_still_offered_for_linking() {
        let json = r#"{
            "_id": "acc_127",
            "_authorisation": "auth_456",
            "connection": { "_id": "conn_789", "name": "ASB", "connection_type": "official" },
            "name": "Carbon Credits",
            "status": "ACTIVE",
            "refreshed": {},
            "balance": { "current": 42.00, "currency": "NZD" },
            "type": "CARBON CREDITS",
            "attributes": ["TRANSACTIONS"]
        }"#;
        let a: akahu_client::Account =
            serde_json::from_str(json).expect("an unrecognised type must not fail the account");
        assert_eq!(a.kind, BankAccountKind::Unknown);

        let acc = map_account(a).expect("a representable balance maps");
        assert_eq!(acc.kind_hint, AccountKind::Asset);
        assert_eq!(acc.balance_minor, 4_200);
        assert_eq!(acc.institution, Some("ASB".to_string()));
        // The rest of the account is intact — only `type` was lost, so a recognised attribute
        // still answers correctly.
        assert!(acc.supports_transactions);
    }

    #[test]
    fn kind_hints_cover_every_bank_account_kind() {
        // Exercises the match arms directly. `BankAccountKind` is `#[non_exhaustive]` since
        // `akahu-client` 0.3, so a variant added upstream reaches `map_kind_hint`'s wildcard
        // rather than failing to compile — this test is the compensating check that every
        // variant that exists today still maps where it is supposed to. A generic
        // name/no-limit combination that doesn't match any loan-disambiguation signal.
        let n = "Everyday Account";
        assert_eq!(
            map_kind_hint(&BankAccountKind::Checking, n, false),
            AccountKind::Bank
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Savings, n, false),
            AccountKind::Savings
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::TermDeposit, n, false),
            AccountKind::Savings
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::CreditCard, n, false),
            AccountKind::CreditCard
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, n, false),
            AccountKind::Loan
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Kiwisaver, n, false),
            AccountKind::SharesNz
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Investment, n, false),
            AccountKind::Brokerage
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Foreign, n, false),
            AccountKind::Cash
        );
        // A tax account is a debt when it isn't zero, not spendable cash.
        assert_eq!(
            map_kind_hint(&BankAccountKind::Tax, n, false),
            AccountKind::Liability
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Rewards, n, false),
            AccountKind::Cash
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Wallet, n, false),
            AccountKind::Brokerage
        );
        // The decision this file makes about a type Akahu has added since: a generic valued
        // asset, which is the only thing an unrecognised account's balance actually tells us.
        // Pinned here so changing it is a deliberate edit rather than a drifting default.
        assert_eq!(
            map_kind_hint(&BankAccountKind::Unknown, n, false),
            AccountKind::Asset
        );
        // And the loan-name/credit-limit signals must not leak into it: `Unknown` is not a
        // loan, so neither a mortgage-shaped name nor an ongoing limit may change the answer.
        assert_eq!(
            map_kind_hint(&BankAccountKind::Unknown, "Prime Housing Lending", true),
            AccountKind::Asset
        );
    }

    #[test]
    fn disambiguates_akahus_generic_loan_kind_by_account_name() {
        // Real-world names Akahu returns for these products — Akahu's API has no
        // Mortgage/StudentLoan distinction, so the name is the only signal available.
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Prime Housing Lending", false),
            AccountKind::Mortgage
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Home Loan", false),
            AccountKind::Mortgage
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Mortgage", false),
            AccountKind::Mortgage
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Student loan", false),
            AccountKind::StudentLoan
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Personal Loan", false),
            AccountKind::Loan
        );
    }

    #[test]
    fn detects_revolving_credit_from_an_ongoing_credit_limit() {
        // Akahu reports both a fixed-term loan and a revolving/line-of-credit facility
        // under the same generic "LOAN" type — but only a revolving facility carries an
        // ongoing `balance.limit`, confirmed against a real account ("The Jam", an ASB
        // revolving-credit product): linking it as `revolving_credit` is exactly what let
        // its credit limit get auto-populated on sync (see `AccountKind::current_balance`).
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "The Jam", true),
            AccountKind::RevolvingCredit
        );
        // A name match still wins over the limit signal — a redraw-facility mortgage is
        // still a mortgage, not a revolving credit account.
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Prime Housing Lending", true),
            AccountKind::Mortgage
        );
        // No limit at all falls back to a plain loan, as before.
        assert_eq!(
            map_kind_hint(&BankAccountKind::Loan, "Personal Loan", false),
            AccountKind::Loan
        );
    }

    #[test]
    fn maps_a_mortgage_account_by_name() {
        let json = r#"{
            "_id": "acc_456",
            "_authorisation": "auth_456",
            "name": "Prime Housing Lending",
            "status": "ACTIVE",
            "refreshed": {},
            "balance": { "current": -479214.83, "currency": "NZD" },
            "type": "LOAN",
            "attributes": []
        }"#;
        let a: akahu_client::Account = serde_json::from_str(json).unwrap();
        let acc = map_account(a).expect("a representable balance maps");
        assert_eq!(acc.kind_hint, AccountKind::Mortgage);
        assert_eq!(acc.balance_minor, -47_921_483);
    }

    #[test]
    fn refuses_an_amount_that_will_not_fit_in_minor_units() {
        // Scaling by 100 is what gives out first: `Decimal::MAX` is ~7.9e28, so the value
        // itself is representable and the product is not. Unchecked, that multiplication
        // *panicked* (`Multiplication overflowed`) — on the scheduler's provider poll, where
        // nothing above it catches a panic, that one wire value ended all background work.
        assert_eq!(decimal_to_minor(Decimal::MAX), None);
        assert_eq!(decimal_to_minor(Decimal::MIN), None);
        // Ordinary amounts still convert: sign kept, rounded to the nearest cent, and a
        // genuine zero still reads as `Some(0)` — the case the old `unwrap_or(0)` made
        // indistinguishable from failure.
        assert_eq!(decimal_to_minor(Decimal::new(123_456, 2)), Some(123_456));
        assert_eq!(decimal_to_minor(Decimal::new(-450, 2)), Some(-450));
        assert_eq!(decimal_to_minor(Decimal::new(4_567, 3)), Some(457));
        assert_eq!(decimal_to_minor(Decimal::ZERO), Some(0));
    }

    #[test]
    fn an_unrepresentable_balance_is_an_error_not_a_zero() {
        // Set on the deserialized fixture rather than in its JSON: what matters is the
        // conversion, and a literal this large in a fixture would only be testing serde.
        let mut a = fixture_account();
        a.balance.current = Decimal::MAX;
        assert!(map_balance(&a).is_err());
        // And the same account is refused for linking rather than offered as worth $0.00.
        assert!(map_account(a).is_err());
    }

    #[test]
    fn an_unrepresentable_credit_limit_is_dropped_rather_than_fatal() {
        // A supplementary field is not worth the balance and transactions it arrived with.
        let mut a = fixture_account();
        a.balance.limit = Some(Decimal::MAX);
        let bal = map_balance(&a).expect("the balance itself is representable");
        assert_eq!(bal.minor, 123_456);
        assert_eq!(bal.limit_minor, None);
    }

    /// W-17, the sweep-budget half. Both ceilings, decided from the two numbers the loop has,
    /// so the test costs microseconds instead of the 100 slow pages the real thing would need.
    #[test]
    fn bounds_the_sweep_in_time_as_well_as_in_pages() {
        // Ordinary progress: plenty of budget left, plenty of pages left.
        assert_eq!(next_step(Duration::ZERO, 0), SweepStep::Fetch);
        assert_eq!(
            next_step(SWEEP_BUDGET - Duration::from_millis(1), 5),
            SweepStep::Fetch
        );
        // The budget is inclusive: reaching it stops the sweep rather than allowing one more
        // page (which could itself take another `REQUEST_TIMEOUT`).
        assert_eq!(next_step(SWEEP_BUDGET, 5), SweepStep::OutOfTime);
        assert_eq!(next_step(SWEEP_BUDGET * 2, 5), SweepStep::OutOfTime);
        // The page cap, likewise inclusive: `MAX_PAGES` fetched means no more.
        assert_eq!(next_step(Duration::ZERO, MAX_PAGES - 1), SweepStep::Fetch);
        assert_eq!(next_step(Duration::ZERO, MAX_PAGES), SweepStep::OutOfPages);
        // Both exhausted: time wins, because "retry" is the actionable diagnosis.
        assert_eq!(next_step(SWEEP_BUDGET, MAX_PAGES), SweepStep::OutOfTime);
    }

    /// The number the budget exists to make impossible. A slow-but-up upstream at 5s/page —
    /// inside every per-request limit — used to be able to hold one scheduler task, and so
    /// every other scheduled task, for well over an hour.
    #[test]
    fn the_worst_case_sweep_is_minutes_not_hours() {
        let slow_page = Duration::from_secs(5);
        let unbounded_worst_case = slow_page * u32::try_from(MAX_PAGES).unwrap();
        assert!(
            unbounded_worst_case > Duration::from_secs(8 * 60),
            "the page cap alone still allows a multi-minute sweep, which is the point"
        );
        assert!(
            SWEEP_BUDGET <= Duration::from_secs(60),
            "the sweep must be bounded in minutes, not tens of minutes"
        );
        // And it has to leave room for several whole pages at the 6s per-request ceiling,
        // or an ordinary first sync could never complete.
        assert!(SWEEP_BUDGET >= Duration::from_secs(6) * 5);
    }

    /// W-24: an upstream date nothing had range-checked. Provider import bypasses the API's
    /// DTOs, so `IsoDate`'s window never saw these rows — one year-9999 transaction stretches
    /// every chart's x-axis over eight millennia.
    #[test]
    fn skips_a_transaction_dated_outside_the_supported_window() {
        let txn = |id: &str, date: &str| {
            format!(
                r#"{{
                    "_id": "{id}",
                    "_account": "acc_123",
                    "_connection": "conn_1",
                    "created_at": "2026-01-06T10:00:00.000Z",
                    "date": "{date}",
                    "description": "Groceries",
                    "amount": -42.50,
                    "type": "DEBIT"
                }}"#
            )
        };
        let mapped = |id: &str, date: &str| {
            let t: akahu_client::Transaction = serde_json::from_str(&txn(id, date)).unwrap();
            map_transaction(t)
        };

        // The absurd future date the check exists for, and the mirror-image past one — a
        // mis-parsed or garbled field, not history.
        assert!(mapped("trans_797", "9999-12-31T00:00:00.000Z").is_none());
        assert!(mapped("trans_798", "1899-12-31T00:00:00.000Z").is_none());
        // The window's own edges are fine, and so is an ordinary date — the check must not
        // quietly narrow what a real feed can deliver.
        assert!(mapped("trans_799", "1900-01-01T00:00:00.000Z").is_some());
        assert!(mapped("trans_800", "2199-12-31T23:59:59.000Z").is_some());
        let ok = mapped("trans_801", "2026-01-06T09:30:00.000Z").expect("an ordinary date maps");
        // The full timestamp still reaches the ledger: only the *day* is range-checked.
        assert_eq!(ok.posted_at, "2026-01-06T09:30:00+00:00");
    }

    /// …and it is a skip, not a failure: a single bad record must not wedge the feed. (A
    /// returned `Err` here would mean the sync isn't recorded, the watermark doesn't move, and
    /// every poll from then on re-fetches the same window and dies on the same row.)
    #[test]
    fn one_absurdly_dated_row_does_not_sink_the_page() {
        let txn = |id: &str, date: &str, amount: &str| {
            format!(
                r#"{{
                    "_id": "{id}",
                    "_account": "acc_123",
                    "_connection": "conn_1",
                    "created_at": "2026-01-06T10:00:00.000Z",
                    "date": "{date}",
                    "description": "Groceries",
                    "amount": {amount},
                    "type": "DEBIT"
                }}"#
            )
        };
        let json = format!(
            r#"{{ "success": true, "items": [{}, {}, {}], "cursor": {{ "next": null }} }}"#,
            txn("trans_802", "2026-01-04T09:30:00.000Z", "-4.50"),
            txn("trans_803", "9999-01-01T00:00:00.000Z", "-12.34"),
            txn("trans_804", "2026-01-06T09:30:00.000Z", "-8.00"),
        );
        let page: akahu_client::PaginatedResponse<akahu_client::Transaction> =
            serde_json::from_str(&json).unwrap();

        // Exactly what `fetch` does with a page.
        let mapped: Vec<_> = page.items.into_iter().filter_map(map_transaction).collect();
        assert_eq!(mapped.len(), 2, "only the absurd row is dropped");
        assert_eq!(mapped[0].external_id, "trans_802");
        assert_eq!(mapped[1].external_id, "trans_804");
    }

    /// W-21, as a regression pin rather than a fix: `enriched_data` is `#[serde(flatten)]` on an
    /// `Option`, which is the pattern that historically swallowed the inner fields. It does not
    /// here — serde 1.0.228's flat-map deserializer yields `Some` when the flattened fields are
    /// present and `None` when they are not — and this test is what makes a regression (in serde,
    /// or in `akahu-client`'s model) fail loudly instead of silently dropping the merchant and
    /// category off **every** transaction of **every** sync, permanently.
    ///
    /// Asserted through a whole page containing one enriched row and one plain one, because
    /// that is how the two cases actually arrive: a mixed page, deserialised as one value.
    #[test]
    fn a_mixed_page_keeps_enrichment_on_the_rows_that_have_it() {
        let json = r#"{
            "success": true,
            "items": [
                {
                    "_id": "trans_805",
                    "_account": "acc_123",
                    "_connection": "conn_1",
                    "created_at": "2026-01-05T10:00:00.000Z",
                    "date": "2026-01-05T09:30:00.000Z",
                    "description": "FLAT WHITE THE ROASTERY",
                    "amount": -5.50,
                    "type": "DEBIT",
                    "merchant": { "_id": "_merchant_1", "name": "The Roastery" },
                    "category": {
                        "_id": "nzfcc_test1",
                        "name": "Cafes and restaurants",
                        "groups": {
                            "personal_finance": { "_id": "group_test1", "name": "Lifestyle" }
                        }
                    }
                },
                {
                    "_id": "trans_806",
                    "_account": "acc_123",
                    "_connection": "conn_1",
                    "created_at": "2026-01-06T10:00:00.000Z",
                    "date": "2026-01-06T09:30:00.000Z",
                    "description": "Salary",
                    "amount": 2500.00,
                    "type": "CREDIT"
                }
            ],
            "cursor": { "next": null }
        }"#;
        let page: akahu_client::PaginatedResponse<akahu_client::Transaction> =
            serde_json::from_str(json).expect("a mixed page deserialises");
        // The client-side half of the invariant: the flattened option is `Some` for the row
        // that carries enrichment and `None` for the one that doesn't.
        assert!(page.items[0].enriched_data.is_some());
        assert!(page.items[1].enriched_data.is_none());

        let mapped: Vec<_> = page.items.into_iter().filter_map(map_transaction).collect();
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].merchant, Some("The Roastery".to_string()));
        let category = mapped[0]
            .category
            .as_ref()
            .expect("the enriched row carries a category");
        assert_eq!(category.name, "Cafes and restaurants");
        assert_eq!(category.group.as_deref(), Some("Lifestyle"));
        // Flatten swallowing the fields would show up here too: an unenriched row must stay
        // unenriched rather than pick up an empty merchant/category.
        assert_eq!(mapped[1].merchant, None);
        assert!(mapped[1].category.is_none());
    }

    #[test]
    fn skips_a_transaction_whose_amount_will_not_fit() {
        let json = r#"{
            "_id": "trans_793",
            "_account": "acc_123",
            "_connection": "conn_1",
            "created_at": "2026-01-06T10:00:00.000Z",
            "date": "2026-01-06T09:30:00.000Z",
            "description": "Salary",
            "amount": 2500.00,
            "type": "CREDIT"
        }"#;
        let mut t: akahu_client::Transaction = serde_json::from_str(json).unwrap();
        t.amount = Decimal::MAX;
        // One unusable row is dropped; `fetch` keeps the other 99,999.
        assert!(map_transaction(t).is_none());
    }
}
