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

/// Ceiling on an explicit `annual_volatility_bps` override, 300%/yr in basis points.
///
/// Volatility is the standard deviation of a lognormal monthly draw, so it sets the
/// *exponent* the simulation raises `e` to: `exp()` saturates past ±745, and an
/// `annual_volatility_bps` in the millions makes both an underflow to `0.0` and an overflow
/// to `inf` routine within a single path. `0.0 * inf` is `NaN`, `NaN >= 0.0` is false so it
/// files itself as a liability, and it reaches the percentile bands — which used to sort with
/// `partial_cmp().unwrap()` and panic. `CatchPanicLayer` turned that into a 500 on
/// `GET /api/forecast` that persisted until the offending row was deleted, through an endpoint
/// on the page that was down.
///
/// So it is rejected here rather than silently clamped: the user typed a number and is told
/// it is out of range, instead of getting a projection quietly computed from a different one.
/// `sure_app::forecast` clamps the same value at the use site regardless — that clamp is the
/// last line of defence for a row written before this validation existed, and for one written
/// by anything that isn't this function. The bound matches
/// `sure_app::forecast::MAX_DERIVED_CATEGORY_VOL_BPS`, which is a numerical guard rather than
/// an opinion about how lumpy a real series can be: measured category volatilities do reach
/// several hundred percent and that is a true description of them.
const MAX_VOLATILITY_BPS: i64 = 30_000;

/// Insert or replace the override for `(target_type, target_id)`. A `None` field clears
/// that knob back to "derive from history" — this replaces the whole row, it doesn't
/// patch individual fields.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert_assumption(
    db: &Db,
    input: SaveForecastAssumption,
) -> AppResult<ForecastAssumption> {
    // Validated on the way in, like `create_event` below: nothing downstream can tell a
    // deliberate 1e14 from a fat-fingered one, and by the time it reaches the simulation the
    // only honest options left are clamping it (a projection of an assumption the user never
    // made) or refusing the whole report.
    if let Some(vol) = input.annual_volatility_bps {
        if !(0..=MAX_VOLATILITY_BPS).contains(&vol) {
            return Err(AppError::validation(format!(
                "annual_volatility_bps must be between 0 and {MAX_VOLATILITY_BPS} \
                 (0-300%/yr), got {vol}"
            )));
        }
    }
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
    sqlx::query_as::<_, ForecastEventRow>(
        "INSERT INTO forecast_events (target_type, target_id, kind, effective_date, amount_minor, label)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING *",
    )
    .bind(input.target_type.as_str())
    .bind(input.target_id)
    .bind(input.kind.as_str())
    .bind(input.effective_date.to_string())
    .bind(input.amount_minor.minor())
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn test_db() -> Db {
        // A single connection so all queries hit the same in-memory database — a pool
        // with >1 connection would give each connection its own empty :memory: db.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    fn assumption(annual_volatility_bps: Option<i64>) -> SaveForecastAssumption {
        SaveForecastAssumption {
            target_type: ForecastTargetType::Account,
            target_id: 1,
            annual_growth_bps: Some(700),
            annual_volatility_bps,
            dividend_yield_bps: None,
            notes: None,
        }
    }

    /// The `GET /api/forecast` permanent-500 guard. A volatility this large makes both an
    /// `exp()` underflow to `0.0` and an overflow to `inf` routine inside one simulated path;
    /// `0.0 * inf` is `NaN`, and a `NaN` used to reach the percentile sort and panic the
    /// request. Refused on the way *in*, so the forecast page can never be taken down by a
    /// row only reachable through a control on that same page.
    #[tokio::test]
    async fn refuses_a_volatility_that_would_overflow_the_simulation() {
        let db = test_db().await;
        let err = upsert_assumption(&db, assumption(Some(10_000_000_000)))
            .await
            .expect_err("an absurd volatility must be refused, not stored");
        assert!(
            matches!(err, AppError::Validation(ref m) if m.contains("annual_volatility_bps")),
            "expected a validation error naming the field, got {err:?}"
        );
        // …and nothing was written, so a retry with a sane value is the whole recovery.
        assert!(list_assumptions(&db).await.unwrap().is_empty());
    }

    /// Negative variance is not a thing. The use-site clamp used to absorb it silently, which
    /// meant a typed minus sign produced a projection with no noise at all rather than a
    /// complaint.
    #[tokio::test]
    async fn refuses_a_negative_volatility() {
        let db = test_db().await;
        assert!(matches!(
            upsert_assumption(&db, assumption(Some(-1))).await,
            Err(AppError::Validation(_))
        ));
    }

    /// The bound is a numerical guard, not an opinion: everything up to it — including the
    /// 300%/yr a genuinely lumpy category really does measure — still stores, and `None`
    /// (derive from history) is untouched by the check.
    #[tokio::test]
    async fn accepts_every_usable_volatility_including_the_ceiling() {
        let db = test_db().await;
        for vol in [None, Some(0), Some(1_500), Some(MAX_VOLATILITY_BPS)] {
            let saved = upsert_assumption(&db, assumption(vol))
                .await
                .unwrap_or_else(|e| panic!("{vol:?} should be accepted: {e:?}"));
            assert_eq!(saved.annual_volatility_bps, vol);
        }
    }

    /// An explicit *growth* override is deliberately unbounded here — that is the user
    /// asserting something about returns, and `sure_app::forecast` clamps it into a safe log
    /// return at the use site. Only volatility, a variance feeding a numerical method, is
    /// refused.
    #[tokio::test]
    async fn leaves_an_explicit_growth_override_alone() {
        let db = test_db().await;
        let saved = upsert_assumption(
            &db,
            SaveForecastAssumption {
                annual_growth_bps: Some(500_000),
                ..assumption(Some(1_000))
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.annual_growth_bps, Some(500_000));
    }
}
