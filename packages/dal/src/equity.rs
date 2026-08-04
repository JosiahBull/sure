//! Equity grants, exercises, and computed vesting status.

use chrono::{Datelike, NaiveDate, Utc};
use sqlx::FromRow;
pub use sure_core::{
    AccountEquity, EquityExercise, EquityGrant, SaveExercise, SaveGrant, VestingStatus,
};
use sure_core::{AppError, AppResult, ValuationSource};

use crate::Db;

/// The largest share count a grant may carry. No real grant covers a trillion units —
/// Apple's entire issued float is around 1.5×10^10 — so a figure above this is data
/// entry (a minor-unit amount pasted into the quantity field, most often), and the user
/// is better served by a 422 naming the field than by a grant whose intrinsic value can
/// only ever be rejected later, at read time, on every request that touches the account.
const MAX_GRANT_QUANTITY: i64 = 1_000_000_000_000;

/// The largest per-unit money figure (strike or fair value) a grant may carry, in minor
/// units: $10 trillion. i64 minor units run out around 9.2×10^16 dollars, so this leaves
/// four orders of magnitude of headroom for the sums layered on top (`account_equity`
/// adds every grant's intrinsic value into one i64, and that total is what `revalue`
/// persists) while still admitting the most expensive share on earth many times over.
///
/// Note these two ceilings deliberately do *not* multiply safely into an i64 — a
/// legitimate high-priced grant (Berkshire A at ~$700k a share) forces the money ceiling
/// well above `i64::MAX / MAX_GRANT_QUANTITY`. They are data-entry sanity checks, not the
/// overflow guard: `validate_grant` separately rejects a *pair* whose product cannot fit,
/// and `compute_status` still computes in i128 because neither check has ever run against
/// the rows already on disk.
const MAX_MONEY_MINOR: i64 = 1_000_000_000_000_000;

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
    // Each grant's intrinsic value fits an i64 on its own (`compute_status` guarantees it), but
    // two near-ceiling grants still sum past it — and this total is the number `revalue` writes
    // into `valuations`. Accumulate wide, then narrow once with a real error.
    let mut total: i128 = 0;
    for g in &grants {
        let s = compute_status(db, g, as_of).await?;
        total += s.intrinsic_value_minor as i128;
        statuses.push(s);
    }
    let total: i64 = total.try_into().map_err(|_| {
        AppError::validation(format!(
            "account {id} total equity intrinsic value does not fit across {} grants",
            grants.len()
        ))
    })?;
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

    let vested_unexercised = vested.saturating_sub(exercised).max(0);
    // `unit_value_minor` and `strike_minor` come straight off the row, so `validate_grant`'s
    // ceilings only cover grants written through this crate: a row from before they existed,
    // or one edited by hand, can still hold extremes where the *subtraction* alone overflows
    // (`i64::MAX - -1`). Saturating keeps that from panicking before the multiply below has a
    // chance to report anything.
    let per_unit_gain = grant
        .unit_value_minor
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{AccountKind, AccountMetadata, Ownership, SaveAccount, SharesMeta};

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
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO equity_grants
                (account_id, company, grant_date, quantity, strike_minor, currency_code,
                 vest_months, cliff_months, unit_value_minor)
             VALUES (?1,'Acme','2020-01-01',?2,?3,'NZD',48,12,?4) RETURNING id",
        )
        .bind(account_id)
        .bind(quantity)
        .bind(strike_minor)
        .bind(unit_value_minor)
        .fetch_one(db)
        .await
        .unwrap()
    }

    fn grant(quantity: i64, strike_minor: i64, unit_value_minor: Option<i64>) -> SaveGrant {
        SaveGrant {
            company: "Acme".to_string(),
            grant_date: "2020-01-01".to_string(),
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
        let persisted = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM valuations WHERE account_id=?1 AND source=?2",
        )
        .bind(account)
        .bind(ValuationSource::Equity.as_str())
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
