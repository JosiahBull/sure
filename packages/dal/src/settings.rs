use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

#[derive(Serialize, FromRow, ToSchema)]
pub struct Settings {
    /// Currency all reports are normalised into.
    pub base_currency_code: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateSettings {
    pub base_currency_code: String,
}

pub async fn get(db: &Db) -> AppResult<Settings> {
    Ok(sqlx::query_as::<_, Settings>(
        "SELECT base_currency_code, updated_at FROM settings WHERE id = 1",
    )
    .fetch_one(db)
    .await?)
}

pub async fn update(db: &Db, input: UpdateSettings) -> AppResult<Settings> {
    let code = input.base_currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &code).await? {
        return Err(AppError::validation(format!("unknown currency '{code}'")));
    }
    Ok(sqlx::query_as::<_, Settings>(
        "UPDATE settings
         SET base_currency_code = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = 1
         RETURNING base_currency_code, updated_at",
    )
    .bind(&code)
    .fetch_one(db)
    .await?)
}

/// The configured base reporting currency code (used by the report queries).
pub async fn base_currency(db: &Db) -> AppResult<String> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT base_currency_code FROM settings WHERE id = 1")
            .fetch_one(db)
            .await?,
    )
}
