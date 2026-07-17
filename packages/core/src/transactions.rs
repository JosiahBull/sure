use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Transaction {
    pub id: i64,
    pub account_id: i64,
    pub posted_at: String,
    /// Signed minor units in `currency_code`; negative = outflow.
    pub amount_minor: i64,
    pub currency_code: String,
    pub description: String,
    /// Raw merchant text (e.g. from an import).
    pub merchant: Option<String>,
    /// Resolved custom merchant, if assigned.
    pub merchant_id: Option<i64>,
    pub notes: Option<String>,
    pub category_id: Option<i64>,
    /// Excluded from regular reports when true.
    pub is_one_off: bool,
    /// The other side of a transfer, if linked.
    pub linked_transaction_id: Option<i64>,
    pub provider: Option<String>,
    pub external_id: Option<String>,
    /// Which rule (if any) last set this transaction's category.
    pub categorized_by_rule_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveTransaction {
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    /// Defaults to the account's currency when omitted.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub merchant: Option<String>,
    #[serde(default)]
    pub merchant_id: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub is_one_off: bool,
}

#[derive(Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxQuery {
    pub account_id: Option<i64>,
    pub category_id: Option<i64>,
    /// Inclusive lower bound on the transaction date (ISO-8601).
    pub from: Option<String>,
    /// Inclusive upper bound on the transaction date (ISO-8601).
    pub to: Option<String>,
    /// When false, one-off transactions are excluded. Defaults to true.
    pub include_one_off: Option<bool>,
    /// Case-insensitive substring match on description/merchant/notes.
    pub search: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct LinkRequest {
    pub linked_transaction_id: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct TransferRequest {
    pub from_account_id: i64,
    pub to_account_id: i64,
    pub posted_at: String,
    /// Amount leaving the source account (positive minor units).
    pub from_amount_minor: i64,
    /// Amount arriving in the destination account; defaults to `from_amount_minor`
    /// (set explicitly for cross-currency transfers).
    #[serde(default)]
    pub to_amount_minor: Option<i64>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category_id: Option<i64>,
}
