//! Scheduled adjustments engine + persistence.

use chrono::{Datelike, NaiveDate, Utc};
use sqlx::SqliteConnection;
use sure_core::{AppError, AppResult, Money, ValuationSource};
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

#[derive(Debug)]
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

#[derive(Debug)]
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
    sqlx::query_as!(
        CronRow,
        r#"SELECT id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                  frequency, day_of_month, start_date, last_run_on, enabled AS "enabled!: bool",
                  created_at, updated_at
             FROM crons ORDER BY id"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Cron::try_from)
    .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveCron) -> AppResult<Cron> {
    validate(&input)?;
    let name = input.name.trim();
    let kind = input.kind.as_str();
    let amount_minor = input.amount_minor.map(Money::minor);
    let day_of_month = input.day_of_month.unwrap_or(1).clamp(1, 28);
    let start_date = input.start_date.to_string();
    sqlx::query_as!(
        CronRow,
        r#"INSERT INTO crons (name, account_id, kind, rate_bps, amount_minor, category_id,
              day_of_month, start_date, enabled)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
           RETURNING id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                     frequency, day_of_month, start_date, last_run_on, enabled AS "enabled!: bool",
                     created_at, updated_at"#,
        name,
        input.account_id,
        kind,
        input.rate_bps,
        amount_minor,
        input.category_id,
        day_of_month,
        start_date,
        input.enabled
    )
    .fetch_one(db)
    .await
    .map_err(map_fk)?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveCron) -> AppResult<Cron> {
    validate(&input)?;
    let name = input.name.trim();
    let kind = input.kind.as_str();
    let amount_minor = input.amount_minor.map(Money::minor);
    let day_of_month = input.day_of_month.unwrap_or(1).clamp(1, 28);
    let start_date = input.start_date.to_string();
    sqlx::query_as!(
        CronRow,
        r#"UPDATE crons SET name=?2, account_id=?3, kind=?4, rate_bps=?5, amount_minor=?6,
              category_id=?7, day_of_month=?8, start_date=?9, enabled=?10,
              updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id=?1
           RETURNING id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                     frequency, day_of_month, start_date, last_run_on, enabled AS "enabled!: bool",
                     created_at, updated_at"#,
        id,
        name,
        input.account_id,
        kind,
        input.rate_bps,
        amount_minor,
        input.category_id,
        day_of_month,
        start_date,
        input.enabled
    )
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("cron"))?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM crons WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("cron"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_runs(db: &Db, cron_id: i64) -> AppResult<Vec<CronRun>> {
    sqlx::query_as!(
        CronRunRow,
        r#"SELECT id AS "id!", cron_id, period, kind, valuation_id, transaction_id, detail,
                  created_at
             FROM cron_runs WHERE cron_id=?1 ORDER BY period DESC"#,
        cron_id
    )
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
    let crons: Vec<Cron> = sqlx::query_as!(
        CronRow,
        r#"SELECT id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                  frequency, day_of_month, start_date, last_run_on, enabled AS "enabled!: bool",
                  created_at, updated_at
                 FROM crons WHERE enabled=1 ORDER BY id"#
    )
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
    let run: CronRun = sqlx::query_as!(
        CronRunRow,
        r#"SELECT id AS "id!", cron_id, period, kind, valuation_id, transaction_id, detail,
                  created_at
             FROM cron_runs WHERE id=?1"#,
        run_id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("cron run"))?
    .try_into()?;
    let mut txn = db.begin().await?;
    if let Some(vid) = run.valuation_id {
        sqlx::query!("DELETE FROM valuations WHERE id=?1", vid)
            .execute(&mut *txn)
            .await?;
    }
    if let Some(tid) = run.transaction_id {
        sqlx::query!("DELETE FROM transactions WHERE id=?1", tid)
            .execute(&mut *txn)
            .await?;
    }
    sqlx::query!("DELETE FROM cron_runs WHERE id=?1", run_id)
        .execute(&mut *txn)
        .await?;
    // MAX over no remaining runs is NULL, which is exactly the value `last_run_on` wants when
    // the undone run was the only one.
    let latest = sqlx::query_scalar!(
        r#"SELECT MAX(period) AS "period: String" FROM cron_runs WHERE cron_id=?1"#,
        run.cron_id
    )
    .fetch_one(&mut *txn)
    .await?;
    sqlx::query!(
        "UPDATE crons SET last_run_on=?2 WHERE id=?1",
        run.cron_id,
        latest
    )
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
        if due && let Some(run) = apply_period(conn, cron, period).await? {
            let period_s = period.to_string();
            sqlx::query!(
                "UPDATE crons SET last_run_on=?2 WHERE id=?1",
                cron.id,
                period_s
            )
            .execute(&mut *conn)
            .await?;
            created.push(run);
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
            apply_valuation_cron(conn, cron, &period_s, ValuationDirection::Grow).await
        }
        CronKind::Depreciation => {
            apply_valuation_cron(conn, cron, &period_s, ValuationDirection::Shrink).await
        }
    }
}

/// Which way a valuation cron moves the balance each period. This was a bare `sign: f64`
/// parameter, whose two legal values (`1.0` / `-1.0`) were indistinguishable from any
/// other float a future caller could pass — and an out-of-band sign lands straight in the
/// `NaN` arithmetic [`monthly_factor`] now guards. As an enum, "which way does this kind
/// go" stays exhaustive for the same reason [`apply_period`]'s own `match cron.kind` does
/// (CLAUDE.md rule 2): a new kind has to say, or the build breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValuationDirection {
    /// Appreciation / interest: compound the balance upward.
    Grow,
    /// Depreciation: compound it downward.
    Shrink,
}

impl ValuationDirection {
    fn signum(self) -> f64 {
        match self {
            ValuationDirection::Grow => 1.0,
            ValuationDirection::Shrink => -1.0,
        }
    }
}

async fn apply_fixed_transaction(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period_s: &str,
) -> AppResult<Option<CronRun>> {
    let amount = cron.amount_minor.unwrap_or(0);
    let ccy = sqlx::query_scalar!(
        "SELECT currency_code FROM accounts WHERE id=?1",
        cron.account_id
    )
    .fetch_one(&mut *conn)
    .await?;
    let description = cron.name.trim();
    let tx_id = sqlx::query_scalar!(
        r#"INSERT INTO transactions
              (account_id, posted_at, amount_minor, currency_code, description, category_id)
           VALUES (?1,?2,?3,?4,?5,?6)
           RETURNING id AS "id!""#,
        cron.account_id,
        period_s,
        amount,
        ccy,
        description,
        cron.category_id
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(
        record_run(conn, cron, period_s, None, Some(tx_id), None).await?,
    ))
}

/// Grow or shrink the account's latest valuation by `cron.rate_bps` annually, compounded
/// monthly, per `direction`.
async fn apply_valuation_cron(
    conn: &mut SqliteConnection,
    cron: &Cron,
    period_s: &str,
    direction: ValuationDirection,
) -> AppResult<Option<CronRun>> {
    let latest = sqlx::query_scalar!(
        "SELECT value_minor FROM valuations WHERE account_id=?1 AND as_of <= ?2
         ORDER BY as_of DESC, id DESC LIMIT 1",
        cron.account_id,
        period_s
    )
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

    let new_value = compounded_value(cron, latest, direction)?;
    let ccy = sqlx::query_scalar!(
        "SELECT currency_code FROM accounts WHERE id=?1",
        cron.account_id
    )
    .fetch_one(&mut *conn)
    .await?;
    let source = ValuationSource::Cron.as_str();
    let note = cron.name.trim();
    let val_id = sqlx::query_scalar!(
        r#"INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
           VALUES (?1,?2,?3,?4,?5,?6)
           RETURNING id AS "id!""#,
        cron.account_id,
        period_s,
        new_value,
        ccy,
        source,
        note
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(
        record_run(conn, cron, period_s, Some(val_id), None, None).await?,
    ))
}

/// One period's compounding factor: the twelfth root of `1 ± rate`.
///
/// The `is_finite` check is the second half of W-19's fix and the half that survives a
/// future kind whose maths differs from today's. `validate` keeps an out-of-range
/// `rate_bps` out of the table in the first place, but rows predating that guard (and any
/// arithmetic a later kind invents) still reach here, and the failure mode is silent
/// rather than loud: for a depreciation rate past 100%/yr the base `1 + r` is negative, a
/// fractional power of a negative base is `NaN`, `NaN.round()` is `NaN`, and `NaN as i64`
/// *saturates to 0* in Rust — no panic, no error, just a persisted `source='cron'`
/// valuation of zero for every period from the start date, with `last_run_on` advanced as
/// if it had worked. Refusing to write is recoverable; a zeroed account that every later
/// date reads back verbatim is not.
fn monthly_factor(cron: &Cron, direction: ValuationDirection) -> AppResult<f64> {
    let r = direction.signum() * (cron.rate_bps.unwrap_or(0) as f64 / 10_000.0);
    let monthly = (1.0 + r).powf(1.0 / 12.0);
    if !monthly.is_finite() {
        return Err(AppError::validation(format!(
            "cron '{}' (#{}) has an unusable rate_bps {}: it compounds to a non-finite \
             monthly factor, so no valuation can be written",
            cron.name.trim(),
            cron.id,
            cron.rate_bps.unwrap_or(0),
        )));
    }
    Ok(monthly)
}

/// `latest` compounded by one period, rounded to whole minor units.
///
/// The result is range-checked as well as the factor: `f64 as i64` is a *saturating* cast,
/// so an out-of-range product would silently become `i64::MAX`/`MIN` (or `0` for `NaN`)
/// and be persisted as a real valuation. Same principle as [`monthly_factor`] — fail the
/// run, don't invent a number.
fn compounded_value(cron: &Cron, latest: i64, direction: ValuationDirection) -> AppResult<i64> {
    let scaled = (latest as f64 * monthly_factor(cron, direction)?).round();
    if !scaled.is_finite() || scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
        return Err(AppError::validation(format!(
            "cron '{}' (#{}) compounds {latest} to a value outside the representable range",
            cron.name.trim(),
            cron.id,
        )));
    }
    Ok(scaled as i64)
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
    let kind = cron.kind.as_str();
    sqlx::query_as!(
        CronRunRow,
        r#"INSERT INTO cron_runs (cron_id, period, kind, valuation_id, transaction_id, detail)
           VALUES (?1,?2,?3,?4,?5,?6)
           RETURNING id AS "id!", cron_id, period, kind, valuation_id, transaction_id, detail,
                     created_at"#,
        cron.id,
        period,
        kind,
        valuation_id,
        transaction_id,
        detail
    )
    .fetch_one(&mut *conn)
    .await?
    .try_into()
}

/// Deliberately the same number, for the same reason, as `sure_app::forecast`'s
/// `MAX_RATE_BPS`: 1000%/yr. Past this a "rate" is a data-entry slip, not a rate, and the
/// compounding it feeds is exactly where a projection turns into `inf`/`NaN`. A cron rate
/// is also never *negative* — which way it moves the balance is [`ValuationDirection`]'s
/// job, not the sign of `rate_bps`, and a negative rate just flips the direction back
/// while smuggling the same negative base into `powf` (see [`monthly_factor`]).
const MAX_RATE_BPS: i64 = 100_000;

/// Exclusive ceiling on a depreciation rate: a decline of 100%/yr or more is not
/// expressible by `(1 - r).powf(1/12)`. At exactly 10_000 bps the base is `0.0` and the
/// account legitimately zeroes out; beyond it the base is negative and the whole
/// computation is `NaN` — which, cast to `i64`, is a *persisted valuation of 0* rather
/// than an error (see [`monthly_factor`]). Both readings are worthless in a balance sheet,
/// so the boundary is rejected too.
const MAX_DEPRECIATION_BPS: i64 = 10_000;

fn validate(input: &SaveCron) -> AppResult<()> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("cron name is required"));
    }
    // Exhaustive over `CronKind` (rule 2): each kind states which of the two mutually
    // exclusive fields it needs, so a new kind cannot inherit the wrong requirement.
    match input.kind {
        CronKind::FixedTransaction => {
            if input.amount_minor.is_none() {
                return Err(AppError::validation(
                    "fixed_transaction requires amount_minor",
                ));
            }
        }
        CronKind::Appreciation | CronKind::Interest | CronKind::Depreciation => {
            let Some(rate_bps) = input.rate_bps else {
                return Err(AppError::validation("valuation crons require rate_bps"));
            };
            validate_rate_bps(input.kind, rate_bps)?;
        }
    }
    Ok(())
}

/// Keep an unusable rate out of the table entirely, so `apply_cron` never has to decide
/// what a run of up to 1200 nonsense periods should do. Without this, `rate_bps: 20000` on
/// a depreciation cron was accepted, and every period from the start date then wrote a
/// `source='cron'` valuation of `0` (see [`monthly_factor`] for why) — each one
/// individually undoable, none of them corrected by any later transaction.
fn validate_rate_bps(kind: CronKind, rate_bps: i64) -> AppResult<()> {
    if rate_bps < 0 {
        return Err(AppError::validation(
            "rate_bps must not be negative — use the cron kind to choose the direction",
        ));
    }
    if rate_bps > MAX_RATE_BPS {
        return Err(AppError::validation(format!(
            "rate_bps must be at most {MAX_RATE_BPS} ({}%/yr)",
            MAX_RATE_BPS / 100
        )));
    }
    // Exhaustive over `CronKind` (rule 2): only depreciation subtracts the rate from 1, so
    // only depreciation has the tighter ceiling. Growth kinds are already bounded above.
    match kind {
        CronKind::Depreciation => {
            if rate_bps >= MAX_DEPRECIATION_BPS {
                return Err(AppError::validation(format!(
                    "depreciation rate_bps must be under {MAX_DEPRECIATION_BPS} \
                     (100%/yr) — a faster decline than that is not expressible"
                )));
            }
        }
        CronKind::Appreciation | CronKind::Interest | CronKind::FixedTransaction => {}
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn fetch(db: &Db, id: i64) -> AppResult<Cron> {
    sqlx::query_as!(
        CronRow,
        r#"SELECT id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                  frequency, day_of_month, start_date, last_run_on, enabled AS "enabled!: bool",
                  created_at, updated_at
             FROM crons WHERE id=?1"#,
        id
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::IsoDate;

    /// A saved cron as the API would submit it: valid in every respect except whatever the
    /// individual test overrides. `rate_bps` is left `None` so each test states its own.
    fn save(kind: CronKind, rate_bps: Option<i64>) -> SaveCron {
        SaveCron {
            name: "Hilux depreciation".to_string(),
            account_id: 1,
            kind,
            rate_bps,
            amount_minor: None,
            category_id: None,
            day_of_month: Some(1),
            start_date: IsoDate::parse("2026-01-01").unwrap(),
            enabled: true,
        }
    }

    /// A stored cron, i.e. what the run engine actually reads. Only `id`, `name` and
    /// `rate_bps` matter to the arithmetic under test.
    fn stored(kind: CronKind, rate_bps: Option<i64>) -> Cron {
        Cron {
            id: 7,
            name: "Hilux depreciation".to_string(),
            account_id: 1,
            kind,
            rate_bps,
            amount_minor: None,
            category_id: None,
            frequency: "monthly".to_string(),
            day_of_month: 1,
            start_date: "2026-01-01".to_string(),
            last_run_on: None,
            enabled: true,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        }
    }

    #[test]
    fn a_depreciation_faster_than_100_percent_a_year_is_rejected() {
        // 200%/yr: the rate that used to be accepted and then wrote a $0 valuation for
        // every period from the start date.
        let err = validate(&save(CronKind::Depreciation, Some(20_000))).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
        // The boundary itself is out too: it zeroes the account exactly rather than via
        // `NaN`, which is no more useful in a balance sheet.
        assert!(validate(&save(CronKind::Depreciation, Some(10_000))).is_err());
        // Just under it is still a legal (if brutal) cron.
        assert!(validate(&save(CronKind::Depreciation, Some(9_999))).is_ok());
    }

    #[test]
    fn a_rate_outside_the_sane_band_is_rejected_for_every_valuation_kind() {
        for kind in [
            CronKind::Appreciation,
            CronKind::Interest,
            CronKind::Depreciation,
        ] {
            // Negative rates flip the direction behind the kind's back and smuggle a
            // negative base into `powf`.
            assert!(
                validate(&save(kind, Some(-1))).is_err(),
                "{kind:?} accepted a negative rate"
            );
            assert!(
                validate(&save(kind, Some(MAX_RATE_BPS + 1))).is_err(),
                "{kind:?} accepted a rate past MAX_RATE_BPS"
            );
        }
        assert!(validate(&save(CronKind::Appreciation, Some(MAX_RATE_BPS))).is_ok());
        // Unchanged: a valuation cron with no rate at all is still a 422, and a
        // fixed-transaction cron needs an amount rather than a rate.
        assert!(validate(&save(CronKind::Appreciation, None)).is_err());
        assert!(validate(&save(CronKind::FixedTransaction, None)).is_err());
        let mut fixed = save(CronKind::FixedTransaction, None);
        fixed.amount_minor = Some(Money::new(-1999).unwrap());
        assert!(validate(&fixed).is_ok());
    }

    #[test]
    fn a_non_finite_factor_is_an_error_not_a_silently_persisted_zero() {
        // Bypasses `validate` on purpose: this is a row that predates the guard above, or
        // a future kind whose arithmetic goes somewhere `validate` doesn't police.
        let cron = stored(CronKind::Depreciation, Some(20_000));
        let err = monthly_factor(&cron, ValuationDirection::Shrink).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
        // The message names the offending cron so a failed run is diagnosable.
        assert!(format!("{err:?}").contains("Hilux depreciation"), "{err:?}");
        let err = compounded_value(&cron, 3_000_000, ValuationDirection::Shrink).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "got {err:?}");

        // What the old code did instead, and why this matters: the cast is saturating, so
        // the caller persisted a valuation of exactly zero with no panic and no error.
        let unguarded = (1.0 - 2.0_f64).powf(1.0 / 12.0);
        assert!(unguarded.is_nan());
        assert_eq!((3_000_000.0 * unguarded).round() as i64, 0);
    }

    #[test]
    fn an_ordinary_depreciation_still_compounds_monthly() {
        // 15%/yr on a $30,000 vehicle: 0.85^(1/12) per month.
        let cron = stored(CronKind::Depreciation, Some(1_500));
        let factor = monthly_factor(&cron, ValuationDirection::Shrink).unwrap();
        assert!(
            (factor - 0.986_548_052_988_131_1).abs() < 1e-12,
            "factor was {factor}"
        );
        assert_eq!(
            compounded_value(&cron, 3_000_000, ValuationDirection::Shrink).unwrap(),
            2_959_644
        );
    }

    #[test]
    fn appreciation_still_grows_the_balance() {
        // 1%/yr on a $1,000,000 house — the case `crons.spec.ts` exercises end to end.
        let cron = stored(CronKind::Appreciation, Some(100));
        let factor = monthly_factor(&cron, ValuationDirection::Grow).unwrap();
        assert!(
            (factor - 1.000_829_538_114_346_2).abs() < 1e-12,
            "factor was {factor}"
        );
        assert_eq!(
            compounded_value(&cron, 100_000_000, ValuationDirection::Grow).unwrap(),
            100_082_954
        );
        // A zero-rate cron is a no-op rather than an error.
        let flat = stored(CronKind::Interest, Some(0));
        assert_eq!(
            compounded_value(&flat, -47_921_483, ValuationDirection::Grow).unwrap(),
            -47_921_483
        );
    }
}
