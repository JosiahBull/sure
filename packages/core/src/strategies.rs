//! Investment strategies: what the household does with money it does not spend, over a window.
//!
//! See `0046_investment_strategies.sql` for why this is windowed rather than
//! singular, and why the *return* lives on the target account's assumption rather than here.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::IsoDate;

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct InvestmentStrategy {
    pub id: i64,
    pub label: String,
    pub active_from: String,
    /// `None` is open-ended.
    pub active_to: Option<String>,
    /// Share of contributing income swept each month, basis points.
    pub income_share_bps: i64,
    /// Which income contributes. `None` means all of it.
    pub income_stream_id: Option<i64>,
    /// Share of any cash raised by a sale in the same month.
    pub windfall_share_bps: i64,
    /// Pay down liabilities whose recorded annual rate exceeds this, highest first, before investing
    /// anything. `None` leaves debt alone.
    pub debt_above_bps: Option<i64>,
    /// Where the remainder goes. Its **own** growth assumption is the return — there is deliberately
    /// no rate on this type, so the figure stays visible on the Assumptions tab where it can be
    /// argued with rather than buried in a strategy row.
    pub target_account_id: i64,
    pub enabled: bool,
    pub sort_order: i64,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveInvestmentStrategy {
    pub label: String,
    #[schema(value_type = String)]
    pub active_from: IsoDate,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub active_to: Option<IsoDate>,
    #[serde(default)]
    pub income_share_bps: i64,
    #[serde(default)]
    pub income_stream_id: Option<i64>,
    #[serde(default = "all_of_it")]
    pub windfall_share_bps: i64,
    #[serde(default)]
    pub debt_above_bps: Option<i64>,
    pub target_account_id: i64,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A windfall is swept whole unless told otherwise: money that arrives once and is not directed
/// somewhere just sits in the account earning nothing, which is rarely what anyone means.
fn all_of_it() -> i64 {
    10_000
}

fn enabled_by_default() -> bool {
    true
}

impl SaveInvestmentStrategy {
    /// Everything the column constraints cannot say, so the API answers 422 with a field name
    /// instead of surfacing a constraint violation as a 500.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        if self.label.trim().is_empty() {
            problems.push("a strategy needs a label".to_string());
        }
        if !(0..=10_000).contains(&self.income_share_bps) {
            problems.push(format!(
                "income_share_bps must be between 0 and 10000, got {}",
                self.income_share_bps
            ));
        }
        if !(0..=10_000).contains(&self.windfall_share_bps) {
            problems.push(format!(
                "windfall_share_bps must be between 0 and 10000, got {}",
                self.windfall_share_bps
            ));
        }
        if self.debt_above_bps.is_some_and(|b| b < 0) {
            problems.push("debt_above_bps cannot be negative".to_string());
        }
        if let Some(to) = &self.active_to
            && to.to_string() < self.active_from.to_string()
        {
            problems.push(
                "active_to is before active_from, which is a strategy that is never active"
                    .to_string(),
            );
        }
        // A strategy that sweeps neither income nor windfalls does nothing at all, and a row that
        // does nothing is more likely a half-filled form than an intention.
        if self.income_share_bps == 0 && self.windfall_share_bps == 0 {
            problems.push(
                "a strategy that invests no share of income and no share of a windfall would do \
                 nothing"
                    .to_string(),
            );
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}
