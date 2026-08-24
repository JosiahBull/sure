//! `investment_strategies` CRUD. See `0046_investment_strategies.sql` for what a strategy is and
//! why the return lives on the target account rather than here, and `sure_app::forecast`'s
//! `StrategySim` for how a month's sweep is applied.

use sure_core::{AppError, AppResult};
pub use sure_core::{InvestmentStrategy, SaveInvestmentStrategy};

use crate::Db;

#[derive(Debug)]
struct StrategyRow {
    id: i64,
    label: String,
    active_from: String,
    active_to: Option<String>,
    income_share_bps: i64,
    income_stream_id: Option<i64>,
    windfall_share_bps: i64,
    debt_above_bps: Option<i64>,
    target_account_id: i64,
    enabled: bool,
    sort_order: i64,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<StrategyRow> for InvestmentStrategy {
    /// Infallible, unlike the commitment and stream rows: every column here is an integer, a date
    /// or free text. There is no stored enum to parse, so there is nothing to fail on.
    fn from(r: StrategyRow) -> Self {
        InvestmentStrategy {
            id: r.id,
            label: r.label,
            active_from: r.active_from,
            active_to: r.active_to,
            income_share_bps: r.income_share_bps,
            income_stream_id: r.income_stream_id,
            windfall_share_bps: r.windfall_share_bps,
            debt_above_bps: r.debt_above_bps,
            target_account_id: r.target_account_id,
            enabled: r.enabled,
            sort_order: r.sort_order,
            notes: r.notes,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<InvestmentStrategy>> {
    Ok(sqlx::query_as!(
        StrategyRow,
        r#"SELECT id AS "id!", label, active_from, active_to, income_share_bps, income_stream_id,
                  windfall_share_bps, debt_above_bps, target_account_id,
                  enabled AS "enabled!: bool", sort_order, notes, created_at, updated_at
             FROM investment_strategies ORDER BY active_from, sort_order, id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<InvestmentStrategy> {
    Ok(sqlx::query_as!(
        StrategyRow,
        r#"SELECT id AS "id!", label, active_from, active_to, income_share_bps, income_stream_id,
                  windfall_share_bps, debt_above_bps, target_account_id,
                  enabled AS "enabled!: bool", sort_order, notes, created_at, updated_at
             FROM investment_strategies WHERE id = ?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("investment strategy"))?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveInvestmentStrategy) -> AppResult<InvestmentStrategy> {
    input
        .validate()
        .map_err(|p| AppError::validation(p.join("; ")))?;
    let label = input.label.trim();
    let active_from = input.active_from.to_string();
    let active_to = input.active_to.as_ref().map(ToString::to_string);
    let notes = input
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let id = sqlx::query_scalar!(
        r#"INSERT INTO investment_strategies
              (label, active_from, active_to, income_share_bps, income_stream_id,
               windfall_share_bps, debt_above_bps, target_account_id, enabled, sort_order, notes)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
           RETURNING id AS "id!""#,
        label,
        active_from,
        active_to,
        input.income_share_bps,
        input.income_stream_id,
        input.windfall_share_bps,
        input.debt_above_bps,
        input.target_account_id,
        input.enabled,
        input.sort_order,
        notes
    )
    .fetch_one(db)
    .await
    .map_err(map_fk)?;
    get(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(
    db: &Db,
    id: i64,
    input: SaveInvestmentStrategy,
) -> AppResult<InvestmentStrategy> {
    input
        .validate()
        .map_err(|p| AppError::validation(p.join("; ")))?;
    let label = input.label.trim();
    let active_from = input.active_from.to_string();
    let active_to = input.active_to.as_ref().map(ToString::to_string);
    let notes = input
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    let done = sqlx::query!(
        "UPDATE investment_strategies SET
            label=?2, active_from=?3, active_to=?4, income_share_bps=?5, income_stream_id=?6,
            windfall_share_bps=?7, debt_above_bps=?8, target_account_id=?9, enabled=?10,
            sort_order=?11, notes=?12, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1",
        id,
        label,
        active_from,
        active_to,
        input.income_share_bps,
        input.income_stream_id,
        input.windfall_share_bps,
        input.debt_above_bps,
        input.target_account_id,
        input.enabled,
        input.sort_order,
        notes
    )
    .execute(db)
    .await
    .map_err(map_fk)?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("investment strategy"));
    }
    get(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let done = sqlx::query!("DELETE FROM investment_strategies WHERE id=?1", id)
        .execute(db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("investment strategy"));
    }
    Ok(())
}

/// A foreign-key violation named rather than surfaced as a 500 — the target account or the named
/// stream, either of which a caller can fix.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_fk(e: sqlx::Error) -> AppError {
    // `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here.
    match &e {
        sqlx::Error::Database(db) if db.message().contains("FOREIGN KEY") => {
            AppError::validation("unknown target account or income stream for this strategy")
        }
        _ => AppError::from(e),
    }
}
