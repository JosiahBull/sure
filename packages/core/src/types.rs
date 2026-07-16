use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The kinds of financial account Sure understands. `kind` selects type-specific
/// behaviour (how balance and net-worth contribution are computed); free-form
/// per-kind configuration lives in an account's `metadata`.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum AccountKind {
    Cash,
    Bank,
    Savings,
    CreditCard,
    RevolvingCredit,
    Mortgage,
    StudentLoan,
    Loan,
    Vehicle,
    RealEstate,
    SharesNz,
    SharesUs,
    SharesPrivate,
    Asset,
    Liability,
}

/// Broad grouping used by net-worth and balance logic.
#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AccountClass {
    /// Spendable cash; balance = sum of transactions.
    Cash,
    /// Valued holding (property, vehicle); net worth = latest valuation.
    Asset,
    /// Valued investment (shares); net worth = latest valuation.
    Investment,
    /// Owed money; contributes negatively to net worth.
    Liability,
}

impl AccountKind {
    pub fn class(self) -> AccountClass {
        use AccountKind::*;
        match self {
            Cash | Bank | Savings => AccountClass::Cash,
            CreditCard | RevolvingCredit | Mortgage | StudentLoan | Loan | Liability => {
                AccountClass::Liability
            }
            Vehicle | RealEstate | Asset => AccountClass::Asset,
            SharesNz | SharesUs | SharesPrivate => AccountClass::Investment,
        }
    }
}

/// Map a stored `kind` string to its class label, for callers that only have the
/// text (e.g. dynamic report queries).
pub fn class_of(kind: &str) -> &'static str {
    match kind {
        "cash" | "bank" | "savings" => "cash",
        "credit_card" | "revolving_credit" | "mortgage" | "student_loan" | "loan"
        | "liability" => "liability",
        "shares_nz" | "shares_us" | "shares_private" => "investment",
        _ => "asset",
    }
}
