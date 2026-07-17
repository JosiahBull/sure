use serde_json::json;
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
pub use sure_core::{Provider, ProviderSync, SaveProvider, SyncRequest};

use crate::Db;

#[derive(FromRow)]
pub struct ProviderRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub account_id: i64,
    pub config: String,
    pub enabled: bool,
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<ProviderRow> for Provider {
    fn from(r: ProviderRow) -> Self {
        Provider {
            config: serde_json::from_str(&r.config).unwrap_or_else(|_| json!({})),
            id: r.id,
            name: r.name,
            kind: r.kind,
            account_id: r.account_id,
            enabled: r.enabled,
            last_synced_at: r.last_synced_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// A normalised transaction handed from a provider to be imported (dedupe on external id).
pub struct ImportRow {
    pub external_id: String,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    pub description: String,
    pub merchant: Option<String>,
}

pub async fn list(db: &Db) -> AppResult<Vec<Provider>> {
    let rows = sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers ORDER BY id")
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(Provider::from).collect())
}

pub async fn get(db: &Db, id: i64) -> AppResult<Provider> {
    let row = sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("provider"))?;
    Ok(row.into())
}

pub async fn create(db: &Db, input: SaveProvider) -> AppResult<Provider> {
    let config = input.config.clone().unwrap_or_else(|| json!({}));
    let row = sqlx::query_as::<_, ProviderRow>(
        "INSERT INTO providers (name, kind, account_id, config, enabled)
         VALUES (?1,?2,?3,?4,?5) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(&input.kind)
    .bind(input.account_id)
    .bind(config.to_string())
    .bind(input.enabled)
    .fetch_one(db)
    .await
    .map_err(map_fk)?;
    Ok(row.into())
}

pub async fn update(db: &Db, id: i64, input: SaveProvider) -> AppResult<Provider> {
    let config = input.config.clone().unwrap_or_else(|| json!({}));
    let row = sqlx::query_as::<_, ProviderRow>(
        "UPDATE providers SET name=?2, kind=?3, account_id=?4, config=?5, enabled=?6,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(&input.kind)
    .bind(input.account_id)
    .bind(config.to_string())
    .bind(input.enabled)
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("provider"))?;
    Ok(row.into())
}

pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM providers WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("provider"));
    }
    Ok(())
}

pub async fn account_currency(db: &Db, account_id: i64) -> AppResult<String> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
            .bind(account_id)
            .fetch_one(db)
            .await?,
    )
}

/// Insert fetched transactions, deduping on (provider, external_id). Returns
/// (imported, skipped).
pub async fn import_transactions(
    db: &Db,
    account_id: i64,
    account_currency: &str,
    provider_tag: &str,
    rows: &[ImportRow],
) -> AppResult<(i64, i64)> {
    let mut imported = 0i64;
    let mut skipped = 0i64;
    for t in rows {
        let ccy = t
            .currency_code
            .clone()
            .unwrap_or_else(|| account_currency.to_string());
        let res = sqlx::query(
            "INSERT OR IGNORE INTO transactions
                (account_id, posted_at, amount_minor, currency_code, description, merchant, provider, external_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )
        .bind(account_id)
        .bind(&t.posted_at)
        .bind(t.amount_minor)
        .bind(&ccy)
        .bind(&t.description)
        .bind(&t.merchant)
        .bind(provider_tag)
        .bind(&t.external_id)
        .execute(db)
        .await?;
        if res.rows_affected() > 0 {
            imported += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((imported, skipped))
}

pub async fn update_last_synced(db: &Db, id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE providers SET last_synced_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn record_sync(
    db: &Db,
    provider_id: i64,
    imported: i64,
    skipped: i64,
    status: &str,
    detail: Option<&str>,
) -> AppResult<ProviderSync> {
    Ok(sqlx::query_as::<_, ProviderSync>(
        "INSERT INTO provider_syncs (provider_id, imported, skipped, status, detail)
         VALUES (?1,?2,?3,?4,?5) RETURNING *",
    )
    .bind(provider_id)
    .bind(imported)
    .bind(skipped)
    .bind(status)
    .bind(detail)
    .fetch_one(db)
    .await?)
}

pub async fn list_syncs(db: &Db, provider_id: i64) -> AppResult<Vec<ProviderSync>> {
    Ok(sqlx::query_as::<_, ProviderSync>(
        "SELECT * FROM provider_syncs WHERE provider_id=?1 ORDER BY id DESC",
    )
    .bind(provider_id)
    .fetch_all(db)
    .await?)
}

fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account does not exist")
        }
        other => AppError::from(other),
    }
}
