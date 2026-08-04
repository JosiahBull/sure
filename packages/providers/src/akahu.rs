//! [`TransactionProvider`] backed by [Akahu](https://akahu.nz), a NZ open-banking data
//! aggregator, via the `akahu-client` crate. Unlike the keyless CSV/Frankfurter
//! providers, this one needs credentials — an app token identifying this app and a user
//! token identifying whose accounts to read — supplied via `AKAHU_APP_TOKEN` /
//! `AKAHU_USER_TOKEN` env vars (no in-app OAuth flow; Akahu's personal-app model issues a
//! static user token directly, and `AppSecret` is only needed for app-scoped endpoints we
//! don't use here). Also implements account discovery, since one set of credentials can
//! surface many bank accounts — see [`TransactionProvider::list_accounts`].

use akahu_client::{AccountId, AkahuClient, Attribute, BankAccountKind, UserToken};
use async_trait::async_trait;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use sure_app::ports::{
    ProviderBalance, ProviderCategory, ProviderTransaction, SyncContext, TransactionProvider,
};
use sure_core::{AccountKind, ProviderAccount};

const BASE_URL: &str = "https://api.akahu.io/v1";
/// Re-fetch a small window before the last successful sync, since a transaction's
/// settlement date can shift slightly as NZ bank data trickles in.
const OVERLAP: chrono::Duration = chrono::Duration::days(3);
/// Defensive cap on pagination so a cursor bug (or a very long-lived account) can't spin
/// forever; 100 txns/page per the API, so this covers 100k transactions in one sync.
const MAX_PAGES: usize = 1_000;

pub struct AkahuProvider;

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

        let mut out = Vec::new();
        let mut cursor = None;
        for page_no in 0..MAX_PAGES {
            let page = client
                .get_account_transactions(&user_token, &account_id, start, None, cursor)
                .await?;
            out.extend(page.items.into_iter().filter_map(map_transaction));
            match page.cursor.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
            if page_no == MAX_PAGES - 1 {
                tracing::warn!(account = %account_id, "Akahu transaction sync hit the page cap; some history may be missing");
            }
        }
        Ok(out)
    }

    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> {
        let (client, user_token) = self.client()?;
        let accounts = client.get_accounts(&user_token).await?;
        accounts.items.into_iter().map(map_account).collect()
    }

    async fn current_balance(
        &self,
        ctx: SyncContext<'_>,
    ) -> anyhow::Result<Option<ProviderBalance>> {
        let account_id = external_account_id(ctx.config)?;
        let (client, user_token) = self.client()?;
        let resp = client.get_account(&user_token, &account_id).await?;
        Ok(Some(map_balance(&resp.item)?))
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
    /// Build an authenticated client from env credentials. Returns a clear error naming
    /// the missing var rather than panicking, since misconfiguration is expected until the
    /// user provides their env file.
    fn client(&self) -> anyhow::Result<(AkahuClient, UserToken)> {
        let app_token = std::env::var("AKAHU_APP_TOKEN")
            .map_err(|_| anyhow::anyhow!("AKAHU_APP_TOKEN is not set"))?;
        let user_token = std::env::var("AKAHU_USER_TOKEN")
            .map_err(|_| anyhow::anyhow!("AKAHU_USER_TOKEN is not set"))?;
        let client = AkahuClient::new(
            crate::http::akahu_client(),
            app_token,
            Some(BASE_URL.to_string()),
        )
        // The one bound `crate::http` cannot apply from the outside: `akahu-client` reads the
        // response body itself, so `json_capped` never sees a `Response` to cut short. Passing
        // `MAX_BODY_BYTES` explicitly — rather than accepting the crate's own default, which
        // happens to be the same 8MiB today — is what keeps the two providers on one number
        // when either side changes its mind.
        .with_max_response_bytes(crate::http::MAX_BODY_BYTES);
        Ok((client, UserToken::new(user_token)))
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
    })
}

/// `None` for a transaction whose amount can't be represented in minor units: one bad row
/// out of a 100k-transaction history is dropped with a WARN naming it, rather than sinking
/// the whole sync (and, because a failed sync isn't recorded, re-fetching the same bad row
/// on every check from then on).
fn map_transaction(t: akahu_client::Transaction) -> Option<ProviderTransaction> {
    let Some(amount_minor) = decimal_to_minor(t.amount) else {
        tracing::warn!(
            transaction = %t.id,
            amount = %t.amount,
            "skipping Akahu transaction whose amount does not fit in minor units"
        );
        return None;
    };

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
