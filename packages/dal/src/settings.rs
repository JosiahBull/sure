use sqlx::FromRow;
use sure_core::{AppError, AppResult};
pub use sure_core::{Settings, UpdateSettings};

use crate::Db;

#[derive(Debug, FromRow)]
struct SettingsRow {
    base_currency_code: String,
    updated_at: String,
}

impl From<SettingsRow> for Settings {
    fn from(r: SettingsRow) -> Self {
        Settings {
            base_currency_code: r.base_currency_code,
            updated_at: r.updated_at,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db) -> AppResult<Settings> {
    Ok(sqlx::query_as::<_, SettingsRow>(
        "SELECT base_currency_code, updated_at FROM settings WHERE id = 1",
    )
    .fetch_one(db)
    .await?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, input: UpdateSettings) -> AppResult<Settings> {
    let code = input.base_currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &code).await? {
        return Err(AppError::validation(format!("unknown currency '{code}'")));
    }
    Ok(sqlx::query_as::<_, SettingsRow>(
        "UPDATE settings
         SET base_currency_code = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = 1
         RETURNING base_currency_code, updated_at",
    )
    .bind(&code)
    .fetch_one(db)
    .await?
    .into())
}

/// The configured base reporting currency code (used by the report queries).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn base_currency(db: &Db) -> AppResult<String> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT base_currency_code FROM settings WHERE id = 1")
            .fetch_one(db)
            .await?,
    )
}
