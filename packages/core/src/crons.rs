//! Scheduled adjustments: wire/domain shapes. The DAL owns the queries and the run
//! engine; these are the request/response bodies the API crate serves directly.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;
use crate::money::Money;

/// What a scheduled adjustment does each period. Stored as `crons.kind` /
/// `cron_runs.kind` (plain `TEXT` columns).
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CronKind {
    /// Grow the account's latest valuation by `rate_bps` annually (e.g. property, shares).
    Appreciation,
    /// Shrink the account's latest valuation by `rate_bps` annually (e.g. a vehicle).
    Depreciation,
    /// Grow a loan/mortgage balance by `rate_bps` annually.
    Interest,
    /// Post a fixed `amount_minor` transaction every period.
    FixedTransaction,
}

impl CronKind {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        use CronKind::*;
        match self {
            Appreciation => "appreciation",
            Depreciation => "depreciation",
            Interest => "interest",
            FixedTransaction => "fixed_transaction",
        }
    }
}

impl std::str::FromStr for CronKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use CronKind::*;
        Ok(match s {
            "appreciation" => Appreciation,
            "depreciation" => Depreciation,
            "interest" => Interest,
            "fixed_transaction" => FixedTransaction,
            other => return Err(format!("unknown cron kind '{other}'")),
        })
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Cron {
    pub id: i64,
    pub name: String,
    pub account_id: i64,
    pub kind: CronKind,
    pub rate_bps: Option<i64>,
    pub amount_minor: Option<i64>,
    pub category_id: Option<i64>,
    pub frequency: String,
    pub day_of_month: i64,
    pub start_date: String,
    pub last_run_on: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveCron {
    pub name: String,
    pub account_id: i64,
    pub kind: CronKind,
    #[serde(default)]
    pub rate_bps: Option<i64>,
    // Required for `CronKind::FixedTransaction` and ignored otherwise (see
    // `sure_dal::crons::validate`). Bounded like every other wire-edge money figure: a cron
    // posts its amount again *every period*, so an absurd one compounds into the ledger
    // unattended rather than being one bad row someone can spot. Kept as a plain comment so
    // utoipa doesn't add a `description` and churn the generated client for no wire change.
    #[serde(default)]
    #[schema(value_type = Option<i64>)]
    pub amount_minor: Option<Money>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub day_of_month: Option<i64>,
    #[schema(value_type = String)]
    pub start_date: IsoDate,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CronRun {
    pub id: i64,
    pub cron_id: i64,
    pub period: String,
    pub kind: CronKind,
    pub valuation_id: Option<i64>,
    pub transaction_id: Option<i64>,
    pub detail: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CronRunResult {
    pub applied: i64,
    pub runs: Vec<CronRun>,
}
