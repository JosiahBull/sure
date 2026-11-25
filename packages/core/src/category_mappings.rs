use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A source category translated into a user-managed category. A null destination disables
/// the mapping without forgetting the source label on the next import.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CategoryMapping {
    pub id: i64,
    /// An adapter or upload source key, shared across its accounts (for example `akahu`).
    pub source: String,
    pub source_group: String,
    pub source_name: String,
    pub category_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveCategoryMapping {
    pub source: String,
    #[serde(default)]
    pub source_group: String,
    pub source_name: String,
    #[serde(default)]
    pub category_id: Option<i64>,
}
