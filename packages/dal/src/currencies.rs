use sure_core::{AppError, AppResult};
pub use sure_core::{Currency, NewCurrency};

use crate::Db;

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Currency>> {
    Ok(sqlx::query_as::<_, Currency>("SELECT * FROM currencies ORDER BY code")
        .fetch_all(db)
        .await?)
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
pub async fn exists(db: &Db, code: &str) -> AppResult<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM currencies WHERE code = ?1")
            .bind(code.trim().to_uppercase())
            .fetch_one(db)
            .await?
            > 0,
    )
}
