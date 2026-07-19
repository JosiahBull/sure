//! Equity grants, exercises, and computed vesting status: wire/domain shapes.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct EquityGrant {
    pub id: i64,
    pub account_id: i64,
    pub company: String,
    pub grant_date: String,
    pub quantity: i64,
    pub strike_minor: i64,
    pub currency_code: String,
    pub vest_months: i64,
    pub cliff_months: i64,
    pub unit_value_minor: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveGrant {
    pub company: String,
    pub grant_date: String,
    pub quantity: i64,
    #[serde(default)]
    pub strike_minor: i64,
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default = "vest48")]
    pub vest_months: i64,
    #[serde(default = "cliff12")]
    pub cliff_months: i64,
    #[serde(default)]
    pub unit_value_minor: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
}
fn vest48() -> i64 {
    48
}
fn cliff12() -> i64 {
    12
}

#[derive(Debug, Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct EquityExercise {
    pub id: i64,
    pub grant_id: i64,
    pub exercise_date: String,
    pub quantity: i64,
    pub price_minor: i64,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveExercise {
    pub exercise_date: String,
    pub quantity: i64,
    #[serde(default)]
    pub price_minor: i64,
    #[serde(default)]
    pub note: Option<String>,
}

/// Vesting/exercise status of a grant as of a date.
#[derive(Debug, Serialize, ToSchema)]
pub struct VestingStatus {
    pub grant_id: i64,
    pub company: String,
    pub as_of: String,
    pub quantity: i64,
    pub vested: i64,
    pub unvested: i64,
    pub exercised: i64,
    /// Vested but not yet exercised (i.e. currently exercisable).
    pub vested_unexercised: i64,
    pub strike_minor: i64,
    pub unit_value_minor: Option<i64>,
    pub currency_code: String,
    /// Intrinsic value of vested-unexercised units: qty × max(0, unit_value − strike).
    pub intrinsic_value_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AccountEquity {
    pub account_id: i64,
    pub as_of: String,
    pub currency_code: String,
    pub grants: Vec<VestingStatus>,
    pub total_intrinsic_minor: i64,
}
