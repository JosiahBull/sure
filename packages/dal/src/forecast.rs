//! Forecast assumption overrides: persistence. The resolution logic (which knob wins)
//! lives in `sure-app`; this module only stores/retrieves the override rows plus the one
//! read query (`trailing_dividends_minor`) nothing else already exposes.

use sqlx::FromRow;
use sure_core::{
    AppError, AppResult, ForecastAssumption, ForecastEvent, ForecastEventKind, ForecastTargetType,
    SaveForecastAssumption, SaveForecastEvent,
};

use crate::Db;

#[derive(Debug, FromRow)]
struct ForecastAssumptionRow {
    id: i64,
    target_type: String,
    target_id: i64,
    annual_growth_bps: Option<i64>,
    annual_volatility_bps: Option<i64>,
    dividend_yield_bps: Option<i64>,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl TryFrom<ForecastAssumptionRow> for ForecastAssumption {
    type Error = AppError;

    fn try_from(r: ForecastAssumptionRow) -> AppResult<Self> {
        // Every writer goes through `ForecastTargetType::as_str`, so a value that
        // doesn't parse means the row was written by something else entirely — surface
        // it as a real error rather than panicking the request.
        let target_type: ForecastTargetType = r
            .target_type
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(ForecastAssumption {
            id: r.id,
            target_type,
            target_id: r.target_id,
            annual_growth_bps: r.annual_growth_bps,
            annual_volatility_bps: r.annual_volatility_bps,
            dividend_yield_bps: r.dividend_yield_bps,
            notes: r.notes,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// Every stored override, across both account and category targets.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_assumptions(db: &Db) -> AppResult<Vec<ForecastAssumption>> {
    sqlx::query_as::<_, ForecastAssumptionRow>("SELECT * FROM forecast_assumptions ORDER BY id")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(TryInto::try_into)
        .collect()
}

/// Insert or replace the override for `(target_type, target_id)`. A `None` field clears
/// that knob back to "derive from history" — this replaces the whole row, it doesn't
/// patch individual fields.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert_assumption(
    db: &Db,
    input: SaveForecastAssumption,
) -> AppResult<ForecastAssumption> {
    sqlx::query_as::<_, ForecastAssumptionRow>(
        "INSERT INTO forecast_assumptions
            (target_type, target_id, annual_growth_bps, annual_volatility_bps, dividend_yield_bps, notes)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(target_type, target_id) DO UPDATE SET
            annual_growth_bps=excluded.annual_growth_bps,
            annual_volatility_bps=excluded.annual_volatility_bps,
            dividend_yield_bps=excluded.dividend_yield_bps,
            notes=excluded.notes,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         RETURNING *",
    )
    .bind(input.target_type.as_str())
    .bind(input.target_id)
    .bind(input.annual_growth_bps)
    .bind(input.annual_volatility_bps)
    .bind(input.dividend_yield_bps)
    .bind(input.notes)
    .fetch_one(db)
    .await?
    .try_into()
}

/// Clear the override for `(target_type, target_id)`, if one exists — the target then
/// falls back to a cron-derived or historical default. Not an error if none was set.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn clear_assumption(
    db: &Db,
    target_type: ForecastTargetType,
    target_id: i64,
) -> AppResult<()> {
    sqlx::query("DELETE FROM forecast_assumptions WHERE target_type=?1 AND target_id=?2")
        .bind(target_type.as_str())
        .bind(target_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Sum of `dividends.net_amount_minor` for `account_id` paid on or after `since`
/// (ISO-8601 date) — the numerator for a trailing-window dividend-yield default.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn trailing_dividends_minor(db: &Db, account_id: i64, since: &str) -> AppResult<i64> {
    Ok(sqlx::query_scalar::<_, Option<i64>>(
        "SELECT SUM(net_amount_minor) FROM dividends WHERE account_id=?1 AND paid_date >= ?2",
    )
    .bind(account_id)
    .bind(since)
    .fetch_one(db)
    .await?
    .unwrap_or(0))
}

// ---- forecast events -------------------------------------------------------------

#[derive(Debug, FromRow)]
struct ForecastEventRow {
    id: i64,
    target_type: String,
    target_id: i64,
    kind: String,
    effective_date: String,
    amount_minor: i64,
    label: String,
    created_at: String,
}

impl TryFrom<ForecastEventRow> for ForecastEvent {
    type Error = AppError;

    fn try_from(r: ForecastEventRow) -> AppResult<Self> {
        let target_type: ForecastTargetType = r
            .target_type
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        let kind: ForecastEventKind = r
            .kind
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(ForecastEvent {
            id: r.id,
            target_type,
            target_id: r.target_id,
            kind,
            effective_date: r.effective_date,
            amount_minor: r.amount_minor,
            label: r.label,
            created_at: r.created_at,
        })
    }
}

/// Every known future step-change/one-off, soonest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_events(db: &Db) -> AppResult<Vec<ForecastEvent>> {
    sqlx::query_as::<_, ForecastEventRow>(
        "SELECT * FROM forecast_events ORDER BY effective_date, id",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_event(db: &Db, input: SaveForecastEvent) -> AppResult<ForecastEvent> {
    if input.label.trim().is_empty() {
        return Err(AppError::validation("label is required"));
    }
    if chrono::NaiveDate::parse_from_str(&input.effective_date, "%Y-%m-%d").is_err() {
        return Err(AppError::validation("effective_date must be YYYY-MM-DD"));
    }
    sqlx::query_as::<_, ForecastEventRow>(
        "INSERT INTO forecast_events (target_type, target_id, kind, effective_date, amount_minor, label)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING *",
    )
    .bind(input.target_type.as_str())
    .bind(input.target_id)
    .bind(input.kind.as_str())
    .bind(input.effective_date.trim())
    .bind(input.amount_minor)
    .bind(input.label.trim())
    .fetch_one(db)
    .await?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_event(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM forecast_events WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("forecast event"));
    }
    Ok(())
}
