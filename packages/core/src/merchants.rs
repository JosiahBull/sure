use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A reusable payee. Custom merchants are unique by name (case-insensitive) and can
/// carry a suggested default category.
#[derive(Debug, Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Merchant {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveMerchant {
    pub name: String,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
}
