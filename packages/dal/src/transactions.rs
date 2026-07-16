use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite};
use sure_core::{AppError, AppResult};
use utoipa::{IntoParams, ToSchema};

use crate::Db;

#[derive(Serialize, FromRow, ToSchema, Clone)]
pub struct Transaction {
    pub id: i64,
    pub account_id: i64,
    pub posted_at: String,
    /// Signed minor units in `currency_code`; negative = outflow.
    pub amount_minor: i64,
    pub currency_code: String,
    pub description: String,
    /// Raw merchant text (e.g. from an import).
    pub merchant: Option<String>,
    /// Resolved custom merchant, if assigned.
    pub merchant_id: Option<i64>,
    pub notes: Option<String>,
    pub category_id: Option<i64>,
    /// Excluded from regular reports when true.
    pub is_one_off: bool,
    /// The other side of a transfer, if linked.
    pub linked_transaction_id: Option<i64>,
    pub provider: Option<String>,
    pub external_id: Option<String>,
    /// Which rule (if any) last set this transaction's category.
    pub categorized_by_rule_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveTransaction {
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    /// Defaults to the account's currency when omitted.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub merchant: Option<String>,
    #[serde(default)]
    pub merchant_id: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub is_one_off: bool,
}

#[derive(Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct TxQuery {
    pub account_id: Option<i64>,
    pub category_id: Option<i64>,
    /// Inclusive lower bound on the transaction date (ISO-8601).
    pub from: Option<String>,
    /// Inclusive upper bound on the transaction date (ISO-8601).
    pub to: Option<String>,
    /// When false, one-off transactions are excluded. Defaults to true.
    pub include_one_off: Option<bool>,
    /// Case-insensitive substring match on description/merchant/notes.
    pub search: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct LinkRequest {
    pub linked_transaction_id: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct TransferRequest {
    pub from_account_id: i64,
    pub to_account_id: i64,
    pub posted_at: String,
    /// Amount leaving the source account (positive minor units).
    pub from_amount_minor: i64,
    /// Amount arriving in the destination account; defaults to `from_amount_minor`
    /// (set explicitly for cross-currency transfers).
    #[serde(default)]
    pub to_amount_minor: Option<i64>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category_id: Option<i64>,
}

pub async fn list(db: &Db, q: TxQuery) -> AppResult<Vec<Transaction>> {
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT * FROM transactions WHERE 1=1");
    if let Some(account_id) = q.account_id {
        qb.push(" AND account_id = ").push_bind(account_id);
    }
    if let Some(category_id) = q.category_id {
        qb.push(" AND category_id = ").push_bind(category_id);
    }
    if let Some(from) = q.from.as_deref() {
        qb.push(" AND date(posted_at) >= date(").push_bind(from.to_string()).push(")");
    }
    if let Some(to) = q.to.as_deref() {
        qb.push(" AND date(posted_at) <= date(").push_bind(to.to_string()).push(")");
    }
    if !q.include_one_off.unwrap_or(true) {
        qb.push(" AND is_one_off = 0");
    }
    if let Some(search) = q.search.as_deref().filter(|s| !s.is_empty()) {
        let like = format!("%{}%", search.to_lowercase());
        qb.push(" AND (lower(description) LIKE ")
            .push_bind(like.clone())
            .push(" OR lower(coalesce(merchant,'')) LIKE ")
            .push_bind(like.clone())
            .push(" OR lower(coalesce(notes,'')) LIKE ")
            .push_bind(like)
            .push(")");
    }
    qb.push(" ORDER BY date(posted_at) DESC, id DESC");
    let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
    qb.push(" LIMIT ").push_bind(limit);
    qb.push(" OFFSET ").push_bind(q.offset.unwrap_or(0).max(0));

    Ok(qb.build_query_as::<Transaction>().fetch_all(db).await?)
}

pub async fn get(db: &Db, id: i64) -> AppResult<Transaction> {
    fetch(db, id).await
}

pub async fn create(db: &Db, input: SaveTransaction) -> AppResult<Transaction> {
    let currency = resolve_currency(db, &input).await?;
    validate_category(db, input.category_id).await?;
    sqlx::query_as::<_, Transaction>(
        "INSERT INTO transactions
            (account_id, posted_at, amount_minor, currency_code, description, merchant, notes,
             category_id, is_one_off, merchant_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) RETURNING *",
    )
    .bind(input.account_id)
    .bind(input.posted_at.trim())
    .bind(input.amount_minor)
    .bind(&currency)
    .bind(input.description.trim())
    .bind(&input.merchant)
    .bind(&input.notes)
    .bind(input.category_id)
    .bind(input.is_one_off)
    .bind(input.merchant_id)
    .fetch_one(db)
    .await
    .map_err(map_fk)
}

pub async fn update(db: &Db, id: i64, input: SaveTransaction) -> AppResult<Transaction> {
    let currency = resolve_currency(db, &input).await?;
    validate_category(db, input.category_id).await?;
    sqlx::query_as::<_, Transaction>(
        "UPDATE transactions SET account_id=?2, posted_at=?3, amount_minor=?4, currency_code=?5,
            description=?6, merchant=?7, notes=?8, category_id=?9, is_one_off=?10, merchant_id=?11,
            categorized_by_rule_id=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.account_id)
    .bind(input.posted_at.trim())
    .bind(input.amount_minor)
    .bind(&currency)
    .bind(input.description.trim())
    .bind(&input.merchant)
    .bind(&input.notes)
    .bind(input.category_id)
    .bind(input.is_one_off)
    .bind(input.merchant_id)
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("transaction"))
}

pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM transactions WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("transaction"));
    }
    Ok(())
}

pub async fn link(db: &Db, id: i64, req: LinkRequest) -> AppResult<Transaction> {
    let other = req.linked_transaction_id;
    if other == id {
        return Err(AppError::validation("a transaction cannot link to itself"));
    }
    let mut tx = db.begin().await?;
    for tid in [id, other] {
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM transactions WHERE id=?1")
            .bind(tid)
            .fetch_one(&mut *tx)
            .await?;
        if exists == 0 {
            return Err(AppError::NotFound("transaction"));
        }
    }
    sqlx::query("UPDATE transactions SET linked_transaction_id=?2 WHERE id=?1")
        .bind(id)
        .bind(other)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE transactions SET linked_transaction_id=?2 WHERE id=?1")
        .bind(other)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    fetch(db, id).await
}

pub async fn unlink(db: &Db, id: i64) -> AppResult<Transaction> {
    let current = fetch(db, id).await?;
    let mut tx = db.begin().await?;
    if let Some(other) = current.linked_transaction_id {
        sqlx::query("UPDATE transactions SET linked_transaction_id=NULL WHERE id=?1")
            .bind(other)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE transactions SET linked_transaction_id=NULL WHERE id=?1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    fetch(db, id).await
}

pub async fn create_transfer(db: &Db, req: TransferRequest) -> AppResult<Vec<Transaction>> {
    if req.from_account_id == req.to_account_id {
        return Err(AppError::validation(
            "transfer source and destination must differ",
        ));
    }
    let out_amount = req.from_amount_minor.abs();
    let in_amount = req.to_amount_minor.unwrap_or(out_amount).abs();
    let from_ccy = account_currency(db, req.from_account_id)
        .await?
        .ok_or(AppError::NotFound("account"))?;
    let to_ccy = account_currency(db, req.to_account_id)
        .await?
        .ok_or(AppError::NotFound("account"))?;
    validate_category(db, req.category_id).await?;

    let mut tx = db.begin().await?;
    let out = sqlx::query_as::<_, Transaction>(
        "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code, description, category_id)
         VALUES (?1,?2,?3,?4,?5,?6) RETURNING *",
    )
    .bind(req.from_account_id)
    .bind(req.posted_at.trim())
    .bind(-out_amount)
    .bind(&from_ccy)
    .bind(req.description.trim())
    .bind(req.category_id)
    .fetch_one(&mut *tx)
    .await?;
    let inflow = sqlx::query_as::<_, Transaction>(
        "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code, description, category_id, linked_transaction_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7) RETURNING *",
    )
    .bind(req.to_account_id)
    .bind(req.posted_at.trim())
    .bind(in_amount)
    .bind(&to_ccy)
    .bind(req.description.trim())
    .bind(req.category_id)
    .bind(out.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE transactions SET linked_transaction_id=?2 WHERE id=?1")
        .bind(out.id)
        .bind(inflow.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let out = fetch(db, out.id).await?;
    Ok(vec![out, inflow])
}

// ---- helpers -------------------------------------------------------------

async fn fetch(db: &Db, id: i64) -> AppResult<Transaction> {
    sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = ?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("transaction"))
}

async fn account_currency(db: &Db, account_id: i64) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id = ?1")
            .bind(account_id)
            .fetch_optional(db)
            .await?,
    )
}

async fn resolve_currency(db: &Db, input: &SaveTransaction) -> AppResult<String> {
    match input.currency_code.as_deref().filter(|s| !s.is_empty()) {
        Some(c) => Ok(c.trim().to_uppercase()),
        None => account_currency(db, input.account_id)
            .await?
            .ok_or(AppError::validation("account does not exist")),
    }
}

async fn validate_category(db: &Db, category_id: Option<i64>) -> AppResult<()> {
    if let Some(cid) = category_id {
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM categories WHERE id=?1")
            .bind(cid)
            .fetch_one(db)
            .await?;
        if exists == 0 {
            return Err(AppError::validation("category does not exist"));
        }
    }
    Ok(())
}

fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account or currency does not exist")
        }
        other => AppError::from(other),
    }
}
