//! Scheduled adjustments engine + persistence.

use chrono::{Datelike, NaiveDate, Utc};
use sqlx::SqliteConnection;
use sure_core::{AppError, AppResult};
pub use sure_core::{Cron, CronRun, CronRunResult, SaveCron};

use crate::Db;

const KINDS: [&str; 4] = ["appreciation", "depreciation", "interest", "fixed_transaction"];

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Cron>> {
    Ok(sqlx::query_as::<_, Cron>("SELECT * FROM crons ORDER BY id")
        .fetch_all(db)
        .await?)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveCron) -> AppResult<Cron> {
    validate(&input)?;
    sqlx::query_as::<_, Cron>(
        "INSERT INTO crons (name, account_id, kind, rate_bps, amount_minor, category_id,
            day_of_month, start_date, enabled)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(input.account_id)
    .bind(&input.kind)
    .bind(input.rate_bps)
    .bind(input.amount_minor)
    .bind(input.category_id)
    .bind(input.day_of_month.unwrap_or(1).clamp(1, 28))
    .bind(input.start_date.trim())
    .bind(input.enabled)
    .fetch_one(db)
    .await
    .map_err(map_fk)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveCron) -> AppResult<Cron> {
    validate(&input)?;
    sqlx::query_as::<_, Cron>(
        "UPDATE crons SET name=?2, account_id=?3, kind=?4, rate_bps=?5, amount_minor=?6,
            category_id=?7, day_of_month=?8, start_date=?9, enabled=?10,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(input.account_id)
    .bind(&input.kind)
    .bind(input.rate_bps)
    .bind(input.amount_minor)
    .bind(input.category_id)
    .bind(input.day_of_month.unwrap_or(1).clamp(1, 28))
    .bind(input.start_date.trim())
    .bind(input.enabled)
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("cron"))
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
    Ok(
        sqlx::query_as::<_, CronRun>("SELECT * FROM cron_runs WHERE cron_id=?1 ORDER BY period DESC")
            .bind(cron_id)
            .fetch_all(db)
            .await?,
    )
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn run_one(db: &Db, id: i64, to: Option<&str>) -> AppResult<CronRunResult> {
    let cron = fetch(db, id).await?;
    let to = to.and_then(parse_date).unwrap_or_else(|| Utc::now().date_naive());
    let mut conn = db.acquire().await?;
    let runs = apply_cron(&mut conn, &cron, to).await?;
    Ok(CronRunResult {
        applied: runs.len() as i64,
        runs,
    })
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn run_all(db: &Db, to: Option<&str>) -> AppResult<CronRunResult> {
    let to = to.and_then(parse_date).unwrap_or_else(|| Utc::now().date_naive());
    let crons = sqlx::query_as::<_, Cron>("SELECT * FROM crons WHERE enabled=1 ORDER BY id")
        .fetch_all(db)
        .await?;
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
    let run = sqlx::query_as::<_, CronRun>("SELECT * FROM cron_runs WHERE id=?1")
        .bind(run_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("cron run"))?;
    let mut txn = db.begin().await?;
    if let Some(vid) = run.valuation_id {
        sqlx::query("DELETE FROM valuations WHERE id=?1").bind(vid).execute(&mut *txn).await?;
    }
    if let Some(tid) = run.transaction_id {
        sqlx::query("DELETE FROM transactions WHERE id=?1").bind(tid).execute(&mut *txn).await?;
    }
    sqlx::query("DELETE FROM cron_runs WHERE id=?1").bind(run_id).execute(&mut *txn).await?;
    let latest =
        sqlx::query_scalar::<_, Option<String>>("SELECT MAX(period) FROM cron_runs WHERE cron_id=?1")
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
async fn apply_cron(conn: &mut SqliteConnection, cron: &Cron, to: NaiveDate) -> AppResult<Vec<CronRun>> {
    let start = parse_date(&cron.start_date).ok_or_else(|| AppError::validation("bad start_date"))?;
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

#[tracing::instrument(level = "debug", skip_all)]
async fn apply_period(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period: NaiveDate,
) -> AppResult<Option<CronRun>> {
    let period_s = period.to_string();
    if cron.kind == "fixed_transaction" {
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
        .bind(&period_s)
        .bind(amount)
        .bind(&ccy)
        .bind(cron.name.trim())
        .bind(cron.category_id)
        .fetch_one(&mut *conn)
        .await?;
        return Ok(Some(record_run(conn, cron, &period_s, None, Some(tx_id), None).await?));
    }

    let latest = sqlx::query_scalar::<_, i64>(
        "SELECT value_minor FROM valuations WHERE account_id=?1 AND as_of <= ?2
         ORDER BY as_of DESC, id DESC LIMIT 1",
    )
    .bind(cron.account_id)
    .bind(&period_s)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(latest) = latest else {
        return Ok(Some(
            record_run(conn, cron, &period_s, None, None, Some("no base valuation".into())).await?,
        ));
    };

    let r = cron.rate_bps.unwrap_or(0) as f64 / 10_000.0;
    let monthly = if cron.kind == "depreciation" {
        (1.0 - r).powf(1.0 / 12.0)
    } else {
        (1.0 + r).powf(1.0 / 12.0)
    };
    let new_value = (latest as f64 * monthly).round() as i64;
    let ccy = sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
        .bind(cron.account_id)
        .fetch_one(&mut *conn)
        .await?;
    let val_id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1,?2,?3,?4,'cron',?5) RETURNING id",
    )
    .bind(cron.account_id)
    .bind(&period_s)
    .bind(new_value)
    .bind(&ccy)
    .bind(cron.name.trim())
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(record_run(conn, cron, &period_s, Some(val_id), None, None).await?))
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
    Ok(sqlx::query_as::<_, CronRun>(
        "INSERT INTO cron_runs (cron_id, period, kind, valuation_id, transaction_id, detail)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING *",
    )
    .bind(cron.id)
    .bind(period)
    .bind(&cron.kind)
    .bind(valuation_id)
    .bind(transaction_id)
    .bind(detail)
    .fetch_one(&mut *conn)
    .await?)
}

fn validate(input: &SaveCron) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("cron name is required"));
    }
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(AppError::validation(format!("kind must be one of {KINDS:?}")));
    }
    if input.kind == "fixed_transaction" && input.amount_minor.is_none() {
        return Err(AppError::validation("fixed_transaction requires amount_minor"));
    }
    if input.kind != "fixed_transaction" && input.rate_bps.is_none() {
        return Err(AppError::validation("valuation crons require rate_bps"));
    }
    if parse_date(&input.start_date).is_none() {
        return Err(AppError::validation("start_date must be YYYY-MM-DD"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn fetch(db: &Db, id: i64) -> AppResult<Cron> {
    sqlx::query_as::<_, Cron>("SELECT * FROM crons WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("cron"))
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}

fn period_date(year: i32, month: u32, day_of_month: u32) -> NaiveDate {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let last_day = NaiveDate::from_ymd_opt(ny, nm, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
        .day();
    NaiveDate::from_ymd_opt(year, month, day_of_month.min(last_day)).unwrap()
}

fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account or category does not exist")
        }
        other => AppError::from(other),
    }
}
