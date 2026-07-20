use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct Currency {
    /// ISO 4217 code (or a user code for private assets), e.g. `NZD`.
    pub code: String,
    pub name: String,
    pub symbol: String,
    /// Number of minor units per major unit (2 => cents).
    pub decimal_places: i64,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewCurrency {
    pub code: String,
    pub name: String,
    pub symbol: String,
    #[serde(default = "default_decimals")]
    pub decimal_places: i64,
}

fn default_decimals() -> i64 {
    2
}
