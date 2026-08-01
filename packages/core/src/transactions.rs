use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::people::Ownership;

#[derive(Debug, Serialize, ToSchema, Clone)]
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
    /// Attribution *override*: who this one transaction belongs to, when that isn't simply
    /// its account's owner. `None` — the usual case, and what every import produces — means
    /// it follows the account (see [`crate::effective_ownership`]).
    pub ownership: Option<Ownership>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
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
    /// Attribution override; omit (or send `null`) to follow the account's owner.
    #[serde(default)]
    pub ownership: Option<Ownership>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub is_one_off: bool,
}

#[derive(Debug, Deserialize, IntoParams, Default)]
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
    /// Restrict to transactions whose *effective* attribution (override, else the account's
    /// owner) is this. Parsed from `?attributed_to=joint|<person id>` at the HTTP edge.
    pub attributed_to: Option<Ownership>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkRequest {
    pub linked_transaction_id: i64,
}

/// A partial patch applied to every transaction in `ids` at once. Each optional field
/// that is *present* is written to all of them; absent fields are left untouched. The
/// nullable id fields use a nested option so a JSON `null` (clear the value) is distinct
/// from an omitted field (leave as-is) — the same distinction the inline edits rely on.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkUpdate {
    pub ids: Vec<i64>,
    /// Present → set the category (or clear it with `null`); absent → leave unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub category_id: Option<Option<i64>>,
    /// Present → set the merchant (or clear it with `null`); absent → leave unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub merchant_id: Option<Option<i64>>,
    /// Present → set the one-off flag; absent → leave unchanged.
    #[serde(default)]
    pub is_one_off: Option<bool>,
    /// Present → override the attribution (or `null` to go back to following the account);
    /// absent → leave unchanged.
    #[serde(default, deserialize_with = "double_option")]
    pub ownership: Option<Option<Ownership>>,
}

/// The ids to delete in a single bulk request.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BulkDelete {
    pub ids: Vec<i64>,
}

/// Result of a bulk mutation: how many transactions were affected.
#[derive(Debug, Serialize, ToSchema)]
pub struct BulkResult {
    pub affected: i64,
}

/// Deserialize into `Option<Option<T>>` such that a present `null` becomes `Some(None)`
/// (an explicit clear) while an omitted field — via `#[serde(default)]` — stays `None`
/// (leave unchanged). Plain `Option<Option<T>>` can't tell the two apart on its own.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

#[derive(Debug, Deserialize, ToSchema)]
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
