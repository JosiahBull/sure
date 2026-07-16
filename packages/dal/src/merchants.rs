use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

/// A reusable payee. Custom merchants are unique by name (case-insensitive) and can
/// carry a suggested default category.
#[derive(Serialize, FromRow, ToSchema)]
pub struct Merchant {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveMerchant {
    pub name: String,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
}

pub async fn list(db: &Db) -> AppResult<Vec<Merchant>> {
    Ok(
        sqlx::query_as::<_, Merchant>("SELECT * FROM merchants ORDER BY name COLLATE NOCASE")
            .fetch_all(db)
            .await?,
    )
}

pub async fn create(db: &Db, input: SaveMerchant) -> AppResult<Merchant> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::validation("merchant name is required"));
    }
    sqlx::query_as::<_, Merchant>(
        "INSERT INTO merchants (name, category_id, note) VALUES (?1, ?2, ?3) RETURNING *",
    )
    .bind(name)
    .bind(input.category_id)
    .bind(&input.note)
    .fetch_one(db)
    .await
    .map_err(unique_or_fk)
}

pub async fn update(db: &Db, id: i64, input: SaveMerchant) -> AppResult<Merchant> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::validation("merchant name is required"));
    }
    sqlx::query_as::<_, Merchant>(
        "UPDATE merchants SET name=?2, category_id=?3, note=?4,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(name)
    .bind(input.category_id)
    .bind(&input.note)
    .fetch_optional(db)
    .await
    .map_err(unique_or_fk)?
    .ok_or(AppError::NotFound("merchant"))
}

pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM merchants WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("merchant"));
    }
    Ok(())
}

fn unique_or_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_unique_violation() => {
            AppError::conflict("a merchant with that name already exists")
        }
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("category does not exist")
        }
        other => AppError::from(other),
    }
}
