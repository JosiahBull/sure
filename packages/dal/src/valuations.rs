use sure_core::{AppError, AppResult};
pub use sure_core::{NewValuation, Valuation, ValuationSource};

use crate::Db;

#[derive(Debug)]
struct ValuationRow {
    id: i64,
    account_id: i64,
    as_of: String,
    value_minor: i64,
    currency_code: String,
    source: String,
    note: Option<String>,
    created_at: String,
}

impl TryFrom<ValuationRow> for Valuation {
    type Error = AppError;

    fn try_from(r: ValuationRow) -> AppResult<Self> {
        // The column has no CHECK constraint (sqlite's is limited), but every writer
        // goes through `ValuationSource::as_str`, so a value that doesn't parse means
        // the row was written by something else entirely — surface it as a real error
        // rather than panicking the request.
        let source: ValuationSource = r
            .source
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(Valuation {
            id: r.id,
            account_id: r.account_id,
            as_of: r.as_of,
            value_minor: r.value_minor,
            currency_code: r.currency_code,
            source,
            note: r.note,
            created_at: r.created_at,
        })
    }
}

/// List an account's valuations, newest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_for_account(db: &Db, account_id: i64) -> AppResult<Vec<Valuation>> {
    sqlx::query_as!(
        ValuationRow,
        r#"SELECT id AS "id!", account_id, as_of, value_minor, currency_code, source, note,
                  created_at
             FROM valuations WHERE account_id=?1 ORDER BY as_of DESC, id DESC"#,
        account_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Valuation::try_from)
    .collect()
}

/// Record a valuation for an account, defaulting the currency to the account's.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, account_id: i64, input: NewValuation) -> AppResult<Valuation> {
    let account_ccy =
        sqlx::query_scalar!("SELECT currency_code FROM accounts WHERE id=?1", account_id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("account"))?;
    let currency = input
        .currency_code
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .unwrap_or(account_ccy);
    let as_of = input.as_of.to_string();
    let value_minor = input.value_minor.minor();
    let source = ValuationSource::Manual.as_str();
    sqlx::query_as!(
        ValuationRow,
        r#"INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         RETURNING id AS "id!", account_id, as_of, value_minor, currency_code, source, note,
                   created_at"#,
        account_id,
        as_of,
        value_minor,
        currency,
        source,
        input.note
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// Record (or refresh, if one already exists for this account today) a provider-sourced
/// balance snapshot. Unlike `create`, the currency is never defaulted — the caller must
/// pass whatever the upstream reports, since a provider-linked account can legitimately
/// hold a different currency than expected (e.g. a foreign-currency wallet).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert_from_provider(
    db: &Db,
    account_id: i64,
    as_of: &str,
    value_minor: i64,
    currency_code: &str,
) -> AppResult<Valuation> {
    // The partial unique index's predicate (`0010_provider_valuations.sql`) is a fixed
    // part of the schema and can't take a bound parameter, so it stays a literal — but
    // it must always match this bound value.
    let source = ValuationSource::Provider.as_str();
    sqlx::query_as!(
        ValuationRow,
        r#"INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id, as_of) WHERE source='provider' DO UPDATE SET
            value_minor = excluded.value_minor, currency_code = excluded.currency_code
         RETURNING id AS "id!", account_id, as_of, value_minor, currency_code, source, note,
                   created_at"#,
        account_id,
        as_of,
        value_minor,
        currency_code,
        source
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// Record (or refresh, if one already exists for this account today) a brokerage-computed
/// valuation (holdings × price + wallet cash — see `sure_api::brokerage`). Its own
/// `source='brokerage'` tag and partial unique index (see `0012_brokerage.sql`) keep it
/// from colliding with an unrelated `source='provider'` sync on the same account (e.g. a
/// still-attached Akahu balance-only link), so the historical backfill can upsert one row
/// per day in place.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn upsert_from_brokerage(
    db: &Db,
    account_id: i64,
    as_of: &str,
    value_minor: i64,
    currency_code: &str,
) -> AppResult<Valuation> {
    // The partial unique index's predicate (`0012_brokerage.sql`) is a fixed part of
    // the schema and can't take a bound parameter, so it stays a literal — but it must
    // always match this bound value.
    let source = ValuationSource::Brokerage.as_str();
    sqlx::query_as!(
        ValuationRow,
        r#"INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id, as_of) WHERE source='brokerage' DO UPDATE SET
            value_minor = excluded.value_minor, currency_code = excluded.currency_code
         RETURNING id AS "id!", account_id, as_of, value_minor, currency_code, source, note,
                   created_at"#,
        account_id,
        as_of,
        value_minor,
        currency_code,
        source
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// Delete a valuation.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM valuations WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("valuation"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{AccountKind, IsoDate, Money, SaveAccount};

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
                name: "Mortgage".to_string(),
                kind: AccountKind::Mortgage,
                currency_code: "NZD".to_string(),
                institution: None,
                // A mortgage's required metadata (see `AccountMetadata::validate_for`); none
                // of it matters to these tests, which are about the valuations themselves.
                metadata: Some(sure_core::AccountMetadata::Mortgage(
                    sure_core::MortgageMeta {
                        lender: Some("ASB".to_string()),
                        original_amount_minor: Some(48_500_000),
                        interest_rate_bps: Some(549),
                        rate_type: Some(sure_core::RateType::Fixed),
                        fixed_until: Some("2027-01-11".to_string()),
                        refix_rate_bps: Some(549),
                        refix_rate_uncertainty_bps: Some(150),
                        term_months: Some(360),
                        start_date: Some("2024-01-01".to_string()),
                        ..Default::default()
                    },
                )),
                archived: false,
                sort_order: 0,
                // Zero seeds no valuation, leaving the ones each test writes on their own.
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".to_string()),
                // These tests don't care who owns the account; joint needs no person row.
                ownership: sure_core::Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn upserts_a_same_day_provider_valuation_in_place() {
        let db = test_db().await;
        let account_id = test_account(&db).await;

        let first = upsert_from_provider(&db, account_id, "2026-07-18", -47_921_483, "NZD")
            .await
            .unwrap();
        assert_eq!(first.value_minor, -47_921_483);
        assert_eq!(first.source, ValuationSource::Provider);

        // A second sync the same day refreshes the same row rather than adding a new one.
        let second = upsert_from_provider(&db, account_id, "2026-07-18", -55_360_000, "NZD")
            .await
            .unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!(second.value_minor, -55_360_000);

        let all = list_for_account(&db, account_id).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn a_manual_valuation_the_same_day_does_not_collide_with_a_provider_one() {
        let db = test_db().await;
        let account_id = test_account(&db).await;

        create(
            &db,
            account_id,
            NewValuation {
                as_of: IsoDate::parse("2026-07-18").unwrap(),
                value_minor: Money::new(-1).unwrap(),
                currency_code: None,
                note: None,
            },
        )
        .await
        .unwrap();
        upsert_from_provider(&db, account_id, "2026-07-18", -47_921_483, "NZD")
            .await
            .unwrap();

        // Distinct rows: the partial unique index only constrains source='provider'.
        assert_eq!(list_for_account(&db, account_id).await.unwrap().len(), 2);
    }
}
