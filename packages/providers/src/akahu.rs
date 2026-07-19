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
use sure_core::AccountKind;

use super::{
    ProviderAccount, ProviderBalance, ProviderCategory, ProviderTransaction, SyncContext,
    TransactionProvider,
};

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
        "NZ bank accounts & transactions via Akahu (requires AKAHU_APP_TOKEN / AKAHU_USER_TOKEN)"
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
            out.extend(page.items.into_iter().map(map_transaction));
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
        Ok(accounts.items.into_iter().map(map_account).collect())
    }

    async fn current_balance(
        &self,
        ctx: SyncContext<'_>,
    ) -> anyhow::Result<Option<ProviderBalance>> {
        let account_id = external_account_id(ctx.config)?;
        let (client, user_token) = self.client()?;
        let resp = client.get_account(&user_token, &account_id).await?;
        Ok(Some(map_balance(&resp.item)))
    }
}

fn map_balance(a: &akahu_client::Account) -> ProviderBalance {
    ProviderBalance {
        minor: decimal_to_minor(a.balance.current),
        currency_code: a.balance.currency.code().to_string(),
        limit_minor: a.balance.limit.map(decimal_to_minor),
        institution: a.connection.as_ref().map(|c| c.name.clone()),
        initial_principal_minor: a
            .meta
            .as_ref()
            .and_then(|m| m.loan_details.as_ref())
            .and_then(|l| l.initial_principal)
            .map(decimal_to_minor),
    }
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
            reqwest_akahu::Client::new(),
            app_token,
            Some(BASE_URL.to_string()),
        );
        Ok((client, UserToken::new(user_token)))
    }
}

/// Best-effort suggestion only — the user confirms/edits the local account's `kind` when
/// linking, so this doesn't need to be exact.
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
        BankAccountKind::Foreign | BankAccountKind::Tax | BankAccountKind::Rewards => {
            AccountKind::Cash
        }
    }
}

/// Convert a decimal dollar amount to minor units (cents), rounding to the nearest cent.
fn decimal_to_minor(amount: Decimal) -> i64 {
    (amount * Decimal::from(100)).round().to_i64().unwrap_or(0)
}

fn map_account(a: akahu_client::Account) -> ProviderAccount {
    let kind_hint = map_kind_hint(&a.kind, &a.name, a.balance.limit.is_some());
    let institution = a.connection.as_ref().map(|c| c.name.clone());
    ProviderAccount {
        external_id: a.id.into_inner(),
        name: a.name,
        currency_code: a.balance.currency.code().to_string(),
        institution,
        kind_hint,
        balance_minor: decimal_to_minor(a.balance.current),
        supports_transactions: a
            .attributes
            .iter()
            .any(|attr| matches!(attr, Attribute::Transactions)),
    }
}

fn map_transaction(t: akahu_client::Transaction) -> ProviderTransaction {
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

    ProviderTransaction {
        external_id: t.id.into_inner(),
        posted_at: t.date.to_rfc3339(),
        amount_minor: decimal_to_minor(t.amount),
        // Akahu doesn't expose a distinct per-transaction currency (`amount` is already in
        // the account's own currency); let the import defer to the local account's
        // configured currency, same as the CSV provider's no-currency-column case.
        currency_code: None,
        description: t.description,
        merchant,
        category,
    }
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
        let acc = map_account(fixture_account());
        assert_eq!(acc.external_id, "acc_123");
        assert_eq!(acc.name, "Spending Account");
        assert_eq!(acc.currency_code, "NZD");
        assert_eq!(acc.kind_hint, AccountKind::Bank);
        assert_eq!(acc.balance_minor, 123_456);
        assert!(acc.supports_transactions);
        // No `connection` in this fixture — institution should be absent, not guessed.
        assert_eq!(acc.institution, None);
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
        assert_eq!(map_account(a).institution, Some("ASB".to_string()));
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
        let bal = map_balance(&a);
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
        let txn = map_transaction(t);
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
        let txn = map_transaction(t);
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
        assert_eq!(map_transaction(t).amount_minor, -450);
    }

    #[test]
    fn kind_hints_cover_every_bank_account_kind() {
        // Exercises the match arms directly so a new BankAccountKind variant added
        // upstream fails to compile here rather than silently falling through. A
        // generic name/no-limit combination that doesn't match any loan-disambiguation
        // signal.
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
        assert_eq!(
            map_kind_hint(&BankAccountKind::Tax, n, false),
            AccountKind::Cash
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Rewards, n, false),
            AccountKind::Cash
        );
        assert_eq!(
            map_kind_hint(&BankAccountKind::Wallet, n, false),
            AccountKind::Brokerage
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
        let acc = map_account(a);
        assert_eq!(acc.kind_hint, AccountKind::Mortgage);
        assert_eq!(acc.balance_minor, -47_921_483);
    }
}
