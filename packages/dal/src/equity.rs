//! Equity grants, exercises, and computed vesting status.

use chrono::{Datelike, NaiveDate, Utc};
use sqlx::FromRow;
pub use sure_core::{
    AccountEquity, EquityExercise, EquityGrant, SaveExercise, SaveGrant, VestingStatus,
};
use sure_core::{AppError, AppResult, ValuationSource};

use crate::Db;

#[derive(Debug, FromRow)]
struct EquityGrantRow {
    id: i64,
    account_id: i64,
    company: String,
    grant_date: String,
    quantity: i64,
    strike_minor: i64,
    currency_code: String,
    vest_months: i64,
    cliff_months: i64,
    unit_value_minor: Option<i64>,
    note: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<EquityGrantRow> for EquityGrant {
    fn from(r: EquityGrantRow) -> Self {
        EquityGrant {
            id: r.id,
            account_id: r.account_id,
            company: r.company,
            grant_date: r.grant_date,
            quantity: r.quantity,
            strike_minor: r.strike_minor,
            currency_code: r.currency_code,
            vest_months: r.vest_months,
            cliff_months: r.cliff_months,
            unit_value_minor: r.unit_value_minor,
            note: r.note,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct EquityExerciseRow {
    id: i64,
    grant_id: i64,
    exercise_date: String,
    quantity: i64,
    price_minor: i64,
    note: Option<String>,
    created_at: String,
}

impl From<EquityExerciseRow> for EquityExercise {
    fn from(r: EquityExerciseRow) -> Self {
        EquityExercise {
            id: r.id,
            grant_id: r.grant_id,
            exercise_date: r.exercise_date,
            quantity: r.quantity,
            price_minor: r.price_minor,
            note: r.note,
            created_at: r.created_at,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_grants(db: &Db, account_id: i64) -> AppResult<Vec<EquityGrant>> {
    Ok(sqlx::query_as::<_, EquityGrantRow>(
        "SELECT * FROM equity_grants WHERE account_id=?1 ORDER BY grant_date, id",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
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
    Ok(sqlx::query_as::<_, EquityGrantRow>(
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
    .await?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update_grant(db: &Db, id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
    validate_grant(&input)?;
    Ok(sqlx::query_as::<_, EquityGrantRow>(
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
    .ok_or(AppError::NotFound("grant"))?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_exercises(db: &Db, grant_id: i64) -> AppResult<Vec<EquityExercise>> {
    Ok(sqlx::query_as::<_, EquityExerciseRow>(
        "SELECT * FROM equity_exercises WHERE grant_id=?1 ORDER BY exercise_date, id",
    )
    .bind(grant_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_exercise(
    db: &Db,
    grant_id: i64,
    input: SaveExercise,
) -> AppResult<EquityExercise> {
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
    Ok(sqlx::query_as::<_, EquityExerciseRow>(
        "INSERT INTO equity_exercises (grant_id, exercise_date, quantity, price_minor, note)
         VALUES (?1,?2,?3,?4,?5) RETURNING *",
    )
    .bind(grant_id)
    .bind(input.exercise_date.trim())
    .bind(input.quantity)
    .bind(input.price_minor)
    .bind(&input.note)
    .fetch_one(db)
    .await?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
pub async fn grant_vesting(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<VestingStatus> {
    let grant = fetch_grant(db, id).await?;
    let as_of = as_of
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    compute_status(db, &grant, as_of).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn account_equity(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
    let as_of = as_of
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let account_ccy =
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
            .bind(id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("account"))?;
    let grants: Vec<EquityGrant> = sqlx::query_as::<_, EquityGrantRow>(
        "SELECT * FROM equity_grants WHERE account_id=?1 ORDER BY grant_date, id",
    )
    .bind(id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();
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
#[tracing::instrument(level = "debug", skip_all)]
pub async fn revalue(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
    let equity = account_equity(db, id, as_of).await?;
    sqlx::query(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1,?2,?3,?4,?5,?6)",
    )
    .bind(id)
    .bind(&equity.as_of)
    .bind(equity.total_intrinsic_minor)
    .bind(&equity.currency_code)
    .bind(ValuationSource::Equity.as_str())
    .bind("equity revaluation")
    .execute(db)
    .await?;
    Ok(equity)
}

#[tracing::instrument(level = "debug", skip_all)]
async fn compute_status(
    db: &Db,
    grant: &EquityGrant,
    as_of: NaiveDate,
) -> AppResult<VestingStatus> {
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

#[tracing::instrument(level = "debug", skip_all)]
async fn fetch_grant(db: &Db, id: i64) -> AppResult<EquityGrant> {
    Ok(
        sqlx::query_as::<_, EquityGrantRow>("SELECT * FROM equity_grants WHERE id=?1")
            .bind(id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("grant"))?
            .into(),
    )
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}
