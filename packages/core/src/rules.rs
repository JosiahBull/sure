//! Auto-classification rules: wire/domain shapes. Evaluation and persistence live in
//! the DAL and API crates; these are the request/response bodies the API crate serves.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What triggered a run, and how much of the rule set it evaluated. Stored as
/// `rule_runs.kind` (plain `TEXT`).
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RuleRunKind {
    Single,
    All,
    /// Every enabled rule, over the transactions that are still uncategorised, run
    /// unattended after an import or a provider sync landed new rows. Distinct from
    /// [`RuleRunKind::All`] because the two answer different questions in the audit log:
    /// nobody pressed a button for this one, and it could only ever have *added* a
    /// category, never replaced one. See `sure_app::rules::RuleService::categorize_new`.
    Auto,
}

impl RuleRunKind {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        match self {
            RuleRunKind::Single => "single",
            RuleRunKind::All => "all",
            RuleRunKind::Auto => "auto",
        }
    }
}

impl std::str::FromStr for RuleRunKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "single" => RuleRunKind::Single,
            "all" => RuleRunKind::All,
            "auto" => RuleRunKind::Auto,
            other => return Err(format!("unknown rule run kind '{other}'")),
        })
    }
}

#[derive(Debug, Serialize, ToSchema, Clone)]
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

#[derive(Debug, Deserialize, ToSchema)]
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

#[derive(Debug, Serialize, ToSchema)]
pub struct RuleRun {
    pub id: i64,
    pub rule_id: Option<i64>,
    pub kind: RuleRunKind,
    pub matched: i64,
    pub changed: i64,
    pub undone: bool,
    pub created_at: String,
}

/// One change from a run, enriched with the transaction it touched, for the audit log's
/// expandable diff. Category/merchant ids are resolved to names by the client; the
/// before/after ids are enough to render "Groceries → Dining" style changes.
#[derive(Debug, Serialize, ToSchema)]
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

#[derive(Debug, Serialize, ToSchema)]
pub struct RunResult {
    pub run_id: i64,
    /// Transactions the rule(s) matched.
    pub matched: i64,
    /// Transactions actually changed (recorded in the audit log).
    pub changed: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PreviewRequest {
    pub expression: String,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RulePreview {
    pub matched: i64,
    pub sample: Vec<PreviewMatch>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewMatch {
    pub transaction_id: i64,
    pub posted_at: String,
    pub description: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
}
