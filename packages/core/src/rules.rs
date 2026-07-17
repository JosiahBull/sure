//! Auto-classification rules: wire/domain shapes. Evaluation and persistence live in
//! the DAL and API crates; these are the request/response bodies the API crate serves.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Rule {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    /// Zen expression evaluated against a transaction context; truthy => match.
    /// Fields available: `amount`, `amount_minor`, `abs_amount`, `is_income`,
    /// `is_expense`, `description`, `merchant`, `merchant_id`, `notes`, `currency`,
    /// `account`, `account_kind`, `account_id`, `category_id`, `is_one_off`, `date`,
    /// `year`, `month`, `day`.
    pub expression: String,
    pub set_category_id: Option<i64>,
    pub set_one_off: Option<bool>,
    /// Action: assign this custom merchant on match.
    pub set_merchant_id: Option<i64>,
    pub overwrite_manual: bool,
    pub stop_on_match: bool,
    pub priority: i64,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveRule {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub expression: String,
    #[serde(default)]
    pub set_category_id: Option<i64>,
    #[serde(default)]
    pub set_one_off: Option<bool>,
    #[serde(default)]
    pub set_merchant_id: Option<i64>,
    #[serde(default)]
    pub overwrite_manual: bool,
    #[serde(default)]
    pub stop_on_match: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct RuleRun {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub kind: String,
    pub matched: i64,
    pub changed: i64,
    pub undone: bool,
    pub created_at: String,
}

/// One change from a run, enriched with the transaction it touched, for the audit log's
/// expandable diff. Category/merchant ids are resolved to names by the client; the
/// before/after ids are enough to render "Groceries → Dining" style changes.
#[derive(Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct RuleApplicationDetail {
    pub id: i64,
    pub transaction_id: i64,
    pub posted_at: String,
    pub description: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub prev_category_id: Option<i64>,
    pub new_category_id: Option<i64>,
    pub prev_merchant_id: Option<i64>,
    pub new_merchant_id: Option<i64>,
    pub prev_one_off: Option<bool>,
    pub new_one_off: Option<bool>,
    pub reverted: bool,
}

#[derive(Serialize, ToSchema)]
pub struct RunResult {
    pub run_id: i64,
    /// Transactions the rule(s) matched.
    pub matched: i64,
    /// Transactions actually changed (recorded in the audit log).
    pub changed: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct PreviewRequest {
    pub expression: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct RulePreview {
    pub matched: i64,
    pub sample: Vec<PreviewMatch>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewMatch {
    pub transaction_id: i64,
    pub posted_at: String,
    pub description: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
}
