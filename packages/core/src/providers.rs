use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::types::{AccountKind, SaveAccount};

/// Whether a sync attempt succeeded. Stored as `provider_syncs.status` (plain `TEXT`).
/// Named `SyncOutcome` rather than `SyncStatus` so `SyncOutcome::Ok` doesn't shadow
/// `Result::Ok` at use sites.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SyncOutcome {
    Ok,
    Error,
}

impl SyncOutcome {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        match self {
            SyncOutcome::Ok => "ok",
            SyncOutcome::Error => "error",
        }
    }
}

impl std::str::FromStr for SyncOutcome {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "ok" => SyncOutcome::Ok,
            "error" => SyncOutcome::Error,
            other => return Err(format!("unknown sync outcome '{other}'")),
        })
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Provider {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub account_id: i64,
    pub config: Value,
    pub enabled: bool,
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveProvider {
    pub name: String,
    pub kind: String,
    pub account_id: i64,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncRequest {
    /// Inline data for payload-based providers (e.g. CSV text).
    #[serde(default)]
    pub payload: Option<String>,
}

/// Link an upstream account (surfaced by `GET /provider-kinds/{kind}/accounts`) to a local
/// account, creating the `providers` connection in the same step. Exactly one of
/// `new_account` / `existing_account_id` must be set.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkProviderAccount {
    pub kind: String,
    /// The upstream's stable identifier for this account (`ProviderAccount::external_id`);
    /// stored as `config.external_account_id` on the created `providers` row.
    pub external_id: String,
    /// Name for the new `providers` row (not the account itself).
    pub name: String,
    /// Create a new local account for this external account.
    #[serde(default)]
    pub new_account: Option<SaveAccount>,
    /// Or attach to an already-existing local account instead.
    #[serde(default)]
    pub existing_account_id: Option<i64>,
}

/// Link several upstream accounts to a *single* local account at once — the case where one
/// real account is exposed by the source as several sibling accounts (e.g. a Sharesies
/// brokerage account surfaces one Akahu account per currency wallet). Each member becomes
/// its own `providers` row pointing at the one local account, so their transactions/
/// balances all flow into it. Exactly one of `new_account` / `existing_account_id` must be
/// set; the account is created once and every member is linked in the same transaction.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkProviderGroup {
    pub kind: String,
    /// The upstream accounts to link (must be non-empty).
    pub members: Vec<LinkGroupMember>,
    #[serde(default)]
    pub new_account: Option<SaveAccount>,
    #[serde(default)]
    pub existing_account_id: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkGroupMember {
    /// The upstream's stable identifier (`ProviderAccount::external_id`).
    pub external_id: String,
    /// Name for this member's `providers` row.
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderSync {
    pub id: i64,
    pub provider_id: i64,
    pub imported: i64,
    pub skipped: i64,
    pub status: SyncOutcome,
    pub detail: Option<String>,
    pub created_at: String,
}

/// An upstream account surfaced by a provider that supports account discovery
/// (see `sure_app::ports::TransactionProvider::list_accounts`) — not yet linked to a
/// local `Account`. Surfaced by `GET /provider-kinds/{kind}/accounts`. Lives here, with
/// the other provider API DTOs, so both the provider adapters and the OpenAPI document
/// can name it without either depending on the other.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderAccount {
    /// Stable identifier from the source; stored as `config.external_account_id` on the
    /// `providers` row once linked, and used to fetch that account's transactions.
    pub external_id: String,
    pub name: String,
    pub currency_code: String,
    /// The financial institution's display name (e.g. "ASB"), if the source reports one.
    pub institution: Option<String>,
    /// Which upstream *authorisation* (login) this account was discovered through.
    ///
    /// Not the institution: two people who each connect their own ASB login produce two
    /// values here and one institution. It is the only thing in a discovery response that
    /// separates one person's accounts from another's — Akahu reports no account-holder
    /// name (`meta.holder` is empty in practice, and `/parties` needs a permission a
    /// personal app doesn't have), so the household attribution this drives is a grouping,
    /// not a lookup. `None` for sources with no such concept.
    pub authorisation_id: Option<String>,
    /// The account number as the source formats it (e.g. `12-3456-0123456-00`), when it
    /// reports one.
    ///
    /// Two accounts under one login routinely share a name ("Emergency Fund" twice), and
    /// the *same* joint account seen through two logins can carry a different nickname in
    /// each — so this is what actually identifies an account to the person linking it.
    pub account_number: Option<String>,
    /// Best-effort suggestion for the local account's `kind`; the user confirms/edits it
    /// when linking, so an imperfect guess here isn't a correctness problem.
    pub kind_hint: AccountKind,
    pub balance_minor: i64,
    /// Whether the source can provide transaction history for this account (some upstream
    /// account types are balance-only).
    pub supports_transactions: bool,
    /// Whether more than one connected login reports this same underlying account — which
    /// is what a joint account looks like from here.
    ///
    /// **Inferred, not reported.** No feed tells us who holds an account: Akahu's
    /// `meta.holder` is empty in practice and `/parties` needs a permission a personal app
    /// doesn't have (see [`Self::authorisation_id`]). So this is derived from two logins
    /// reporting the same [`Self::account_number`] at the same institution, and an account
    /// whose co-holder has not connected their login reads as `false` — the inference can
    /// only see the logins it has.
    ///
    /// **Filled in above the adapter.** An adapter maps one upstream account at a time and
    /// cannot answer a question about the whole household, so it leaves this `false` and
    /// `sure_app::sync::SyncService::survey_accounts` — the one place the judgement is
    /// made — overwrites it. Both the discovery route and the link guard read it from
    /// there, so what the dialog shows and what the server enforces cannot drift apart.
    pub joint: bool,
}

/// Metadata about an available provider kind, surfaced via `GET /provider-kinds`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderKind {
    pub kind: String,
    pub description: String,
    pub accepts_payload: bool,
    pub supports_account_discovery: bool,
}
