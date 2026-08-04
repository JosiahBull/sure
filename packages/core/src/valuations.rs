use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;
use crate::money::Money;

/// Where a valuation's value came from. Stored as `valuations.source` (a plain `TEXT`
/// column — see `0001_core.sql`, whose comment only lists `manual`/`cron`/`provider`;
/// `brokerage` (`sure_dal::valuations::upsert_from_brokerage`) and `equity`
/// (`sure_dal::equity::revalue`) are written too and belong here just the same).
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ValuationSource {
    /// Entered by hand through the account/valuation form.
    Manual,
    /// Written by a scheduled appreciation/depreciation/interest cron.
    Cron,
    /// Synced from a linked provider's reported balance.
    Provider,
    /// Computed from a brokerage account's holdings + wallet cash.
    Brokerage,
    /// Snapshotted from an equity grant's computed intrinsic value.
    Equity,
}

impl ValuationSource {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        use ValuationSource::*;
        match self {
            Manual => "manual",
            Cron => "cron",
            Provider => "provider",
            Brokerage => "brokerage",
            Equity => "equity",
        }
    }
}

impl std::str::FromStr for ValuationSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use ValuationSource::*;
        Ok(match s {
            "manual" => Manual,
            "cron" => Cron,
            "provider" => Provider,
            "brokerage" => Brokerage,
            "equity" => Equity,
            other => return Err(format!("unknown valuation source '{other}'")),
        })
    }
}

/// A point-in-time value for an account (property price, share holding value, loan
/// balance, ...). Net-worth history is built from these plus cash-account flows.
#[derive(Debug, Serialize, ToSchema)]
pub struct Valuation {
    pub id: i64,
    pub account_id: i64,
    pub as_of: String,
    /// Signed minor units in `currency_code`; liabilities are negative.
    pub value_minor: i64,
    pub currency_code: String,
    pub source: ValuationSource,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewValuation {
    #[schema(value_type = String)]
    pub as_of: IsoDate,
    #[schema(value_type = i64)]
    pub value_minor: Money,
    /// Defaults to the account's currency.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}
