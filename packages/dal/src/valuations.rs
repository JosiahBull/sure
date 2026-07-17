use sure_core::{AppError, AppResult};
pub use sure_core::{NewValuation, Valuation};

use crate::Db;

/// List an account's valuations, newest first.
pub async fn list_for_account(db: &Db, account_id: i64) -> AppResult<Vec<Valuation>> {
    Ok(sqlx::query_as::<_, Valuation>(
        "SELECT * FROM valuations WHERE account_id=?1 ORDER BY as_of DESC, id DESC",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?)
}

/// Record a valuation for an account, defaulting the currency to the account's.
pub async fn create(db: &Db, account_id: i64, input: NewValuation) -> AppResult<Valuation> {
    let account_ccy =
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
            .bind(account_id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound("account"))?;
    let currency = input
        .currency_code
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_uppercase())
        .unwrap_or(account_ccy);
    Ok(sqlx::query_as::<_, Valuation>(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1, ?2, ?3, ?4, 'manual', ?5) RETURNING *",
    )
    .bind(account_id)
    .bind(input.as_of.trim())
    .bind(input.value_minor)
    .bind(currency)
    .bind(&input.note)
    .fetch_one(db)
    .await?)
}

/// Record (or refresh, if one already exists for this account today) a provider-sourced
/// balance snapshot. Unlike `create`, the currency is never defaulted — the caller must
/// pass whatever the upstream reports, since a provider-linked account can legitimately
/// hold a different currency than expected (e.g. a foreign-currency wallet).
pub async fn upsert_from_provider(
    db: &Db,
    account_id: i64,
    as_of: &str,
    value_minor: i64,
    currency_code: &str,
) -> AppResult<Valuation> {
    Ok(sqlx::query_as::<_, Valuation>(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source)
         VALUES (?1, ?2, ?3, ?4, 'provider')
         ON CONFLICT(account_id, as_of) WHERE source='provider' DO UPDATE SET
            value_minor = excluded.value_minor, currency_code = excluded.currency_code
         RETURNING *",
    )
    .bind(account_id)
    .bind(as_of)
    .bind(value_minor)
    .bind(currency_code)
    .fetch_one(db)
    .await?)
}

/// Delete a valuation.
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM valuations WHERE id=?1")
        .bind(id)
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
    use sure_core::{AccountKind, SaveAccount};

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
                metadata: None,
                archived: false,
                sort_order: 0,
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
        assert_eq!(first.source, "provider");

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
                as_of: "2026-07-18".to_string(),
                value_minor: -1,
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
