use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A point-in-time value for an account (property price, share holding value, loan
/// balance, ...). Net-worth history is built from these plus cash-account flows.
#[derive(Debug, Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Valuation {
    pub id: i64,
    pub account_id: i64,
    pub as_of: String,
    /// Signed minor units in `currency_code`; liabilities are negative.
    pub value_minor: i64,
    pub currency_code: String,
    pub source: String,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewValuation {
    pub as_of: String,
    pub value_minor: i64,
    /// Defaults to the account's currency.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}
