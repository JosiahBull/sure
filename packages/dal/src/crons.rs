//! Scheduled adjustments engine + persistence.

use chrono::{Datelike, NaiveDate, Utc};
use sqlx::{FromRow, SqliteConnection};
use sure_core::{AppError, AppResult, ValuationSource};
pub use sure_core::{Cron, CronKind, CronRun, CronRunResult, SaveCron};

use crate::Db;

/// Parse a stored `kind` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `CronKind::as_str`, so an unparseable value means the row came
/// from something else entirely and deserves a real error, not a silent default.
fn parse_kind(kind: String) -> AppResult<CronKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

#[derive(Debug, FromRow)]
struct CronRow {
    id: i64,
    name: String,
    account_id: i64,
    kind: String,
    rate_bps: Option<i64>,
    amount_minor: Option<i64>,
    category_id: Option<i64>,
    frequency: String,
    day_of_month: i64,
    start_date: String,
    last_run_on: Option<String>,
    enabled: bool,
    created_at: String,
    updated_at: String,
}

impl TryFrom<CronRow> for Cron {
    type Error = AppError;

    fn try_from(r: CronRow) -> AppResult<Self> {
        Ok(Cron {
            kind: parse_kind(r.kind)?,
            id: r.id,
            name: r.name,
            account_id: r.account_id,
            rate_bps: r.rate_bps,
            amount_minor: r.amount_minor,
            category_id: r.category_id,
            frequency: r.frequency,
            day_of_month: r.day_of_month,
            start_date: r.start_date,
            last_run_on: r.last_run_on,
            enabled: r.enabled,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

#[derive(Debug, FromRow)]
struct CronRunRow {
    id: i64,
    cron_id: i64,
    period: String,
    kind: String,
    valuation_id: Option<i64>,
    transaction_id: Option<i64>,
    detail: Option<String>,
    created_at: String,
}

impl TryFrom<CronRunRow> for CronRun {
    type Error = AppError;

    fn try_from(r: CronRunRow) -> AppResult<Self> {
        Ok(CronRun {
            kind: parse_kind(r.kind)?,
            id: r.id,
            cron_id: r.cron_id,
            period: r.period,
            valuation_id: r.valuation_id,
            transaction_id: r.transaction_id,
            detail: r.detail,
            created_at: r.created_at,
        })
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Cron>> {
    sqlx::query_as::<_, CronRow>("SELECT * FROM crons ORDER BY id")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(Cron::try_from)
        .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveCron) -> AppResult<Cron> {
    validate(&input)?;
    sqlx::query_as::<_, CronRow>(
        "INSERT INTO crons (name, account_id, kind, rate_bps, amount_minor, category_id,
            day_of_month, start_date, enabled)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(input.account_id)
    .bind(input.kind.as_str())
    .bind(input.rate_bps)
    .bind(input.amount_minor)
    .bind(input.category_id)
    .bind(input.day_of_month.unwrap_or(1).clamp(1, 28))
    .bind(input.start_date.trim())
    .bind(input.enabled)
    .fetch_one(db)
    .await
    .map_err(map_fk)?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveCron) -> AppResult<Cron> {
    validate(&input)?;
    sqlx::query_as::<_, CronRow>(
        "UPDATE crons SET name=?2, account_id=?3, kind=?4, rate_bps=?5, amount_minor=?6,
            category_id=?7, day_of_month=?8, start_date=?9, enabled=?10,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(input.account_id)
    .bind(input.kind.as_str())
    .bind(input.rate_bps)
    .bind(input.amount_minor)
    .bind(input.category_id)
    .bind(input.day_of_month.unwrap_or(1).clamp(1, 28))
    .bind(input.start_date.trim())
    .bind(input.enabled)
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("cron"))?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM crons WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("cron"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_runs(db: &Db, cron_id: i64) -> AppResult<Vec<CronRun>> {
    sqlx::query_as::<_, CronRunRow>("SELECT * FROM cron_runs WHERE cron_id=?1 ORDER BY period DESC")
        .bind(cron_id)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(CronRun::try_from)
        .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn run_one(db: &Db, id: i64, to: Option<&str>) -> AppResult<CronRunResult> {
    let cron = fetch(db, id).await?;
    let to = to
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let mut conn = db.acquire().await?;
    let runs = apply_cron(&mut conn, &cron, to).await?;
    Ok(CronRunResult {
        applied: runs.len() as i64,
        runs,
    })
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn run_all(db: &Db, to: Option<&str>) -> AppResult<CronRunResult> {
    let to = to
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let crons: Vec<Cron> =
        sqlx::query_as::<_, CronRow>("SELECT * FROM crons WHERE enabled=1 ORDER BY id")
            .fetch_all(db)
            .await?
            .into_iter()
            .map(Cron::try_from)
            .collect::<AppResult<_>>()?;
    let mut all = Vec::new();
    let mut conn = db.acquire().await?;
    for cron in crons {
        all.extend(apply_cron(&mut conn, &cron, to).await?);
    }
    Ok(CronRunResult {
        applied: all.len() as i64,
        runs: all,
    })
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn undo_run(db: &Db, run_id: i64) -> AppResult<()> {
    let run: CronRun = sqlx::query_as::<_, CronRunRow>("SELECT * FROM cron_runs WHERE id=?1")
        .bind(run_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("cron run"))?
        .try_into()?;
    let mut txn = db.begin().await?;
    if let Some(vid) = run.valuation_id {
        sqlx::query("DELETE FROM valuations WHERE id=?1")
            .bind(vid)
            .execute(&mut *txn)
            .await?;
    }
    if let Some(tid) = run.transaction_id {
        sqlx::query("DELETE FROM transactions WHERE id=?1")
            .bind(tid)
            .execute(&mut *txn)
            .await?;
    }
    sqlx::query("DELETE FROM cron_runs WHERE id=?1")
        .bind(run_id)
        .execute(&mut *txn)
        .await?;
    let latest = sqlx::query_scalar::<_, Option<String>>(
        "SELECT MAX(period) FROM cron_runs WHERE cron_id=?1",
    )
    .bind(run.cron_id)
    .fetch_one(&mut *txn)
    .await?;
    sqlx::query("UPDATE crons SET last_run_on=?2 WHERE id=?1")
        .bind(run.cron_id)
        .bind(latest)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;
    Ok(())
}

// ---- engine --------------------------------------------------------------

#[tracing::instrument(level = "debug", skip_all)]
async fn apply_cron(
    conn: &mut SqliteConnection,
    cron: &Cron,
    to: NaiveDate,
) -> AppResult<Vec<CronRun>> {
    let start =
        parse_date(&cron.start_date).ok_or_else(|| AppError::validation("bad start_date"))?;
    let last = cron.last_run_on.as_deref().and_then(parse_date);
    let mut created = Vec::new();

    let (mut y, mut m) = (start.year(), start.month());
    let mut guard = 0;
    while guard < 1200 {
        guard += 1;
        let period = period_date(y, m, cron.day_of_month as u32);
        if period > to {
            break;
        }
        let due = period >= start && last.map(|l| period > l).unwrap_or(true);
        if due {
            if let Some(run) = apply_period(conn, cron, period).await? {
                sqlx::query("UPDATE crons SET last_run_on=?2 WHERE id=?1")
                    .bind(cron.id)
                    .bind(period.to_string())
                    .execute(&mut *conn)
                    .await?;
                created.push(run);
            }
        }
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    }
    Ok(created)
}

/// Dispatches a due period to the right engine. Naming every [`CronKind`] variant here
/// — rather than the previous `if kind == "fixed_transaction" { .. } .. if kind ==
/// "depreciation" { shrink } else { grow }` — is the fix for a real bug: that shape
/// silently *appreciated* any kind it didn't specifically recognise. An exhaustive
/// match means a future cron kind must say which way it goes, or the build breaks.
#[tracing::instrument(level = "debug", skip_all)]
async fn apply_period(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period: NaiveDate,
) -> AppResult<Option<CronRun>> {
    let period_s = period.to_string();
    match cron.kind {
        CronKind::FixedTransaction => apply_fixed_transaction(conn, cron, &period_s).await,
        // Interest compounds a loan/mortgage balance upward exactly like appreciation
        // grows an asset's.
        CronKind::Appreciation | CronKind::Interest => {
            apply_valuation_cron(conn, cron, &period_s, 1.0).await
        }
        CronKind::Depreciation => apply_valuation_cron(conn, cron, &period_s, -1.0).await,
    }
}

async fn apply_fixed_transaction(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period_s: &str,
) -> AppResult<Option<CronRun>> {
    let amount = cron.amount_minor.unwrap_or(0);
    let ccy = sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
        .bind(cron.account_id)
        .fetch_one(&mut *conn)
        .await?;
    let tx_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code, description, category_id)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING id",
    )
    .bind(cron.account_id)
    .bind(period_s)
    .bind(amount)
    .bind(&ccy)
    .bind(cron.name.trim())
    .bind(cron.category_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(
        record_run(conn, cron, period_s, None, Some(tx_id), None).await?,
    ))
}

/// Grow (`sign = 1.0`) or shrink (`sign = -1.0`) the account's latest valuation by
/// `cron.rate_bps` annually, compounded monthly.
async fn apply_valuation_cron(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period_s: &str,
    sign: f64,
) -> AppResult<Option<CronRun>> {
    let latest = sqlx::query_scalar::<_, i64>(
        "SELECT value_minor FROM valuations WHERE account_id=?1 AND as_of <= ?2
         ORDER BY as_of DESC, id DESC LIMIT 1",
    )
    .bind(cron.account_id)
    .bind(period_s)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(latest) = latest else {
        return Ok(Some(
            record_run(
                conn,
                cron,
                period_s,
                None,
                None,
                Some("no base valuation".into()),
            )
            .await?,
        ));
    };

    let r = sign * (cron.rate_bps.unwrap_or(0) as f64 / 10_000.0);
    let monthly = (1.0 + r).powf(1.0 / 12.0);
    let new_value = (latest as f64 * monthly).round() as i64;
    let ccy = sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
        .bind(cron.account_id)
        .fetch_one(&mut *conn)
        .await?;
    let val_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING id",
    )
    .bind(cron.account_id)
    .bind(period_s)
    .bind(new_value)
    .bind(&ccy)
    .bind(ValuationSource::Cron.as_str())
    .bind(cron.name.trim())
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(
        record_run(conn, cron, period_s, Some(val_id), None, None).await?,
    ))
}

#[tracing::instrument(level = "debug", skip_all)]
async fn record_run(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period: &str,
    valuation_id: Option<i64>,
    transaction_id: Option<i64>,
    detail: Option<String>,
) -> AppResult<CronRun> {
    sqlx::query_as::<_, CronRunRow>(
        "INSERT INTO cron_runs (cron_id, period, kind, valuation_id, transaction_id, detail)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING *",
    )
    .bind(cron.id)
    .bind(period)
    .bind(cron.kind.as_str())
    .bind(valuation_id)
    .bind(transaction_id)
    .bind(detail)
    .fetch_one(&mut *conn)
    .await?
    .try_into()
}

fn validate(input: &SaveCron) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("cron name is required"));
    }
    if input.kind == CronKind::FixedTransaction && input.amount_minor.is_none() {
        return Err(AppError::validation(
            "fixed_transaction requires amount_minor",
        ));
    }
    if input.kind != CronKind::FixedTransaction && input.rate_bps.is_none() {
        return Err(AppError::validation("valuation crons require rate_bps"));
    }
    if parse_date(&input.start_date).is_none() {
        return Err(AppError::validation("start_date must be YYYY-MM-DD"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn fetch(db: &Db, id: i64) -> AppResult<Cron> {
    sqlx::query_as::<_, CronRow>("SELECT * FROM crons WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("cron"))?
        .try_into()
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}

fn period_date(year: i32, month: u32, day_of_month: u32) -> NaiveDate {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let last_day = NaiveDate::from_ymd_opt(ny, nm, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
        .day();
    NaiveDate::from_ymd_opt(year, month, day_of_month.min(last_day)).unwrap()
}

// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
// (CLAUDE.md rule 2's escape hatch) — the arm above is exhaustive over our own types.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account or category does not exist")
        }
        other => AppError::from(other),
    }
}
