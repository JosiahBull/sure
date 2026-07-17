use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

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
