//! Equity grants, exercises, and computed vesting status.

use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

#[derive(Serialize, FromRow, ToSchema, Clone)]
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

#[derive(Deserialize, ToSchema)]
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

#[derive(Serialize, FromRow, ToSchema)]
pub struct EquityExercise {
    pub id: i64,
    pub grant_id: i64,
    pub exercise_date: String,
    pub quantity: i64,
    pub price_minor: i64,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveExercise {
    pub exercise_date: String,
    pub quantity: i64,
    #[serde(default)]
    pub price_minor: i64,
    #[serde(default)]
    pub note: Option<String>,
}

/// Vesting/exercise status of a grant as of a date.
#[derive(Serialize, ToSchema)]
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

#[derive(Serialize, ToSchema)]
pub struct AccountEquity {
    pub account_id: i64,
    pub as_of: String,
    pub currency_code: String,
    pub grants: Vec<VestingStatus>,
    pub total_intrinsic_minor: i64,
}

pub async fn list_grants(db: &Db, account_id: i64) -> AppResult<Vec<EquityGrant>> {
    Ok(sqlx::query_as::<_, EquityGrant>(
        "SELECT * FROM equity_grants WHERE account_id=?1 ORDER BY grant_date, id",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?)
}

pub async fn create_grant(db: &Db, account_id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
    let account_ccy =
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
            .bind(account_id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("account"))?;
    validate_grant(&input)?;
    let ccy = input
        .currency_code
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .unwrap_or(account_ccy);
    Ok(sqlx::query_as::<_, EquityGrant>(
        "INSERT INTO equity_grants
            (account_id, company, grant_date, quantity, strike_minor, currency_code,
             vest_months, cliff_months, unit_value_minor, note)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) RETURNING *",
    )
    .bind(account_id)
    .bind(input.company.trim())
    .bind(input.grant_date.trim())
    .bind(input.quantity)
    .bind(input.strike_minor)
    .bind(ccy)
    .bind(input.vest_months.max(1))
    .bind(input.cliff_months.max(0))
    .bind(input.unit_value_minor)
    .bind(&input.note)
    .fetch_one(db)
    .await?)
}

pub async fn update_grant(db: &Db, id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
    validate_grant(&input)?;
    sqlx::query_as::<_, EquityGrant>(
        "UPDATE equity_grants SET company=?2, grant_date=?3, quantity=?4, strike_minor=?5,
            vest_months=?6, cliff_months=?7, unit_value_minor=?8, note=?9,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.company.trim())
    .bind(input.grant_date.trim())
    .bind(input.quantity)
    .bind(input.strike_minor)
    .bind(input.vest_months.max(1))
    .bind(input.cliff_months.max(0))
    .bind(input.unit_value_minor)
    .bind(&input.note)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("grant"))
}

pub async fn delete_grant(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM equity_grants WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("grant"));
    }
    Ok(())
}

pub async fn list_exercises(db: &Db, grant_id: i64) -> AppResult<Vec<EquityExercise>> {
    Ok(sqlx::query_as::<_, EquityExercise>(
        "SELECT * FROM equity_exercises WHERE grant_id=?1 ORDER BY exercise_date, id",
    )
    .bind(grant_id)
    .fetch_all(db)
    .await?)
}

pub async fn create_exercise(db: &Db, grant_id: i64, input: SaveExercise) -> AppResult<EquityExercise> {
    let grant = fetch_grant(db, grant_id).await?;
    if input.quantity <= 0 {
        return Err(AppError::validation("exercise quantity must be positive"));
    }
    let as_of = parse_date(&input.exercise_date).unwrap_or_else(|| Utc::now().date_naive());
    let status = compute_status(db, &grant, as_of).await?;
    if input.quantity > status.vested_unexercised {
        return Err(AppError::validation(format!(
            "only {} vested & unexercised units available",
            status.vested_unexercised
        )));
    }
    Ok(sqlx::query_as::<_, EquityExercise>(
        "INSERT INTO equity_exercises (grant_id, exercise_date, quantity, price_minor, note)
         VALUES (?1,?2,?3,?4,?5) RETURNING *",
    )
    .bind(grant_id)
    .bind(input.exercise_date.trim())
    .bind(input.quantity)
    .bind(input.price_minor)
    .bind(&input.note)
    .fetch_one(db)
    .await?)
}

pub async fn delete_exercise(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM equity_exercises WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("exercise"));
    }
    Ok(())
}

pub async fn grant_vesting(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<VestingStatus> {
    let grant = fetch_grant(db, id).await?;
    let as_of = as_of.and_then(parse_date).unwrap_or_else(|| Utc::now().date_naive());
    compute_status(db, &grant, as_of).await
}

pub async fn account_equity(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
    let as_of = as_of.and_then(parse_date).unwrap_or_else(|| Utc::now().date_naive());
    let account_ccy =
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
            .bind(id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("account"))?;
    let grants = sqlx::query_as::<_, EquityGrant>(
        "SELECT * FROM equity_grants WHERE account_id=?1 ORDER BY grant_date, id",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    let mut statuses = Vec::new();
    let mut total = 0i64;
    for g in &grants {
        let s = compute_status(db, g, as_of).await?;
        total += s.intrinsic_value_minor;
        statuses.push(s);
    }
    Ok(AccountEquity {
        account_id: id,
        as_of: as_of.to_string(),
        currency_code: account_ccy,
        grants: statuses,
        total_intrinsic_minor: total,
    })
}

/// Snapshot the account's current equity intrinsic value into a valuation.
pub async fn revalue(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
    let equity = account_equity(db, id, as_of).await?;
    sqlx::query(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1,?2,?3,?4,'equity','equity revaluation')",
    )
    .bind(id)
    .bind(&equity.as_of)
    .bind(equity.total_intrinsic_minor)
    .bind(&equity.currency_code)
    .execute(db)
    .await?;
    Ok(equity)
}

async fn compute_status(db: &Db, grant: &EquityGrant, as_of: NaiveDate) -> AppResult<VestingStatus> {
    let grant_date = parse_date(&grant.grant_date).unwrap_or(as_of);
    let elapsed = months_between(grant_date, as_of);
    let vested = if elapsed < grant.cliff_months {
        0
    } else {
        let capped = elapsed.min(grant.vest_months);
        ((grant.quantity as i128 * capped as i128) / grant.vest_months.max(1) as i128) as i64
    }
    .clamp(0, grant.quantity);

    let exercised: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT SUM(quantity) FROM equity_exercises WHERE grant_id=?1 AND exercise_date <= ?2",
    )
    .bind(grant.id)
    .bind(as_of.to_string())
    .fetch_one(db)
    .await?
    .unwrap_or(0);

    let vested_unexercised = (vested - exercised).max(0);
    let per_unit_gain = grant
        .unit_value_minor
        .map(|v| (v - grant.strike_minor).max(0))
        .unwrap_or(0);
    let intrinsic = vested_unexercised * per_unit_gain;

    Ok(VestingStatus {
        grant_id: grant.id,
        company: grant.company.clone(),
        as_of: as_of.to_string(),
        quantity: grant.quantity,
        vested,
        unvested: grant.quantity - vested,
        exercised,
        vested_unexercised,
        strike_minor: grant.strike_minor,
        unit_value_minor: grant.unit_value_minor,
        currency_code: grant.currency_code.clone(),
        intrinsic_value_minor: intrinsic,
    })
}

fn months_between(from: NaiveDate, to: NaiveDate) -> i64 {
    if to < from {
        return 0;
    }
    let mut months =
        (to.year() - from.year()) as i64 * 12 + (to.month() as i64 - from.month() as i64);
    if to.day() < from.day() {
        months -= 1;
    }
    months.max(0)
}

fn validate_grant(input: &SaveGrant) -> AppResult<()> {
    if input.company.trim().is_empty() {
        return Err(AppError::validation("company is required"));
    }
    if input.quantity <= 0 {
        return Err(AppError::validation("quantity must be positive"));
    }
    if parse_date(&input.grant_date).is_none() {
        return Err(AppError::validation("grant_date must be YYYY-MM-DD"));
    }
    Ok(())
}

async fn fetch_grant(db: &Db, id: i64) -> AppResult<EquityGrant> {
    sqlx::query_as::<_, EquityGrant>("SELECT * FROM equity_grants WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("grant"))
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}
