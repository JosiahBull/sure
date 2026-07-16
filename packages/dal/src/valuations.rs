use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

/// A point-in-time value for an account (property price, share holding value, loan
/// balance, ...). Net-worth history is built from these plus cash-account flows.
#[derive(Serialize, FromRow, ToSchema)]
pub struct Valuation {
    pub id: i64,
    pub account_id: i64,
    pub as_of: String,
    /// Signed minor units in `currency_code`; liabilities are negative.
    pub value_minor: i64,
    pub currency_code: String,
    pub source: String,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct NewValuation {
    pub as_of: String,
    pub value_minor: i64,
    /// Defaults to the account's currency.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

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
