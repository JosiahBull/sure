use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

#[derive(Serialize, FromRow, ToSchema)]
pub struct Currency {
    /// ISO 4217 code (or a user code for private assets), e.g. `NZD`.
    pub code: String,
    pub name: String,
    pub symbol: String,
    /// Number of minor units per major unit (2 => cents).
    pub decimal_places: i64,
    pub created_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct NewCurrency {
    pub code: String,
    pub name: String,
    pub symbol: String,
    #[serde(default = "default_decimals")]
    pub decimal_places: i64,
}

fn default_decimals() -> i64 {
    2
}

pub async fn list(db: &Db) -> AppResult<Vec<Currency>> {
    Ok(sqlx::query_as::<_, Currency>("SELECT * FROM currencies ORDER BY code")
        .fetch_all(db)
        .await?)
}

pub async fn upsert(db: &Db, input: NewCurrency) -> AppResult<Currency> {
    let code = input.code.trim().to_uppercase();
    if code.is_empty() {
        return Err(AppError::validation("currency code is required"));
    }
    Ok(sqlx::query_as::<_, Currency>(
        "INSERT INTO currencies (code, name, symbol, decimal_places)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(code) DO UPDATE SET
            name = excluded.name, symbol = excluded.symbol, decimal_places = excluded.decimal_places
         RETURNING *",
    )
    .bind(&code)
    .bind(input.name.trim())
    .bind(input.symbol.trim())
    .bind(input.decimal_places)
    .fetch_one(db)
    .await?)
}

pub async fn delete(db: &Db, code: &str) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM currencies WHERE code = ?1")
        .bind(code.trim().to_uppercase())
        .execute(db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref d) if d.is_foreign_key_violation() => {
                AppError::conflict("currency is in use and cannot be deleted")
            }
            other => AppError::from(other),
        })?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("currency"));
    }
    Ok(())
}

pub async fn exists(db: &Db, code: &str) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM currencies WHERE code = ?1")
            .bind(code.trim().to_uppercase())
            .fetch_one(db)
            .await?
            > 0,
    )
}
