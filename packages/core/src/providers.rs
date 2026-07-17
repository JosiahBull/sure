use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::types::SaveAccount;

#[derive(Serialize, ToSchema)]
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

#[derive(Deserialize, ToSchema)]
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

#[derive(Deserialize, ToSchema)]
pub struct SyncRequest {
    /// Inline data for payload-based providers (e.g. CSV text).
    #[serde(default)]
    pub payload: Option<String>,
}

/// Link an upstream account (surfaced by `GET /provider-kinds/{kind}/accounts`) to a local
/// account, creating the `providers` connection in the same step. Exactly one of
/// `new_account` / `existing_account_id` must be set.
#[derive(Deserialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct ProviderSync {
    pub id: i64,
    pub provider_id: i64,
    pub imported: i64,
    pub skipped: i64,
    pub status: String,
    pub detail: Option<String>,
    pub created_at: String,
}
