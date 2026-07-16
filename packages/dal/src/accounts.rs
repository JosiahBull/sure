use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use sure_core::{AccountClass, AccountKind, AppError, AppResult};
use utoipa::ToSchema;

use crate::Db;

#[derive(FromRow)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
    pub institution: Option<String>,
    pub metadata: String,
    pub archived: bool,
    pub sort_order: i64,
    pub secured_by_account_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    /// Derived grouping (cash / asset / investment / liability).
    pub class: AccountClass,
    pub currency_code: String,
    pub institution: Option<String>,
    /// Kind-specific configuration as a JSON object.
    pub metadata: Value,
    pub archived: bool,
    pub sort_order: i64,
    /// For a liability, the asset account it is secured against (e.g. a mortgage's home).
    pub secured_by_account_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<AccountRow> for Account {
    fn from(r: AccountRow) -> Self {
        Account {
            class: r.kind.class(),
            metadata: serde_json::from_str(&r.metadata).unwrap_or_else(|_| json!({})),
            id: r.id,
            name: r.name,
            kind: r.kind,
            currency_code: r.currency_code,
            institution: r.institution,
            archived: r.archived,
            sort_order: r.sort_order,
            secured_by_account_id: r.secured_by_account_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct SaveAccount {
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
    #[serde(default)]
    pub institution: Option<String>,
    /// Kind-specific JSON config; defaults to `{}`.
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub include_archived: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct SetSecuredBy {
    /// The asset account this (liability) account is secured against; `null` to unlink.
    pub secured_by_account_id: Option<i64>,
}

pub async fn list(db: &Db, include_archived: bool) -> AppResult<Vec<Account>> {
    let sql = if include_archived {
        "SELECT * FROM accounts ORDER BY sort_order, name"
    } else {
        "SELECT * FROM accounts WHERE archived = 0 ORDER BY sort_order, name"
    };
    let rows = sqlx::query_as::<_, AccountRow>(sql).fetch_all(db).await?;
    Ok(rows.into_iter().map(Account::from).collect())
}

pub async fn get(db: &Db, id: i64) -> AppResult<Account> {
    let row = sqlx::query_as::<_, AccountRow>("SELECT * FROM accounts WHERE id = ?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("account"))?;
    Ok(row.into())
}

/// Validate input and return the metadata JSON to persist (as a string).
async fn validate(db: &Db, input: &SaveAccount) -> AppResult<String> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("account name is required"));
    }
    let currency = input.currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &currency).await? {
        return Err(AppError::validation(format!("unknown currency '{currency}'")));
    }
    let metadata = input.metadata.clone().unwrap_or_else(|| json!({}));
    if !metadata.is_object() {
        return Err(AppError::validation("metadata must be a JSON object"));
    }
    Ok(metadata.to_string())
}

pub async fn create(db: &Db, input: SaveAccount) -> AppResult<Account> {
    let metadata = validate(db, &input).await?;
    let row = sqlx::query_as::<_, AccountRow>(
        "INSERT INTO accounts (name, kind, currency_code, institution, metadata, archived, sort_order)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(input.kind)
    .bind(input.currency_code.trim().to_uppercase())
    .bind(&input.institution)
    .bind(metadata)
    .bind(input.archived)
    .bind(input.sort_order)
    .fetch_one(db)
    .await?;
    Ok(row.into())
}

pub async fn update(db: &Db, id: i64, input: SaveAccount) -> AppResult<Account> {
    let metadata = validate(db, &input).await?;
    let row = sqlx::query_as::<_, AccountRow>(
        "UPDATE accounts SET name=?2, kind=?3, currency_code=?4, institution=?5, metadata=?6,
            archived=?7, sort_order=?8, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(input.kind)
    .bind(input.currency_code.trim().to_uppercase())
    .bind(&input.institution)
    .bind(metadata)
    .bind(input.archived)
    .bind(input.sort_order)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("account"))?;
    Ok(row.into())
}

pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM accounts WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("account"));
    }
    Ok(())
}

pub async fn set_secured_by(db: &Db, id: i64, target: Option<i64>) -> AppResult<Account> {
    if let Some(t) = target {
        if t == id {
            return Err(AppError::validation("an account cannot secure itself"));
        }
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts WHERE id=?1")
            .bind(t)
            .fetch_one(db)
            .await?;
        if exists == 0 {
            return Err(AppError::validation("securing account does not exist"));
        }
    }
    let row = sqlx::query_as::<_, AccountRow>(
        "UPDATE accounts SET secured_by_account_id=?2,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(target)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("account"))?;
    Ok(row.into())
}
