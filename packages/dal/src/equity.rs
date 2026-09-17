//! Equity grants, exercises, and computed vesting status.

use chrono::{Datelike, NaiveDate, Utc};
pub use sure_core::{
    AccountEquity, EquityEvent, EquityEventKind, EquityExercise, EquityGrant, EquityMark,
    RebuildResult, SaveExercise, SaveGrant, SaveMark, VestingStatus,
};
use sure_core::{AppError, AppResult, ValuationSource};

use crate::Db;

/// The largest share count a grant may carry. No real grant covers a trillion units —
/// Apple's entire issued float is around 1.5×10^10 — so a figure above this is data
/// entry (a minor-unit amount pasted into the quantity field, most often), and the user
/// is better served by a 422 naming the field than by a grant whose intrinsic value can
/// only ever be rejected later, at read time, on every request that touches the account.
const MAX_GRANT_QUANTITY: i64 = 1_000_000_000_000;

/// The largest per-unit money figure (strike or fair value) a grant may carry: the shared
/// wire-edge money ceiling, ±$1 trillion in minor units. Imported rather than redefined —
/// this file used to carry its own `1_000_000_000_000_000`, and a second number meant a
/// transaction and a strike price could disagree about what "absurd" means. See
/// [`sure_core::MAX_MONEY_MINOR`] for how the value is justified (it is picked for the
/// headroom it leaves the `i64` sums layered on top — `account_equity` adds every grant's
/// intrinsic value into one total, and that total is what `revalue` persists).
///
/// Note this ceiling and [`MAX_GRANT_QUANTITY`] deliberately do *not* multiply safely into an
/// i64 — a legitimate high-priced grant (Berkshire A at ~$700k a share) needs a money ceiling
/// well above `i64::MAX / MAX_GRANT_QUANTITY`. They are data-entry sanity checks, not the
/// overflow guard: `validate_grant` separately rejects a *pair* whose product cannot fit,
/// and `compute_status` still computes in i128 because neither check has ever run against
/// the rows already on disk.
const MAX_MONEY_MINOR: i64 = sure_core::MAX_MONEY_MINOR;

#[derive(Debug)]
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

#[derive(Debug)]
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

// ---------------------------------------------------------------------------
// The price ledger. See `0039_equity_marks.sql` for why price is a series and not a scalar.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct EquityMarkRow {
    id: i64,
    account_id: i64,
    as_of: String,
    unit_value_minor: i64,
    currency_code: String,
    note: Option<String>,
    created_at: String,
}

impl From<EquityMarkRow> for EquityMark {
    fn from(r: EquityMarkRow) -> Self {
        EquityMark {
            id: r.id,
            account_id: r.account_id,
            as_of: r.as_of,
            unit_value_minor: r.unit_value_minor,
            currency_code: r.currency_code,
            note: r.note,
            created_at: r.created_at,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_marks(db: &Db, account_id: i64) -> AppResult<Vec<EquityMark>> {
    Ok(sqlx::query_as!(
        EquityMarkRow,
        r#"SELECT id AS "id!", account_id, as_of, unit_value_minor, currency_code, note, created_at
             FROM equity_marks WHERE account_id=?1 ORDER BY as_of DESC, id DESC"#,
        account_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

/// Record what a unit is worth from a date on, replacing any mark already on that date.
///
/// Upserts rather than erroring on a duplicate date: correcting a figure is the common reason to
/// set one twice, and there is only ever one price on a given day (`0039`'s unique index).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_mark(db: &Db, account_id: i64, input: SaveMark) -> AppResult<EquityMark> {
    let account_ccy =
        sqlx::query_scalar!("SELECT currency_code FROM accounts WHERE id=?1", account_id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("account"))?;
    validate_mark(input.unit_value_minor)?;
    let ccy = input
        .currency_code
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .unwrap_or(account_ccy);
    let as_of = input.as_of.to_string();
    Ok(sqlx::query_as!(
        EquityMarkRow,
        r#"INSERT INTO equity_marks (account_id, as_of, unit_value_minor, currency_code, note)
           VALUES (?1,?2,?3,?4,?5)
           ON CONFLICT(account_id, as_of) DO UPDATE SET
               unit_value_minor=excluded.unit_value_minor,
               currency_code=excluded.currency_code, note=excluded.note
           RETURNING id AS "id!", account_id, as_of, unit_value_minor, currency_code, note,
                     created_at"#,
        account_id,
        as_of,
        input.unit_value_minor,
        ccy,
        input.note
    )
    .fetch_one(db)
    .await?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_mark(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM equity_marks WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("mark"));
    }
    Ok(())
}

fn validate_mark(unit_value_minor: i64) -> AppResult<()> {
    if unit_value_minor < 0 {
        return Err(AppError::validation("unit value cannot be negative"));
    }
    if unit_value_minor > MAX_MONEY_MINOR {
        return Err(AppError::validation(format!(
            "unit value must be at most {MAX_MONEY_MINOR} minor units"
        )));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_grants(db: &Db, account_id: i64) -> AppResult<Vec<EquityGrant>> {
    Ok(sqlx::query_as!(
        EquityGrantRow,
        r#"SELECT id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                  currency_code, vest_months, cliff_months, unit_value_minor, note, created_at,
                  updated_at
             FROM equity_grants WHERE account_id=?1 ORDER BY grant_date, id"#,
        account_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_grant(db: &Db, account_id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
    let account_ccy =
        sqlx::query_scalar!("SELECT currency_code FROM accounts WHERE id=?1", account_id)
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
    let company = input.company.trim();
    let grant_date = input.grant_date.to_string();
    let vest_months = input.vest_months.max(1);
    let cliff_months = input.cliff_months.max(0);
    let grant: EquityGrant = sqlx::query_as!(
        EquityGrantRow,
        r#"INSERT INTO equity_grants
              (account_id, company, grant_date, quantity, strike_minor, currency_code,
               vest_months, cliff_months, unit_value_minor, note)
           VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
           RETURNING id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                     currency_code, vest_months, cliff_months, unit_value_minor, note, created_at,
                     updated_at"#,
        account_id,
        company,
        grant_date,
        input.quantity,
        input.strike_minor,
        ccy.clone(),
        vest_months,
        cliff_months,
        input.unit_value_minor,
        input.note
    )
    .fetch_one(db)
    .await?
    .into();
    // `unit_value_minor` is a convenience on the way in, not a field valuation reads: record it
    // as a mark dated at the grant, which is the earliest it could have applied. Upserting means
    // adding a second grant with the same price is a no-op rather than a duplicate, and adding
    // one with a *newer* price does not silently restate the older grant's history — it lands on
    // its own date and carries forward from there.
    if let Some(unit_value_minor) = input.unit_value_minor {
        create_mark(
            db,
            account_id,
            SaveMark {
                as_of: input.grant_date,
                unit_value_minor,
                currency_code: Some(ccy),
                note: Some("set when the grant was added".to_string()),
            },
        )
        .await?;
    }
    Ok(grant)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update_grant(db: &Db, id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
    validate_grant(&input)?;
    let company = input.company.trim();
    let grant_date = input.grant_date.to_string();
    let vest_months = input.vest_months.max(1);
    let cliff_months = input.cliff_months.max(0);
    Ok(sqlx::query_as!(
        EquityGrantRow,
        r#"UPDATE equity_grants SET company=?2, grant_date=?3, quantity=?4, strike_minor=?5,
              vest_months=?6, cliff_months=?7, unit_value_minor=?8, note=?9,
              updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id=?1
           RETURNING id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                     currency_code, vest_months, cliff_months, unit_value_minor, note, created_at,
                     updated_at"#,
        id,
        company,
        grant_date,
        input.quantity,
        input.strike_minor,
        vest_months,
        cliff_months,
        input.unit_value_minor,
        input.note
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("grant"))?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_grant(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM equity_grants WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("grant"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_exercises(db: &Db, grant_id: i64) -> AppResult<Vec<EquityExercise>> {
    Ok(sqlx::query_as!(
        EquityExerciseRow,
        r#"SELECT id AS "id!", grant_id, exercise_date, quantity, price_minor, note, created_at
             FROM equity_exercises WHERE grant_id=?1 ORDER BY exercise_date, id"#,
        grant_id
    )
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
    let as_of = input.exercise_date.date();
    // No mark needed: this check is about *units*, and how many are exercisable on a date does
    // not depend on what one is worth. Passing `None` keeps a grant on an account with no mark
    // yet fully usable — you can record an exercise before anybody has priced the company.
    let status = compute_status(db, &grant, as_of, None).await?;
    if input.quantity > status.vested_unexercised {
        return Err(AppError::validation(format!(
            "only {} vested & unexercised units available",
            status.vested_unexercised
        )));
    }
    let exercise_date = input.exercise_date.to_string();
    Ok(sqlx::query_as!(
        EquityExerciseRow,
        r#"INSERT INTO equity_exercises (grant_id, exercise_date, quantity, price_minor, note)
           VALUES (?1,?2,?3,?4,?5)
           RETURNING id AS "id!", grant_id, exercise_date, quantity, price_minor, note, created_at"#,
        grant_id,
        exercise_date,
        input.quantity,
        input.price_minor,
        input.note
    )
    .fetch_one(db)
    .await?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_exercise(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM equity_exercises WHERE id=?1", id)
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
    let mark = mark_at(db, grant.account_id, as_of).await?;
    compute_status(db, &grant, as_of, mark.map(|m| m.unit_value_minor)).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn account_equity(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
    let as_of = as_of
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let account_ccy = sqlx::query_scalar!("SELECT currency_code FROM accounts WHERE id=?1", id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("account"))?;
    let grants: Vec<EquityGrant> = sqlx::query_as!(
        EquityGrantRow,
        r#"SELECT id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                  currency_code, vest_months, cliff_months, unit_value_minor, note, created_at,
                  updated_at
             FROM equity_grants WHERE account_id=?1 ORDER BY grant_date, id"#,
        id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();
    // One lookup for the whole account: the mark is a property of the company, not of a grant.
    let mark = mark_at(db, id, as_of).await?;
    let mark_minor = mark.as_ref().map(|m| m.unit_value_minor);
    let mut statuses = Vec::new();
    // Each grant's figures fit an i64 on their own (`compute_status` guarantees it), but two
    // near-ceiling grants still sum past it — and `total_value` is the number `revalue` writes
    // into `valuations`. Accumulate wide, then narrow once each with a real error.
    let mut intrinsic: i128 = 0;
    let mut owned: i128 = 0;
    for g in &grants {
        let s = compute_status(db, g, as_of, mark_minor).await?;
        intrinsic += s.intrinsic_value_minor as i128;
        owned += s.owned_value_minor as i128;
        statuses.push(s);
    }
    let narrow = |total: i128, what: &str| -> AppResult<i64> {
        total.try_into().map_err(|_| {
            AppError::validation(format!(
                "account {id} total equity {what} does not fit across {} grants",
                grants.len()
            ))
        })
    };
    let total_intrinsic = narrow(intrinsic, "intrinsic value")?;
    let total_owned = narrow(owned, "owned value")?;
    let total_value = narrow(intrinsic + owned, "value")?;
    Ok(AccountEquity {
        account_id: id,
        as_of: as_of.to_string(),
        currency_code: account_ccy,
        grants: statuses,
        total_intrinsic_minor: total_intrinsic,
        total_owned_minor: total_owned,
        total_value_minor: total_value,
        unit_value_minor: mark_minor,
        unit_value_as_of: mark.map(|m| m.as_of),
    })
}

/// Snapshot the account's current equity value into a valuation.
///
/// That is [`AccountEquity::total_value_minor`] — vested-unexercised intrinsic *plus* shares
/// already exercised and held. It used to be the intrinsic figure alone, which made every
/// exercise look like the position had shrunk by the market value of the units exercised: the
/// options left `vested_unexercised` and the shares they became were counted nowhere, so a
/// fully-exercised grant valued at zero.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn revalue(db: &Db, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
    let equity = account_equity(db, id, as_of).await?;
    write_valuation(db, id, &equity).await?;
    Ok(equity)
}

/// Upsert one date's equity valuation. Idempotent per (account, date) — see
/// `0040_equity_valuations_daily.sql`; revaluing the same day twice refreshes the row rather
/// than stacking another beside it, which is what makes [`rebuild_history`] safe to re-run.
async fn write_valuation(db: &Db, id: i64, equity: &AccountEquity) -> AppResult<()> {
    let source = ValuationSource::Equity.as_str();
    sqlx::query!(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1,?2,?3,?4,?5,'equity revaluation')
         ON CONFLICT(account_id, as_of) WHERE source='equity' DO UPDATE SET
             value_minor=excluded.value_minor, currency_code=excluded.currency_code",
        id,
        equity.as_of,
        equity.total_value_minor,
        equity.currency_code,
        source
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Every date this account's value can change, oldest first, capped at `today`.
///
/// Exactly the vesting tranche dates, the exercise dates and the mark dates — nothing else moves
/// the figure, and a valuation is a level that carries forward until the next one, so this set is
/// the *whole* history rather than a sample of it. Sampling monthly regardless would miss a
/// mid-month exercise; sampling daily would repeat a carried-forward number a thousand times.
async fn value_change_dates(
    db: &Db,
    account_id: i64,
    today: NaiveDate,
) -> AppResult<Vec<NaiveDate>> {
    let grants = list_grants(db, account_id).await?;
    let mut dates: std::collections::BTreeSet<NaiveDate> = std::collections::BTreeSet::new();
    for g in &grants {
        let Some(start) = parse_date(&g.grant_date) else {
            continue;
        };
        // Tranche n vests at start+n months, for n in 0..=vest_months. n=0 is the grant date
        // itself, which is where the series should start from zero rather than from whatever the
        // first tranche happens to be.
        for n in 0..=g.vest_months.max(1) {
            let Some(d) = add_months(start, n) else { break };
            if d > today {
                break;
            }
            dates.insert(d);
        }
    }
    for e in sqlx::query_scalar!(
        // Read out of a join, so SQLite describes it nullable; `!` names what the column
        // already guarantees.
        r#"SELECT e.exercise_date AS "exercise_date!" FROM equity_exercises e
             JOIN equity_grants g ON g.id = e.grant_id
            WHERE g.account_id = ?1"#,
        account_id
    )
    .fetch_all(db)
    .await?
    {
        if let Some(d) = parse_date(&e).filter(|d| *d <= today) {
            dates.insert(d);
        }
    }
    for m in list_marks(db, account_id).await? {
        if let Some(d) = parse_date(&m.as_of).filter(|d| *d <= today) {
            dates.insert(d);
        }
    }
    if !dates.is_empty() {
        // The series has to reach the present or the latest valuation is a stale level that
        // carries forward as today's value.
        dates.insert(today);
    }
    Ok(dates.into_iter().collect())
}

/// The largest number of valuations one rebuild may write.
///
/// A guard against a mistyped grant date, not a real limit: the dates come from vesting
/// schedules, so a grant accidentally dated 1970 would otherwise write six hundred rows nobody
/// asked for. Four grants over a decade is well under a hundred.
const MAX_REBUILD_VALUATIONS: usize = 1_000;

/// Recompute this account's whole valuation history from the grant schedule and the mark ledger.
///
/// This is the payoff of separating quantity from price. Quantity on any date was always exact —
/// derived from the deeds — and now that price is a series too, the value on any past date is
/// computable rather than something a human has to remember and type. Re-runnable: each date
/// upserts (see [`write_valuation`]), so correcting a mark and rebuilding restates the history
/// instead of doubling it.
#[tracing::instrument(level = "debug", skip_all)]
/// Every account that holds equity, oldest id first.
///
/// "Holds equity" means it has at least one grant: a price mark on an account with no grants
/// values nothing, and an account with grants always has a history worth rebuilding even before
/// anyone records a price. The background rebuild walks this rather than every account, so a
/// household with no ESOP does no work at all.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn accounts_with_equity(db: &Db) -> AppResult<Vec<i64>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT account_id AS "account_id!" FROM equity_grants
                                ORDER BY account_id"#
    )
    .fetch_all(db)
    .await?)
}

pub async fn rebuild_history(db: &Db, id: i64, today: Option<&str>) -> AppResult<RebuildResult> {
    let today = today
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let dates = value_change_dates(db, id, today).await?;
    if dates.len() > MAX_REBUILD_VALUATIONS {
        return Err(AppError::validation(format!(
            "rebuilding would write {} valuations, past the {MAX_REBUILD_VALUATIONS} limit — \
             check the grant dates",
            dates.len()
        )));
    }
    let from = dates.first().map(|d| d.to_string());
    let to = dates.last().map(|d| d.to_string());
    let mut written = 0;
    for d in &dates {
        let equity = account_equity(db, id, Some(&d.to_string())).await?;
        write_valuation(db, id, &equity).await?;
        written += 1;
    }
    Ok(RebuildResult { written, from, to })
}

/// What this position will be worth at each month from `from`, for `0..=months`.
///
/// The quantity ramp a private holding is projected along. Vesting is contractual — the deed
/// already says how many units land on the 1st of each month for the next four years — so the
/// *quantity* side of a future value is known rather than fitted. Price is not: each month is
/// valued at the mark in force then, which for a future date is the latest one recorded, and the
/// forecast applies its own growth assumption on top of this ramp.
///
/// This is what a fitted trend cannot express. Read as a series of past values, a grant vesting
/// into a re-marked company looks like an asset compounding at hundreds of percent; projecting
/// that forward is nonsense, while projecting a flat value ignores four years of contractual
/// vesting. The ramp is the honest middle.
///
/// One `account_equity` call per month rather than a bespoke in-memory computation: the vesting formula
/// then has exactly one implementation, and these are local prepared queries called once per
/// forecast, not per simulated path.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn projected_values(
    db: &Db,
    id: i64,
    from: NaiveDate,
    months: i64,
) -> AppResult<Vec<i64>> {
    let mut out = Vec::with_capacity(months.max(0) as usize + 1);
    for m in 0..=months.max(0) {
        let Some(d) = add_months(from, m) else { break };
        out.push(
            account_equity(db, id, Some(&d.to_string()))
                .await?
                .total_value_minor,
        );
    }
    Ok(out)
}

/// The dated vesting/exercise ledger: what this account holds and when that changed.
///
/// Vesting rows are computed, not stored — a tranche is not something anybody records, it is
/// what the deed already says happens on the 1st of each month — so this is assembled on read.
/// Exercises are real rows. Together they are the quantity half of the account's value, the
/// counterpart to a brokerage account's `holdings` lots.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_events(
    db: &Db,
    account_id: i64,
    as_of: Option<&str>,
) -> AppResult<Vec<EquityEvent>> {
    let as_of = as_of
        .and_then(parse_date)
        .unwrap_or_else(|| Utc::now().date_naive());
    let grants = list_grants(db, account_id).await?;
    let marks = list_marks(db, account_id).await?;
    // Ascending, so the running mark can be advanced in step with the events below.
    let mut marks: Vec<(NaiveDate, i64)> = marks
        .iter()
        .filter_map(|m| parse_date(&m.as_of).map(|d| (d, m.unit_value_minor)))
        .collect();
    marks.sort();
    let mark_on = |d: NaiveDate| -> Option<i64> {
        marks.iter().rev().find(|(md, _)| *md <= d).map(|(_, v)| *v)
    };

    let mut events = Vec::new();
    for g in &grants {
        let Some(start) = parse_date(&g.grant_date) else {
            continue;
        };
        let vest_months = g.vest_months.max(1);
        let cliff = g.cliff_months.max(0);
        // Cumulative vested after n months, the same floor() the status computation uses — so
        // the tranche sizes here always add up to what `grant_vesting` reports, including the
        // rounding that makes one month in four a unit larger.
        let vested_after = |n: i64| -> i64 {
            if n < cliff {
                0
            } else {
                ((g.quantity as i128 * n.min(vest_months) as i128) / vest_months as i128) as i64
            }
        };
        for n in 1..=vest_months {
            let Some(d) = add_months(start, n) else { break };
            if d > as_of {
                break;
            }
            let tranche = vested_after(n) - vested_after(n - 1);
            if tranche == 0 {
                continue;
            }
            events.push(EquityEvent {
                date: d.to_string(),
                grant_id: g.id,
                grant_label: g.note.clone(),
                // The cliff month is not a bigger tranche, it is the one date where a year of
                // them lands at once — worth naming so a reader is not left wondering why.
                kind: if n == cliff {
                    EquityEventKind::Cliff
                } else {
                    EquityEventKind::Vest
                },
                quantity: tranche,
                vested_running: vested_after(n),
                exercised_running: 0,
                unit_value_minor: mark_on(d),
                note: None,
            });
        }
        for ex in list_exercises(db, g.id).await? {
            let Some(d) = parse_date(&ex.exercise_date).filter(|d| *d <= as_of) else {
                continue;
            };
            events.push(EquityEvent {
                date: ex.exercise_date.clone(),
                grant_id: g.id,
                grant_label: g.note.clone(),
                kind: EquityEventKind::Exercise,
                quantity: ex.quantity,
                vested_running: vested_after(months_between(start, d)),
                exercised_running: 0,
                unit_value_minor: mark_on(d),
                note: ex.note.clone(),
            });
        }
    }
    // Newest first, matching every other ledger in the app; within a date, vesting before the
    // exercise it made possible.
    events.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then_with(|| a.grant_id.cmp(&b.grant_id))
            .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
    });
    // Running exercised totals, per grant, computed oldest-first then left on the rows.
    let mut running: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    for e in events.iter_mut().rev() {
        let acc = running.entry(e.grant_id).or_insert(0);
        if e.kind == EquityEventKind::Exercise {
            *acc += e.quantity;
        }
        e.exercised_running = *acc;
    }
    Ok(events)
}

/// `date` plus `months` calendar months, clamping the day to the target month's length.
///
/// Returns `None` only on a date arithmetic overflow — a year past `NaiveDate`'s range, which a
/// mistyped grant date can reach. Callers stop their loop rather than treating it as "today".
fn add_months(date: NaiveDate, months: i64) -> Option<NaiveDate> {
    let total = date.year() as i64 * 12 + (date.month0() as i64) + months;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month = total.rem_euclid(12) as u32 + 1;
    let last = days_in_month(year, month)?;
    NaiveDate::from_ymd_opt(year, month, date.day().min(last))
}

fn days_in_month(year: i32, month: u32) -> Option<u32> {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = NaiveDate::from_ymd_opt(ny, nm, 1)?;
    Some((next - first).num_days() as u32)
}

/// The mark in force on a date: the latest one dated on or before it.
///
/// A mark is a level that carries forward, exactly like a `valuations` row — so "the price on
/// 2025-03-14" is the last one set, not one that has to exist on that date. `None` means the
/// ledger starts later than the date asked about, and the caller values the position at zero
/// rather than reaching for a price nobody supplied.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn mark_at(db: &Db, account_id: i64, as_of: NaiveDate) -> AppResult<Option<EquityMark>> {
    let as_of = as_of.to_string();
    Ok(sqlx::query_as!(
        EquityMarkRow,
        r#"SELECT id AS "id!", account_id, as_of, unit_value_minor, currency_code, note,
                  created_at
             FROM equity_marks WHERE account_id=?1 AND as_of <= ?2
            ORDER BY as_of DESC LIMIT 1"#,
        account_id,
        as_of
    )
    .fetch_optional(db)
    .await?
    .map(Into::into))
}

/// Compute one grant's status against a mark the caller has already resolved.
///
/// The mark is passed in rather than looked up here because `account_equity` resolves it once
/// for the whole account: every grant on an account is equity in the same company, so a
/// per-grant lookup would be the same query repeated and could not disagree usefully anyway.
#[tracing::instrument(level = "debug", skip_all)]
async fn compute_status(
    db: &Db,
    grant: &EquityGrant,
    as_of: NaiveDate,
    mark_minor: Option<i64>,
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

    let as_of_text = as_of.to_string();
    let exercised: i64 = sqlx::query_scalar!(
        // SUM over no rows is NULL, so this is genuinely nullable — `unwrap_or(0)` below is
        // "nothing exercised yet", not a swallowed decode failure.
        r#"SELECT SUM(quantity) AS "exercised: i64"
             FROM equity_exercises WHERE grant_id=?1 AND exercise_date <= ?2"#,
        grant.id,
        as_of_text
    )
    .fetch_one(db)
    .await?
    .unwrap_or(0);

    let vested_unexercised = vested.saturating_sub(exercised).max(0);
    // The mark and `strike_minor` come straight off their rows, so `validate_grant`/`validate_mark`
    // ceilings only cover what was written through this crate: a row from before they existed, or
    // one edited by hand, can still hold extremes where the *subtraction* alone overflows
    // (`i64::MAX - -1`). Saturating keeps that from panicking before the multiply below has a
    // chance to report anything.
    let per_unit_gain = mark_minor
        .map(|v| v.saturating_sub(grant.strike_minor).max(0))
        .unwrap_or(0);
    // `revalue` persists this product into `valuations`, so it must never wrap: with no
    // `overflow-checks` in `[profile.release]`, an unchecked multiply is a debug panic (a 500)
    // and, in release, a large *negative* valuation that is then handed back verbatim as the
    // account's value for every later date. Widen exactly as the `vested` maths above does and
    // refuse the grant rather than store a fiction.
    let intrinsic: i64 = (vested_unexercised as i128 * per_unit_gain as i128)
        .try_into()
        .map_err(|_| {
            AppError::validation(format!(
                "grant {} intrinsic value does not fit: {} units x {} per-unit gain",
                grant.id, vested_unexercised, per_unit_gain
            ))
        })?;

    // Exercised units are shares now, and a share is worth the whole unit value — not the
    // intrinsic spread, which is what an *option* is worth. Without this the grant's value
    // falls by the full market price of every unit exercised, so exercising reads as the
    // position being sold off rather than converted.
    //
    // At full price rather than `per_unit_gain` because the strike on these units is already
    // spent: it left a bank account on the exercise date. Widened for the same reason the
    // intrinsic product is (`revalue` persists the sum), and negative marks are floored at
    // zero — a share cannot be worth less than nothing, and an unset `unit_value_minor` means
    // "no mark", which values at zero exactly as it does above.
    let owned = exercised;
    let unit_value = mark_minor.unwrap_or(0).max(0);
    let owned_value: i64 = (owned as i128 * unit_value as i128)
        .try_into()
        .map_err(|_| {
            AppError::validation(format!(
                "grant {} owned value does not fit: {} shares x {} unit value",
                grant.id, owned, unit_value
            ))
        })?;
    // Both halves fit an i64 individually; their sum need not.
    let total_value: i64 = (intrinsic as i128 + owned_value as i128)
        .try_into()
        .map_err(|_| {
            AppError::validation(format!(
                "grant {} total value does not fit: {intrinsic} intrinsic + {owned_value} owned",
                grant.id
            ))
        })?;

    Ok(VestingStatus {
        grant_id: grant.id,
        company: grant.company.clone(),
        as_of: as_of.to_string(),
        quantity: grant.quantity,
        vested,
        unvested: grant.quantity - vested,
        exercised,
        vested_unexercised,
        owned,
        strike_minor: grant.strike_minor,
        unit_value_minor: mark_minor,
        currency_code: grant.currency_code.clone(),
        intrinsic_value_minor: intrinsic,
        owned_value_minor: owned_value,
        total_value_minor: total_value,
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
    // Bound the three arithmetic inputs here, at the edge that can still explain itself. Left
    // unbounded, a mistyped quantity or unit value is accepted silently and only surfaces as a
    // rejected `GET /accounts/{id}/equity` (or a refused revaluation) long afterwards, pointing
    // at a grant id rather than the field the user typed.
    if input.quantity > MAX_GRANT_QUANTITY {
        return Err(AppError::validation(format!(
            "quantity must be at most {MAX_GRANT_QUANTITY}"
        )));
    }
    if !(-MAX_MONEY_MINOR..=MAX_MONEY_MINOR).contains(&input.strike_minor) {
        return Err(AppError::validation(format!(
            "strike_minor must be within +/-{MAX_MONEY_MINOR} minor units"
        )));
    }
    if input
        .unit_value_minor
        .is_some_and(|v| !(-MAX_MONEY_MINOR..=MAX_MONEY_MINOR).contains(&v))
    {
        return Err(AppError::validation(format!(
            "unit_value_minor must be within +/-{MAX_MONEY_MINOR} minor units"
        )));
    }
    // The ceilings above are individually generous enough that a legal pair still multiplies
    // past an i64 (1e12 units at $10 a unit is 1e15 minor units — fine; 4e9 units at $40m a unit
    // is not). `compute_status` refuses such a grant on every read, which is correct but useless
    // to the user: the account's equity endpoint 422s from then on, naming a grant id rather than
    // the field they mistyped. Check the same product here, where they can still fix it.
    if let Some(unit_value) = input.unit_value_minor {
        let per_unit_gain = (unit_value as i128 - input.strike_minor as i128).max(0);
        if input.quantity as i128 * per_unit_gain > i64::MAX as i128 {
            return Err(AppError::validation(
                "quantity x (unit_value_minor - strike_minor) is too large to represent",
            ));
        }
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn fetch_grant(db: &Db, id: i64) -> AppResult<EquityGrant> {
    Ok(sqlx::query_as!(
        EquityGrantRow,
        r#"SELECT id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                  currency_code, vest_months, cliff_months, unit_value_minor, note, created_at,
                  updated_at
                 FROM equity_grants WHERE id=?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("grant"))?
    .into())
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{AccountKind, AccountMetadata, IsoDate, Ownership, SaveAccount, SharesMeta};

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    async fn test_account(db: &Db) -> i64 {
        crate::accounts::create(
            db,
            SaveAccount {
                name: "Startup equity".to_string(),
                kind: AccountKind::SharesPrivate,
                currency_code: "NZD".to_string(),
                institution: Some("Sharesies".to_string()),
                // An unlisted holding needs only its broker (see `KIND_REQUIRED`); none of it
                // matters to these tests, which are about the vesting arithmetic.
                metadata: Some(AccountMetadata::Shares(SharesMeta {
                    broker: Some("Sharesies".to_string()),
                    ..Default::default()
                })),
                archived: false,
                sort_order: 0,
                // Zero seeds no meaningful valuation, leaving only what `revalue` writes.
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".to_string()),
                // These tests don't care who owns the account; joint needs no person row.
                ownership: Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id
    }

    /// Insert a grant row straight into SQLite, bypassing [`validate_grant`] — how a grant that
    /// predates the input ceilings, or one edited by hand, actually looks on disk. The read path
    /// has to survive these on its own, which is what the overflow tests below exercise.
    async fn insert_unvalidated(
        db: &Db,
        account_id: i64,
        quantity: i64,
        strike_minor: i64,
        unit_value_minor: Option<i64>,
    ) -> i64 {
        let id = sqlx::query_scalar!(
            r#"INSERT INTO equity_grants
                  (account_id, company, grant_date, quantity, strike_minor, currency_code,
                   vest_months, cliff_months, unit_value_minor)
               VALUES (?1,'Acme','2020-01-01',?2,?3,'NZD',48,12,?4)
               RETURNING id AS "id!""#,
            account_id,
            quantity,
            strike_minor,
            unit_value_minor
        )
        .fetch_one(db)
        .await
        .unwrap();
        // The mark ledger is what valuation reads, so an extreme price has to land *there* for
        // these tests to reach the arithmetic they exist for. Written straight to the table,
        // like the grant above: `validate_mark` would reject it at the edge, which is the
        // point — these cover the rows that got in before those ceilings existed, or by hand.
        if let Some(unit_value_minor) = unit_value_minor {
            sqlx::query!(
                "INSERT INTO equity_marks (account_id, as_of, unit_value_minor, currency_code)
                 VALUES (?1,'2019-01-01',?2,'NZD')
                 ON CONFLICT(account_id, as_of) DO UPDATE SET
                     unit_value_minor=excluded.unit_value_minor",
                account_id,
                unit_value_minor
            )
            .execute(db)
            .await
            .unwrap();
        }
        id
    }

    fn grant(quantity: i64, strike_minor: i64, unit_value_minor: Option<i64>) -> SaveGrant {
        SaveGrant {
            company: "Acme".to_string(),
            grant_date: IsoDate::parse("2020-01-01").unwrap(),
            quantity,
            strike_minor,
            currency_code: None,
            vest_months: 48,
            cliff_months: 12,
            unit_value_minor,
            note: None,
        }
    }

    fn validation_message<T: std::fmt::Debug>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::Validation(msg)) => msg,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_ordinary_grant_computes_its_intrinsic_value() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // 4,800 units over 48 months from 2020-01-01, so fully vested by 2024-01-01; a $25.00
        // unit value against a $1.00 strike is $24.00 of gain on each.
        let g = create_grant(&db, account, grant(4_800, 100, Some(2_500)))
            .await
            .unwrap();
        let status = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        assert_eq!(status.vested, 4_800);
        assert_eq!(status.intrinsic_value_minor, 4_800 * 2_400);
    }

    async fn mark(db: &Db, account: i64, as_of: &str, unit_value_minor: i64) {
        create_mark(
            db,
            account,
            SaveMark {
                as_of: IsoDate::parse(as_of).unwrap(),
                unit_value_minor,
                currency_code: None,
                note: None,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_past_date_is_priced_at_the_mark_that_applied_then() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // No unit value on the grant: the mark ledger below is the only price.
        let g = create_grant(&db, account, grant(4_800, 100, None))
            .await
            .unwrap();
        mark(&db, account, "2020-01-01", 1_000).await;
        mark(&db, account, "2024-01-01", 2_500).await;

        // 2023 is still on the old mark, even though a newer one exists — the whole reason price
        // is a ledger. Before this, valuing a past date used whatever the grant currently said.
        let then = grant_vesting(&db, g.id, Some("2023-01-01")).await.unwrap();
        assert_eq!(then.vested, 3_600); // 36/48
        assert_eq!(then.unit_value_minor, Some(1_000));
        assert_eq!(then.intrinsic_value_minor, 3_600 * 900);

        let now = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        assert_eq!(now.unit_value_minor, Some(2_500));
        assert_eq!(now.intrinsic_value_minor, 4_800 * 2_400);
    }

    #[tokio::test]
    async fn a_date_before_the_first_mark_values_at_zero_with_exact_quantities() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let g = create_grant(&db, account, grant(4_800, 100, None))
            .await
            .unwrap();
        mark(&db, account, "2024-01-01", 2_500).await;

        // Quantities are known from the deed regardless of whether anybody has priced the
        // company; value is not invented in the gap.
        let s = grant_vesting(&db, g.id, Some("2022-01-01")).await.unwrap();
        assert_eq!(s.vested, 2_400);
        assert_eq!(s.unit_value_minor, None);
        assert_eq!(s.total_value_minor, 0);
    }

    #[tokio::test]
    async fn a_mark_replaces_the_one_already_on_its_date() {
        let db = test_db().await;
        let account = test_account(&db).await;
        create_grant(&db, account, grant(4_800, 100, None))
            .await
            .unwrap();
        mark(&db, account, "2024-01-01", 2_500).await;
        mark(&db, account, "2024-01-01", 2_600).await;
        let marks = list_marks(&db, account).await.unwrap();
        assert_eq!(
            marks.len(),
            1,
            "a corrected figure replaces, it does not stack"
        );
        assert_eq!(marks[0].unit_value_minor, 2_600);
    }

    #[tokio::test]
    async fn a_grant_created_with_a_unit_value_seeds_the_mark_ledger() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // The convenience field still works — it lands as a mark dated at the grant.
        create_grant(&db, account, grant(4_800, 100, Some(2_500)))
            .await
            .unwrap();
        let marks = list_marks(&db, account).await.unwrap();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].as_of, "2020-01-01");
        assert_eq!(marks[0].unit_value_minor, 2_500);
    }

    #[tokio::test]
    async fn rebuilding_writes_one_valuation_per_change_and_is_idempotent() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // 48 units over 48 months from 2020-01-01, 12-month cliff, so one unit vests a month.
        let g = create_grant(&db, account, grant(48, 0, None))
            .await
            .unwrap();
        mark(&db, account, "2020-01-01", 100).await;
        create_exercise(
            &db,
            g.id,
            SaveExercise {
                exercise_date: IsoDate::parse("2021-06-15").unwrap(),
                quantity: 10,
                price_minor: 0,
                note: None,
            },
        )
        .await
        .unwrap();

        let first = rebuild_history(&db, account, Some("2022-01-01"))
            .await
            .unwrap();
        // 25 tranche dates (2020-01-01 through 2022-01-01 inclusive) plus the mid-month
        // exercise. Every one of them is a date the value actually moved.
        assert_eq!(first.written, 26);
        assert_eq!(first.from.as_deref(), Some("2020-01-01"));
        assert_eq!(first.to.as_deref(), Some("2022-01-01"));

        async fn equity_rows(db: &Db) -> i64 {
            sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "n: i64" FROM valuations WHERE source='equity'"#
            )
            .fetch_one(db)
            .await
            .unwrap()
        }
        assert_eq!(equity_rows(&db).await, 26);

        // Re-running restates rather than doubling — what makes correcting a mark safe.
        let again = rebuild_history(&db, account, Some("2022-01-01"))
            .await
            .unwrap();
        assert_eq!(again.written, 26);
        assert_eq!(equity_rows(&db).await, 26);

        // A corrected mark flows through the whole series on the next rebuild.
        mark(&db, account, "2020-01-01", 200).await;
        rebuild_history(&db, account, Some("2022-01-01"))
            .await
            .unwrap();
        let at_cliff = sqlx::query_scalar!(
            "SELECT value_minor FROM valuations WHERE source='equity' AND as_of='2021-01-01'"
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(at_cliff, 12 * 200);
    }

    #[tokio::test]
    async fn the_event_ledger_names_the_cliff_and_totals_to_the_grant() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let g = create_grant(&db, account, grant(48, 0, Some(100)))
            .await
            .unwrap();
        create_exercise(
            &db,
            g.id,
            SaveExercise {
                exercise_date: IsoDate::parse("2021-06-15").unwrap(),
                quantity: 10,
                price_minor: 0,
                note: None,
            },
        )
        .await
        .unwrap();

        let events = list_events(&db, account, Some("2024-01-01")).await.unwrap();
        // Newest first.
        assert!(events.first().unwrap().date > events.last().unwrap().date);

        let cliffs: Vec<_> = events
            .iter()
            .filter(|e| e.kind == EquityEventKind::Cliff)
            .collect();
        assert_eq!(cliffs.len(), 1, "exactly one cliff on this grant");
        assert_eq!(cliffs[0].date, "2021-01-01");
        assert_eq!(cliffs[0].quantity, 12, "a year's tranches land at once");

        // The tranches must add up to the grant, or the ledger and the status disagree.
        let vested: i64 = events
            .iter()
            .filter(|e| e.kind != EquityEventKind::Exercise)
            .map(|e| e.quantity)
            .sum();
        let status = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        assert_eq!(vested, status.vested);
        assert_eq!(vested, 48);

        let exercises: Vec<_> = events
            .iter()
            .filter(|e| e.kind == EquityEventKind::Exercise)
            .collect();
        assert_eq!(exercises.len(), 1);
        assert_eq!(exercises[0].exercised_running, 10);
        assert_eq!(exercises[0].unit_value_minor, Some(100));
    }

    #[tokio::test]
    async fn exercising_moves_value_across_rather_than_destroying_it() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // 4,800 units, fully vested by 2024-01-01: $25.00 a unit against a $1.00 strike.
        let g = create_grant(&db, account, grant(4_800, 100, Some(2_500)))
            .await
            .unwrap();
        let before = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        assert_eq!(before.total_value_minor, 4_800 * 2_400);
        assert_eq!(before.owned, 0);

        create_exercise(
            &db,
            g.id,
            SaveExercise {
                exercise_date: IsoDate::parse("2024-01-01").unwrap(),
                quantity: 3_000,
                price_minor: 100,
                note: None,
            },
        )
        .await
        .unwrap();

        let after = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        assert_eq!(after.owned, 3_000);
        assert_eq!(after.vested_unexercised, 1_800);
        // The 1,800 options still carry only their $24.00 spread...
        assert_eq!(after.intrinsic_value_minor, 1_800 * 2_400);
        // ...but the 3,000 exercised units are shares now, worth the whole $25.00: the strike
        // was paid in cash on the exercise date, so netting it off again would double-count it.
        assert_eq!(after.owned_value_minor, 3_000 * 2_500);
        // Exercising is a conversion, not a disposal, so the total may only go *up* here — by
        // exactly the strike now sunk into the 3,000 shares. Before this was modelled it fell
        // by $75,000, the full market value of everything exercised.
        assert_eq!(after.total_value_minor, 1_800 * 2_400 + 3_000 * 2_500);
        assert!(after.total_value_minor > before.total_value_minor);
    }

    #[tokio::test]
    async fn a_fully_exercised_grant_is_still_worth_its_shares() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let g = create_grant(&db, account, grant(4_800, 100, Some(2_500)))
            .await
            .unwrap();
        create_exercise(
            &db,
            g.id,
            SaveExercise {
                exercise_date: IsoDate::parse("2024-01-01").unwrap(),
                quantity: 4_800,
                price_minor: 100,
                note: None,
            },
        )
        .await
        .unwrap();
        let status = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        // Nothing left to exercise, so no intrinsic value — and for as long as that was the
        // only figure, a grant fully converted into shares valued at zero.
        assert_eq!(status.intrinsic_value_minor, 0);
        assert_eq!(status.total_value_minor, 4_800 * 2_500);

        let equity = account_equity(&db, account, Some("2024-01-01"))
            .await
            .unwrap();
        assert_eq!(equity.total_intrinsic_minor, 0);
        assert_eq!(equity.total_owned_minor, 4_800 * 2_500);
        assert_eq!(equity.total_value_minor, 4_800 * 2_500);
    }

    #[tokio::test]
    async fn a_revaluation_persists_the_whole_position_not_just_the_options() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let g = create_grant(&db, account, grant(4_800, 100, Some(2_500)))
            .await
            .unwrap();
        create_exercise(
            &db,
            g.id,
            SaveExercise {
                exercise_date: IsoDate::parse("2024-01-01").unwrap(),
                quantity: 3_000,
                price_minor: 100,
                note: None,
            },
        )
        .await
        .unwrap();
        let equity = revalue(&db, account, Some("2024-01-01")).await.unwrap();
        let expected = 1_800 * 2_400 + 3_000 * 2_500;
        assert_eq!(equity.total_value_minor, expected);
        // The row written is the whole position: this is the figure net worth reads.
        let persisted = sqlx::query_scalar!(
            "SELECT value_minor FROM valuations WHERE account_id=?1 AND source='equity'",
            account
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(persisted, expected);
    }

    #[tokio::test]
    async fn an_owned_value_past_i64_is_an_error_not_a_wrap() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // Written straight to the table: `validate_grant` bounds the pair on the way in, so a
        // product this size only exists on a row that predates those ceilings or was edited by
        // hand — which is exactly the case `compute_status` still has to survive.
        let id = insert_unvalidated(&db, account, 1_000, 0, Some(i64::MAX)).await;
        sqlx::query!(
            "INSERT INTO equity_exercises (grant_id, exercise_date, quantity) VALUES (?1,'2024-01-01',1000)",
            id
        )
        .execute(&db)
        .await
        .unwrap();
        let message = validation_message(grant_vesting(&db, id, Some("2024-01-01")).await);
        assert!(
            message.contains("owned value does not fit"),
            "unexpected message: {message}"
        );
    }

    #[tokio::test]
    async fn an_underwater_grant_floors_its_intrinsic_value_at_zero() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // Unit value $5.00 under a $10.00 strike: worth nothing, never negative.
        let g = create_grant(&db, account, grant(1_000, 1_000, Some(500)))
            .await
            .unwrap();
        let status = grant_vesting(&db, g.id, Some("2024-01-01")).await.unwrap();
        assert_eq!(status.vested, 1_000);
        assert_eq!(status.intrinsic_value_minor, 0);
    }

    #[tokio::test]
    async fn an_intrinsic_value_past_i64_is_an_error_not_a_wrap() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // 4e9 units x 4e9 minor units of gain is 1.6e19 — past `i64::MAX` (~9.2e18). Before the
        // widening guard this panicked in debug (a 500) and wrapped *negative* in release.
        insert_unvalidated(&db, account, 4_000_000_000, 0, Some(4_000_000_000)).await;
        let message = validation_message(account_equity(&db, account, Some("2024-01-01")).await);
        assert!(
            message.contains("does not fit"),
            "the error should name the arithmetic that failed, got {message:?}"
        );
    }

    #[tokio::test]
    async fn revalue_refuses_to_persist_an_overflowing_intrinsic_value() {
        let db = test_db().await;
        let account = test_account(&db).await;
        insert_unvalidated(&db, account, 4_000_000_000, 0, Some(4_000_000_000)).await;
        revalue(&db, account, Some("2024-01-01"))
            .await
            .expect_err("a wrapped total must never reach the valuations table");
        let source = ValuationSource::Equity.as_str();
        let persisted = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM valuations WHERE account_id=?1 AND source=?2",
            account,
            source
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(persisted, 0, "no equity valuation should have been written");
    }

    #[tokio::test]
    async fn a_negative_strike_saturates_rather_than_overflowing_the_subtraction() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // `i64::MAX - -1` overflows the *subtraction* on its own, before any multiply.
        insert_unvalidated(&db, account, 1, -1, Some(i64::MAX)).await;
        let equity = account_equity(&db, account, Some("2024-01-01"))
            .await
            .unwrap();
        assert_eq!(equity.total_intrinsic_minor, i64::MAX);
    }

    #[tokio::test]
    async fn an_account_total_past_i64_is_an_error_not_a_wrap() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // Two grants that each fit an i64 alone but not together.
        for _ in 0..2 {
            insert_unvalidated(&db, account, 1, 0, Some(i64::MAX)).await;
        }
        let message = validation_message(account_equity(&db, account, Some("2024-01-01")).await);
        assert!(message.contains("total equity intrinsic value does not fit"));
    }

    #[tokio::test]
    async fn an_absurd_quantity_is_rejected_at_the_edge() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let message = validation_message(
            create_grant(&db, account, grant(MAX_GRANT_QUANTITY + 1, 0, Some(1))).await,
        );
        assert!(message.contains("quantity must be at most"));
        // The ceiling itself is still accepted, so the bound is inclusive.
        create_grant(&db, account, grant(MAX_GRANT_QUANTITY, 0, Some(1)))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_money_figure_past_the_ceiling_is_rejected_at_the_edge() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let message = validation_message(
            create_grant(&db, account, grant(10, 0, Some(MAX_MONEY_MINOR + 1))).await,
        );
        assert!(message.contains("unit_value_minor must be within"));

        // `i64::MIN` also proves the bound is not written with `abs()`, which panics on it.
        let message =
            validation_message(create_grant(&db, account, grant(10, i64::MIN, Some(1))).await);
        assert!(message.contains("strike_minor must be within"));
    }

    #[tokio::test]
    async fn a_grant_whose_intrinsic_value_cannot_fit_is_refused_on_creation() {
        let db = test_db().await;
        let account = test_account(&db).await;
        // Each figure clears its own ceiling — 4e9 units, $40m a unit — but the product is 1.6e19,
        // past `i64::MAX`. Caught here, the account's equity endpoint keeps working.
        let message = validation_message(
            create_grant(&db, account, grant(4_000_000_000, 0, Some(4_000_000_000))).await,
        );
        assert!(message.contains("too large to represent"));
        // A strike that eats the gain brings the same pair back under the limit: the check is on
        // the gain, not on the raw unit value.
        create_grant(
            &db,
            account,
            grant(4_000_000_000, 4_000_000_000, Some(4_000_000_000)),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn updating_a_grant_is_bounded_too() {
        let db = test_db().await;
        let account = test_account(&db).await;
        let g = create_grant(&db, account, grant(4_800, 100, Some(2_500)))
            .await
            .unwrap();
        // An update is the other way an overflowing grant would reach the read path.
        let message = validation_message(
            update_grant(&db, g.id, grant(4_000_000_000, 0, Some(4_000_000_000))).await,
        );
        assert!(message.contains("too large to represent"));
    }
}
