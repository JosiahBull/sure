use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::types::SaveAccount;

#[derive(Debug, Serialize, ToSchema)]
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
    pub status: String,
    pub detail: Option<String>,
    pub created_at: String,
}
