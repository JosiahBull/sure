use sqlx::FromRow;
use sure_core::{AppError, AppResult};
pub use sure_core::{Merchant, SaveMerchant};

use crate::Db;

#[derive(Debug, FromRow)]
struct MerchantRow {
    id: i64,
    name: String,
    category_id: Option<i64>,
    note: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<MerchantRow> for Merchant {
    fn from(r: MerchantRow) -> Self {
        Merchant {
            id: r.id,
            name: r.name,
            category_id: r.category_id,
            note: r.note,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Merchant>> {
    Ok(
        sqlx::query_as::<_, MerchantRow>("SELECT * FROM merchants ORDER BY name COLLATE NOCASE")
            .fetch_all(db)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    )
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveMerchant) -> AppResult<Merchant> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::validation("merchant name is required"));
    }
    Ok(sqlx::query_as::<_, MerchantRow>(
        "INSERT INTO merchants (name, category_id, note) VALUES (?1, ?2, ?3) RETURNING *",
    )
    .bind(name)
    .bind(input.category_id)
    .bind(&input.note)
    .fetch_one(db)
    .await
    .map_err(unique_or_fk)?
    .into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveMerchant) -> AppResult<Merchant> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::validation("merchant name is required"));
    }
    Ok(sqlx::query_as::<_, MerchantRow>(
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
    .ok_or(AppError::NotFound("merchant"))?
    .into())
}

/// Find an existing merchant by name (case-insensitive) or create one with the given
/// default category. Used by provider imports to reuse a source's own merchant
/// enrichment (e.g. Akahu's) without duplicating a merchant on every sync — an
/// already-known merchant's `category_id` is left untouched even if a later import
/// suggests a different one.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn find_or_create(db: &Db, name: &str, category_id: Option<i64>) -> AppResult<Merchant> {
    let name = name.trim();
    if let Some(existing) =
        sqlx::query_as::<_, MerchantRow>("SELECT * FROM merchants WHERE name = ?1 COLLATE NOCASE")
            .bind(name)
            .fetch_optional(db)
            .await?
    {
        return Ok(existing.into());
    }
    match sqlx::query_as::<_, MerchantRow>(
        "INSERT INTO merchants (name, category_id) VALUES (?1, ?2) RETURNING *",
    )
    .bind(name)
    .bind(category_id)
    .fetch_one(db)
    .await
    {
        Ok(m) => Ok(m.into()),
        // Lost a race with a concurrent import of the same merchant name — reuse theirs.
        Err(sqlx::Error::Database(ref e)) if e.is_unique_violation() => {
            sqlx::query_as::<_, MerchantRow>(
                "SELECT * FROM merchants WHERE name = ?1 COLLATE NOCASE",
            )
            .bind(name)
            .fetch_optional(db)
            .await?
            .map(Into::into)
            .ok_or(AppError::NotFound("merchant"))
        }
        Err(e) => Err(AppError::from(e)),
    }
}

#[tracing::instrument(level = "debug", skip_all)]
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
