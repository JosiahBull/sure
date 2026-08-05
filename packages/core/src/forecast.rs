//! Forecast assumption overrides: wire/domain shapes. A row's fields default to `NULL`,
//! meaning "derive this from history" — only a knob the user has actually tuned is
//! persisted here. The DAL owns the queries; [`crate::forecast`]'s resolution logic
//! (which knob wins: override, then an existing cron's rate, then a historical default)
//! lives in `sure-app`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;
use crate::money::Money;

/// What kind of thing a `forecast_assumptions` row tunes.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ForecastTargetType {
    /// An asset/investment/liability account's growth, volatility, or dividend yield.
    Account,
    /// A top-level income/expense category's growth or volatility.
    Category,
}

impl ForecastTargetType {
    pub fn as_str(self) -> &'static str {
        match self {
            ForecastTargetType::Account => "account",
            ForecastTargetType::Category => "category",
        }
    }
}

impl std::str::FromStr for ForecastTargetType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "account" => Ok(ForecastTargetType::Account),
            "category" => Ok(ForecastTargetType::Category),
            other => Err(format!("unknown forecast target type '{other}'")),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ForecastAssumption {
    pub id: i64,
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    pub annual_growth_bps: Option<i64>,
    pub annual_volatility_bps: Option<i64>,
    pub dividend_yield_bps: Option<i64>,
    /// The annual rate a *derived* growth trend decays toward beyond the window it was
    /// fitted over, in basis points. `None` reads as 0 — flat in nominal terms, which is
    /// what `AssumptionSource::InsufficientHistory` already yields, so it is the
    /// conservative claim rather than an invented one. Ignored when
    /// `annual_growth_bps` is set: that is the user asserting a rate, and an assertion is
    /// not decayed.
    pub long_run_growth_bps: Option<i64>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Upsert body, keyed by `(target_type, target_id)`. A field left `None` means "no
/// override for this knob — derive it from history"; this is a full-replace PUT, not a
/// patch, so clearing a previously-set override is just omitting it here.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveForecastAssumption {
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    #[serde(default)]
    pub annual_growth_bps: Option<i64>,
    #[serde(default)]
    pub annual_volatility_bps: Option<i64>,
    #[serde(default)]
    pub dividend_yield_bps: Option<i64>,
    #[serde(default)]
    pub long_run_growth_bps: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A known future change, applied identically across every simulated path (it's a
/// certainty the user is asserting, not a statistical estimate).
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ForecastEventKind {
    /// From `effective_date` on, replaces the ongoing baseline — a category's recurring
    /// monthly amount (a promotion), or an account's value (a known revaluation) —
    /// rather than a single month's delta.
    StepChange,
    /// A one-time delta applied only in the month containing `effective_date` (a planned
    /// bonus, a lump-sum contribution, an extra loan repayment).
    OneOffAmount,
}

impl ForecastEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ForecastEventKind::StepChange => "step_change",
            ForecastEventKind::OneOffAmount => "one_off_amount",
        }
    }
}

impl std::str::FromStr for ForecastEventKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "step_change" => Ok(ForecastEventKind::StepChange),
            "one_off_amount" => Ok(ForecastEventKind::OneOffAmount),
            other => Err(format!("unknown forecast event kind '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ForecastEvent {
    pub id: i64,
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    pub kind: ForecastEventKind,
    pub effective_date: String,
    pub amount_minor: i64,
    pub label: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveForecastEvent {
    pub target_type: ForecastTargetType,
    pub target_id: i64,
    pub kind: ForecastEventKind,
    #[schema(value_type = String)]
    pub effective_date: IsoDate,
    #[schema(value_type = i64)]
    pub amount_minor: Money,
    pub label: String,
}
