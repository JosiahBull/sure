//! Expense commitments: a recurring obligation with a *stated* amount, and the mirror image of
//! [`crate::IncomeStream`] on the spending side of the ledger.
//!
//! The two are the same shape on purpose — an amount, a cadence, an escalation, a window, and a
//! category it is netted out of — because they are the same idea pointed in opposite directions,
//! and the income side already proved the arrangement works: `reconciliations` reports modelled
//! against recorded with a coverage figure, so a mis-netting is visible rather than silent.
//!
//! What is *not* shared is the payday calendar. `sure_app::income::payment_counts` enumerates
//! individual paydays so a three-payday month lands where it really lands; a rates bill has no
//! such subtlety and wants only a monthly equivalent. See the `cadence` column comment in
//! `0044_expense_commitments.sql`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{IsoDate, Money, PayFrequency};

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct ExpenseCommitment {
    pub id: i64,
    /// The category this is netted out of. May be a subcategory — the forecast resolves it to its
    /// top-level ancestor, because that is the level assumptions are made at.
    pub category_id: i64,
    pub label: String,
    /// Minor units of `currency_code`, per `cadence` period. Always positive: this type is a cost,
    /// and a signed amount would make "a negative commitment" expressible without meaning anything.
    pub amount_minor: i64,
    pub currency_code: String,
    pub cadence: PayFrequency,
    pub first_due_on: String,
    /// When the obligation ends, if it does. A fixed-term contract expiring is the single most
    /// useful thing this type can express that a fitted trend cannot represent at all.
    pub ends_on: Option<String>,
    /// Annual escalation *relative to* `settings.inflation_bps`. 0 means "rises with inflation",
    /// 300 means "inflation + 3%", -250 at a 2.5% household rate means "flat in nominal terms".
    ///
    /// Relative rather than absolute for the reason `0043_real_growth_override.sql` gives: an
    /// opinion about one bill outrunning everything else has to survive a revision of the household
    /// rate, and an absolute figure is a copy of that rate that does not get revised with it.
    pub escalation_delta_bps: i64,
    /// The merchant whose transactions *are* this commitment.
    ///
    /// Load-bearing, not decorative. With it set, the forecast removes those transactions from the
    /// series before fitting, so the residual's volatility narrows as well as its level; without
    /// it, only the level is corrected. See `sure_app::forecast`'s `CommitmentNetting`.
    pub merchant_id: Option<i64>,
    pub enabled: bool,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveExpenseCommitment {
    pub category_id: i64,
    pub label: String,
    #[schema(value_type = i64)]
    pub amount_minor: Money,
    pub currency_code: String,
    pub cadence: PayFrequency,
    #[schema(value_type = String)]
    pub first_due_on: IsoDate,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub ends_on: Option<IsoDate>,
    #[serde(default)]
    pub escalation_delta_bps: i64,
    #[serde(default)]
    pub merchant_id: Option<i64>,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A commitment you have just entered is one you are currently paying, so it counts.
fn enabled_by_default() -> bool {
    true
}

impl ExpenseCommitment {
    /// This commitment's monthly equivalent, in its own minor units.
    ///
    /// `amount × periods_per_year / 12`, so a $250 fortnightly power bill is $541.67/mo rather than
    /// $250 — the error that arrangement invites is a 2.17× understatement, and it is invited every
    /// time someone reads a bank statement showing $250 and calls it monthly.
    pub fn monthly_minor(&self) -> f64 {
        self.amount_minor as f64 * self.cadence.periods_per_year() / 12.0
    }
}
