//! Scheduled adjustments: wire/domain shapes. The DAL owns the queries and the run
//! engine; these are the request/response bodies the API crate serves directly.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Cron {
    pub id: i64,
    pub name: String,
    pub account_id: i64,
    pub kind: String,
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

#[derive(Deserialize, ToSchema)]
pub struct SaveCron {
    pub name: String,
    pub account_id: i64,
    pub kind: String,
    #[serde(default)]
    pub rate_bps: Option<i64>,
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub day_of_month: Option<i64>,
    pub start_date: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}

#[derive(Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct CronRun {
    pub id: i64,
    pub cron_id: i64,
    pub period: String,
    pub kind: String,
    pub valuation_id: Option<i64>,
    pub transaction_id: Option<i64>,
    pub detail: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct CronRunResult {
    pub applied: i64,
    pub runs: Vec<CronRun>,
}
