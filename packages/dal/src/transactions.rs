use sqlx::{AssertSqlSafe, FromRow, QueryBuilder, Sqlite};
use sure_core::transactions::MAX_BULK_IDS;
use sure_core::{AppError, AppResult, Ownership};
pub use sure_core::{
    BulkDelete, BulkResult, BulkUpdate, LinkRequest, SaveTransaction, Transaction, TransferRequest,
    TxQuery,
};

use crate::Db;

// The one row struct that still needs `FromRow`: `list`'s filters are decided at runtime, so it
// goes through `QueryBuilder::build_query_as` rather than `query_as!`. Every other row struct in
// this crate is built field-by-field by the checked macros and needs no runtime mapping.
#[derive(Debug, FromRow)]
pub(crate) struct TransactionRow {
    pub(crate) id: i64,
    pub(crate) account_id: i64,
    pub(crate) posted_at: String,
    pub(crate) amount_minor: i64,
    pub(crate) currency_code: String,
    pub(crate) description: String,
    pub(crate) merchant: Option<String>,
    pub(crate) merchant_id: Option<i64>,
    pub(crate) notes: Option<String>,
    pub(crate) category_id: Option<i64>,
    pub(crate) is_one_off: bool,
    pub(crate) linked_transaction_id: Option<i64>,
    pub(crate) provider: Option<String>,
    pub(crate) external_id: Option<String>,
    pub(crate) categorized_by_rule_id: Option<i64>,
    pub(crate) ownership: Option<String>,
    pub(crate) person_id: Option<i64>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

impl TryFrom<TransactionRow> for Transaction {
    type Error = AppError;

    fn try_from(r: TransactionRow) -> AppResult<Self> {
        // `None` is a legitimate value here (inherit from the account), so only a *present*
        // discriminant is parsed — and then strictly, exactly like the accounts row does:
        // the pair is only ever written together, so a half-written one means the row came
        // from something that went around every writer we own.
        let ownership = match (r.ownership.as_deref(), r.person_id) {
            (None, None) => None,
            (None, Some(_)) => {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "transaction has a person_id but no ownership discriminant"
                )));
            }
            (Some(kind), person_id) => Some(
                Ownership::from_stored(kind, person_id)
                    .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?,
            ),
        };
        Ok(Transaction {
            ownership,
            id: r.id,
            account_id: r.account_id,
            posted_at: r.posted_at,
            amount_minor: r.amount_minor,
            currency_code: r.currency_code,
            description: r.description,
            merchant: r.merchant,
            merchant_id: r.merchant_id,
            notes: r.notes,
            category_id: r.category_id,
            is_one_off: r.is_one_off,
            linked_transaction_id: r.linked_transaction_id,
            provider: r.provider,
            external_id: r.external_id,
            categorized_by_rule_id: r.categorized_by_rule_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
    }
}

/// Split an attribution override into the two columns to bind, validating the person exists
/// so a stale id is a 422 naming it rather than an opaque FK violation.
///
/// `None` (no override) is the common case and stores as two NULLs — the transaction follows
/// its account, for good, including when that account is later re-attributed.
async fn split_ownership(
    db: &Db,
    ownership: Option<Ownership>,
) -> AppResult<(Option<&'static str>, Option<i64>)> {
    let Some(ownership) = ownership else {
        return Ok((None, None));
    };
    if let Ownership::Person { person_id } = ownership {
        if !crate::people::exists(db, person_id).await? {
            return Err(AppError::validation(format!(
                "person {person_id} does not exist"
            )));
        }
    }
    let (kind, person_id) = ownership.as_parts();
    Ok((Some(kind), person_id))
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db, q: TxQuery) -> AppResult<Vec<Transaction>> {
    // `t.*`, not `*`: the join below would otherwise splice the account's own `ownership`
    // and `person_id` columns into the row and shadow the transaction's.
    // Not `sqlx::query_as!`: which filters are present decides the SQL text, and the macro
    // needs a literal. One of four such sites in this module (with `bulk_update`,
    // `bulk_delete` and `amounts_for_matching`); everything else here is compile-time checked.
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
        "SELECT t.* FROM transactions t JOIN accounts a ON a.id = t.account_id WHERE 1=1",
    );
    if let Some(account_id) = q.account_id {
        qb.push(" AND t.account_id = ").push_bind(account_id);
    }
    if let Some(category_id) = q.category_id {
        qb.push(" AND t.category_id = ").push_bind(category_id);
    }
    if let Some(from) = q.from.as_deref() {
        qb.push(" AND date(t.posted_at) >= date(")
            .push_bind(from.to_string())
            .push(")");
    }
    if let Some(to) = q.to.as_deref() {
        qb.push(" AND date(t.posted_at) <= date(")
            .push_bind(to.to_string())
            .push(")");
    }
    if !q.include_one_off.unwrap_or(true) {
        qb.push(" AND t.is_one_off = 0");
    }
    if let Some(uncategorized) = q.uncategorized {
        qb.push(if uncategorized {
            " AND t.category_id IS NULL"
        } else {
            " AND t.category_id IS NOT NULL"
        });
    }
    if let Some(search) = q.search.as_deref().filter(|s| !s.is_empty()) {
        let like = format!("%{}%", search.to_lowercase());
        qb.push(" AND (lower(t.description) LIKE ")
            .push_bind(like.clone())
            .push(" OR lower(coalesce(t.merchant,'')) LIKE ")
            .push_bind(like.clone())
            .push(" OR lower(coalesce(t.notes,'')) LIKE ")
            .push_bind(like)
            .push(")");
    }
    // Effective attribution: the transaction's own override, or — when it has none — its
    // account's owner. Written as two OR'd equality branches rather than a CASE so SQLite
    // can still use `idx_tx_person` / `idx_accounts_person` for the selective half.
    if let Some(attributed_to) = q.attributed_to {
        match attributed_to {
            Ownership::Person { person_id } => {
                qb.push(" AND ((t.ownership = 'person' AND t.person_id = ")
                    .push_bind(person_id)
                    .push(") OR (t.ownership IS NULL AND a.ownership = 'person' AND a.person_id = ")
                    .push_bind(person_id)
                    .push("))");
            }
            Ownership::Joint => {
                qb.push(
                    " AND (t.ownership = 'joint'                      OR (t.ownership IS NULL AND a.ownership = 'joint'))",
                );
            }
        }
    }
    qb.push(" ORDER BY date(t.posted_at) DESC, t.id DESC");
    let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
    qb.push(" LIMIT ").push_bind(limit);
    qb.push(" OFFSET ").push_bind(q.offset.unwrap_or(0).max(0));

    qb.build_query_as::<TransactionRow>()
        .fetch_all(db)
        .await?
        .into_iter()
        .map(Transaction::try_from)
        .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<Transaction> {
    fetch(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveTransaction) -> AppResult<Transaction> {
    let currency = resolve_currency(db, &input).await?;
    validate_category(db, input.category_id).await?;
    let (ownership, person_id) = split_ownership(db, input.ownership).await?;
    let posted_at = input.posted_at.to_string();
    let amount_minor = input.amount_minor.minor();
    let description = input.description.trim();
    sqlx::query_as!(
        TransactionRow,
        r#"INSERT INTO transactions
              (account_id, posted_at, amount_minor, currency_code, description, merchant, notes,
               category_id, is_one_off, merchant_id, ownership, person_id)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
           RETURNING id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                     merchant, merchant_id, notes, category_id, is_one_off AS "is_one_off!: bool",
                     linked_transaction_id, provider, external_id, categorized_by_rule_id,
                     ownership, person_id, created_at, updated_at"#,
        input.account_id,
        posted_at,
        amount_minor,
        currency,
        description,
        input.merchant,
        input.notes,
        input.category_id,
        input.is_one_off,
        input.merchant_id,
        ownership,
        person_id
    )
    .fetch_one(db)
    .await
    .map_err(map_fk)?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveTransaction) -> AppResult<Transaction> {
    let currency = resolve_currency(db, &input).await?;
    validate_category(db, input.category_id).await?;
    let (ownership, person_id) = split_ownership(db, input.ownership).await?;
    let posted_at = input.posted_at.to_string();
    let amount_minor = input.amount_minor.minor();
    let description = input.description.trim();
    sqlx::query_as!(
        TransactionRow,
        r#"UPDATE transactions SET account_id=?2, posted_at=?3, amount_minor=?4,
              currency_code=?5, description=?6, merchant=?7, notes=?8, category_id=?9,
              is_one_off=?10, merchant_id=?11, ownership=?12, person_id=?13,
              categorized_by_rule_id=NULL, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id=?1
           RETURNING id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                     merchant, merchant_id, notes, category_id, is_one_off AS "is_one_off!: bool",
                     linked_transaction_id, provider, external_id, categorized_by_rule_id,
                     ownership, person_id, created_at, updated_at"#,
        id,
        input.account_id,
        posted_at,
        amount_minor,
        currency,
        description,
        input.merchant,
        input.notes,
        input.category_id,
        input.is_one_off,
        input.merchant_id,
        ownership,
        person_id
    )
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("transaction"))?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM transactions WHERE id = ?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("transaction"));
    }
    Ok(())
}

/// Apply a partial patch to every transaction in `ids`. Returns the number of rows
/// actually changed. A no-op (no fields to set) short-circuits to 0.
///
/// The id list can be neither empty nor unbounded: [`BulkIds`](sure_core::transactions::BulkIds)
/// refuses both as it is parsed, so the `IN (…)` list below is guaranteed to stay under
/// SQLite's bind-variable ceiling — one bind per id, and a statement past
/// `SQLITE_MAX_VARIABLE_NUMBER` fails to *prepare*.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn bulk_update(db: &Db, input: BulkUpdate) -> AppResult<i64> {
    let BulkUpdate {
        ids,
        category_id,
        merchant_id,
        is_one_off,
        ownership,
    } = input;
    if category_id.is_none() && merchant_id.is_none() && is_one_off.is_none() && ownership.is_none()
    {
        return Ok(0);
    }
    // Validate a to-be-set category once up front (a cleared/absent one needs no check).
    if let Some(Some(cid)) = category_id {
        validate_category(db, Some(cid)).await?;
    }
    // Same for an attribution: check the person exists once, not once per row.
    let ownership_columns = match ownership {
        Some(o) => Some(split_ownership(db, o).await?),
        None => None,
    };

    // Runtime-shaped, so not macro-checkable: the `SET` list is whichever fields the patch
    // carries, and the `IN (…)` list is one placeholder per id.
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("UPDATE transactions SET ");
    {
        let mut set = qb.separated(", ");
        if let Some(cid) = category_id {
            set.push("category_id = ");
            set.push_bind_unseparated(cid);
            // Manually reassigning the category clears the "categorised by rule" marker so a
            // later rule re-run won't clobber the manual choice — same as the single update.
            set.push("categorized_by_rule_id = NULL");
        }
        if let Some(mid) = merchant_id {
            set.push("merchant_id = ");
            set.push_bind_unseparated(mid);
        }
        if let Some(one_off) = is_one_off {
            set.push("is_one_off = ");
            set.push_bind_unseparated(one_off);
        }
        // Both columns always move together — a discriminant without its person (or the
        // reverse) is refused by the table's trigger, and rightly so.
        if let Some((kind, person_id)) = ownership_columns {
            set.push("ownership = ");
            set.push_bind_unseparated(kind);
            set.push("person_id = ");
            set.push_bind_unseparated(person_id);
        }
        set.push("updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')");
    }
    qb.push(" WHERE id IN (");
    {
        let mut list = qb.separated(", ");
        for id in ids.iter() {
            list.push_bind(*id);
        }
    }
    qb.push(")");

    let res = qb.build().execute(db).await.map_err(map_fk)?;
    Ok(res.rows_affected() as i64)
}

/// Delete every transaction in `ids`. The `linked_transaction_id` FK is `ON DELETE SET
/// NULL`, so the other side of any transfer is unlinked automatically. Returns the
/// number of rows deleted.
///
/// Takes a bare slice — that is the repository port's signature — so unlike [`bulk_update`]
/// it cannot lean on [`BulkIds`](sure_core::transactions::BulkIds) for the bound and re-checks
/// it here. Without the check a longer list fails at *prepare* time, and that error is
/// `AppError::Database` → a scrubbed 500 the caller can do nothing with; with it, every caller
/// that assembles a slice by hand still gets a 422 naming the limit. An empty slice stays a
/// harmless 0 for in-process callers (an HTTP body carrying one is already refused).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn bulk_delete(db: &Db, ids: &[i64]) -> AppResult<i64> {
    if ids.is_empty() {
        return Ok(0);
    }
    if ids.len() > MAX_BULK_IDS {
        return Err(AppError::validation(format!(
            "too many ids: {} (maximum {MAX_BULK_IDS} per bulk request — split the selection into smaller batches)",
            ids.len()
        )));
    }
    // Runtime-shaped, so not macro-checkable: one placeholder per id in the list.
    let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("DELETE FROM transactions WHERE id IN (");
    {
        let mut list = qb.separated(", ");
        for id in ids {
            list.push_bind(*id);
        }
    }
    qb.push(")");
    let res = qb.build().execute(db).await?;
    Ok(res.rows_affected() as i64)
}

/// The earliest `posted_at` on this account that a *different* feed already owns.
///
/// The cutover for a manual historical import: from this date on, some connected provider
/// is already posting this account's movements, and importing a file's rows over the top
/// would count the same money twice — dedupe is `(provider, external_id)`, so two sources
/// describing one transaction look like two transactions.
///
/// Two deliberate exclusions. `provider IS NULL` skips hand-entered rows and the seeded
/// `'Opening balance'`: those are sparse, so one of them must not be read as coverage of
/// everything after it. And `exclude_provider` skips the importer's own previous rows,
/// without which a second upload would see its own history and hold back the lot.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn earliest_posted_at_from_other_feed(
    db: &Db,
    account_id: i64,
    exclude_provider: &str,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT MIN(posted_at) AS "earliest: String" FROM transactions
            WHERE account_id = ?1 AND provider IS NOT NULL AND provider <> ?2"#,
        account_id,
        exclude_provider
    )
    .fetch_one(db)
    .await?)
}

/// Every amount on this account, summed. Against the account's recorded balance this says
/// whether its ledger actually reconciles — the check that catches a historical import whose
/// window disagrees with what a live feed already posted for the same period.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn sum_amount_minor(db: &Db, account_id: i64) -> AppResult<i64> {
    Ok(sqlx::query_scalar!(
        r#"SELECT COALESCE(SUM(amount_minor), 0) AS "total!: i64"
             FROM transactions WHERE account_id = ?1"#,
        account_id
    )
    .fetch_one(db)
    .await?)
}

/// The earliest `posted_at` on this account, whatever wrote it. Unlike
/// [`earliest_posted_at_from_other_feed`] this counts manual rows and the importer's own,
/// because the question it answers is "is there any ledger here already?" rather than "who
/// owns this window?".
#[tracing::instrument(level = "debug", skip_all)]
pub async fn earliest_posted_at(db: &Db, account_id: i64) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT MIN(posted_at) AS "earliest: String"
             FROM transactions WHERE account_id = ?1"#,
        account_id
    )
    .fetch_one(db)
    .await?)
}

/// One `external_id` per account, over the rows whose provider tag starts with
/// `provider_prefix`.
///
/// Lets a manual importer recover which *upstream* account it last imported into which local
/// account, from the ids it wrote — the ids are the only durable record of that mapping, and
/// re-deriving it is what lets a repeat upload of the same bank export route itself. One
/// sample is enough because a tag only ever covers one upstream account.
///
/// `provider_prefix` is matched with `LIKE`, so it must not contain `%` or `_`; every caller
/// passes a fixed tag stem like `asb#`.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn sample_external_ids(db: &Db, provider_prefix: &str) -> AppResult<Vec<(i64, String)>> {
    Ok(sqlx::query!(
        // `external_id IS NOT NULL` is what makes the MIN non-null for every group the
        // `GROUP BY` produces, which SQLite's describe cannot see on its own.
        r#"SELECT account_id AS "account_id!", MIN(external_id) AS "external_id!: String"
             FROM transactions
            WHERE provider LIKE ?1 || '%' AND external_id IS NOT NULL
            GROUP BY account_id"#,
        provider_prefix
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|r| (r.account_id, r.external_id))
    .collect())
}

/// `(account_id, date, amount_minor)` for these accounts, for matching an uploaded bank export
/// to the account it belongs to. Dates are the first ten characters of `posted_at`, which is
/// what the comparison works in — a day's tolerance either side, because a feed and the bank's
/// own export routinely disagree by one about when a transaction landed.
///
/// The id list is interpolated rather than bound: sqlx has no array binding for SQLite, and
/// these are `i64`s the caller just read out of this same database, so there is no string to
/// escape. `limit` bounds the whole result, oldest first.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn amounts_for_matching(
    db: &Db,
    account_ids: &[i64],
    limit: i64,
) -> AppResult<Vec<(i64, String, i64)>> {
    if account_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = account_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // Runtime-shaped, so not macro-checkable: the `IN (…)` list is built above. `AssertSqlSafe`
    // is sqlx 0.9's audit marker for exactly that — `ids` is `i64::to_string` output and
    // nothing else, so it cannot carry a quote, a comment, or a statement separator.
    let sql = AssertSqlSafe(format!(
        "SELECT account_id, substr(posted_at, 1, 10), amount_minor FROM transactions
         WHERE account_id IN ({ids})
         ORDER BY posted_at
         LIMIT ?1"
    ));
    Ok(sqlx::query_as::<_, (i64, String, i64)>(sql)
        .bind(limit)
        .fetch_all(db)
        .await?)
}

/// Delete every transaction on this account that `provider_tag` imported — undo for a bulk
/// upload. Scoped to the account as well as the tag so a mistyped tag can't reach further
/// than the account it was invoked for. Returns the number of rows deleted.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete_by_provider(db: &Db, account_id: i64, provider_tag: &str) -> AppResult<i64> {
    let res = sqlx::query!(
        "DELETE FROM transactions WHERE account_id = ?1 AND provider = ?2",
        account_id,
        provider_tag
    )
    .execute(db)
    .await?;
    Ok(res.rows_affected() as i64)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn link(db: &Db, id: i64, req: LinkRequest) -> AppResult<Transaction> {
    let other = req.linked_transaction_id;
    if other == id {
        return Err(AppError::validation("a transaction cannot link to itself"));
    }
    let mut tx = db.begin().await?;
    for tid in [id, other] {
        let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM transactions WHERE id=?1", tid)
            .fetch_one(&mut *tx)
            .await?;
        if exists == 0 {
            return Err(AppError::NotFound("transaction"));
        }
    }
    sqlx::query!(
        "UPDATE transactions SET linked_transaction_id=?2 WHERE id=?1",
        id,
        other
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE transactions SET linked_transaction_id=?2 WHERE id=?1",
        other,
        id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    fetch(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn unlink(db: &Db, id: i64) -> AppResult<Transaction> {
    let current = fetch(db, id).await?;
    let mut tx = db.begin().await?;
    if let Some(other) = current.linked_transaction_id {
        sqlx::query!(
            "UPDATE transactions SET linked_transaction_id=NULL WHERE id=?1",
            other
        )
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query!(
        "UPDATE transactions SET linked_transaction_id=NULL WHERE id=?1",
        id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    fetch(db, id).await
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_transfer(db: &Db, req: TransferRequest) -> AppResult<Vec<Transaction>> {
    if req.from_account_id == req.to_account_id {
        return Err(AppError::validation(
            "transfer source and destination must differ",
        ));
    }
    // Both `.abs()` calls are `Money::abs`, which is total. On the raw `i64` these fields used
    // to be, `i64::MIN.abs()` panicked in debug and yielded `i64::MIN` in release — and the
    // outflow bind below negates the result, so release turned a transfer of `i64::MIN` into
    // two wrapped, wrong legs with a 201 on top. `Money` cannot hold that value at all.
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
    let posted_at = req.posted_at.to_string();
    let description = req.description.trim();
    let outflow_minor = out_amount.neg().minor();
    let inflow_minor = in_amount.minor();
    let out: Transaction = sqlx::query_as!(
        TransactionRow,
        r#"INSERT INTO transactions
              (account_id, posted_at, amount_minor, currency_code, description, category_id)
           VALUES (?1,?2,?3,?4,?5,?6)
           RETURNING id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                     merchant, merchant_id, notes, category_id, is_one_off AS "is_one_off!: bool",
                     linked_transaction_id, provider, external_id, categorized_by_rule_id,
                     ownership, person_id, created_at, updated_at"#,
        req.from_account_id,
        posted_at,
        outflow_minor,
        from_ccy,
        description,
        req.category_id
    )
    .fetch_one(&mut *tx)
    .await?
    .try_into()?;
    let inflow: Transaction = sqlx::query_as!(
        TransactionRow,
        r#"INSERT INTO transactions
              (account_id, posted_at, amount_minor, currency_code, description, category_id,
               linked_transaction_id)
           VALUES (?1,?2,?3,?4,?5,?6,?7)
           RETURNING id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                     merchant, merchant_id, notes, category_id, is_one_off AS "is_one_off!: bool",
                     linked_transaction_id, provider, external_id, categorized_by_rule_id,
                     ownership, person_id, created_at, updated_at"#,
        req.to_account_id,
        posted_at,
        inflow_minor,
        to_ccy,
        description,
        req.category_id,
        out.id
    )
    .fetch_one(&mut *tx)
    .await?
    .try_into()?;
    sqlx::query!(
        "UPDATE transactions SET linked_transaction_id=?2 WHERE id=?1",
        out.id,
        inflow.id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let out = fetch(db, out.id).await?;
    Ok(vec![out, inflow])
}

/// Best-effort reconciliation of internal transfers across *all* accounts: for each
/// currently-unlinked transaction, link it to the single unlinked transaction on a
/// *different* account with the exact opposite amount, same currency, posted within
/// `window_days` — the classic "money left one account, arrived in another" pair (a
/// Sharesies withdrawal landing in the bank, a bank-to-bank transfer, …). Only an
/// unambiguous 1:1 match is linked; zero or multiple candidates are left untouched for the
/// user to reconcile by hand, so it's idempotent and safe to run repeatedly.
///
/// Runs on a schedule rather than one-shot at import (see `sure_api::transfer_link`), so a
/// pair links no matter the order its two sides were imported/synced — the case that left
/// a Sharesies↔bank transfer unlinked when the bank was synced after the brokerage import.
/// Returns how many pairs were newly linked.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn link_transfers(db: &Db, window_days: i64) -> AppResult<i64> {
    // Snapshot the candidate ids up front; each link mutates both sides, so we re-check
    // each one is still unlinked before pairing it (an earlier iteration may have already
    // consumed it as some other row's counterpart).
    let ids = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM transactions
            WHERE linked_transaction_id IS NULL ORDER BY id"#
    )
    .fetch_all(db)
    .await?;

    let mut linked = 0i64;
    for id in ids {
        // Load this row's current state (it may have been linked as a prior counterpart).
        let Some(row) = sqlx::query!(
            "SELECT account_id, amount_minor, currency_code, posted_at FROM transactions
              WHERE id=?1 AND linked_transaction_id IS NULL",
            id
        )
        .fetch_optional(db)
        .await?
        else {
            continue;
        };
        let (account_id, amount, currency, posted_at) = (
            row.account_id,
            row.amount_minor,
            row.currency_code,
            row.posted_at,
        );

        // This row's opposite-amount counterparts on other accounts. Need exactly one.
        let opposite = -amount; // an outflow here meets an inflow there
        let candidates = sqlx::query!(
            r#"SELECT id AS "id!", account_id, posted_at FROM transactions
                WHERE account_id <> ?1
                  AND linked_transaction_id IS NULL
                  AND amount_minor = ?2
                  AND currency_code = ?3
                  AND ABS(julianday(posted_at) - julianday(?4)) <= ?5
                LIMIT 2"#,
            account_id,
            opposite,
            currency,
            posted_at,
            window_days
        )
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|r| (r.id, r.account_id, r.posted_at))
        .collect::<Vec<_>>();
        let [(other, other_account, other_posted_at)] = candidates.as_slice() else {
            continue; // zero or multiple → ambiguous from this side, leave it
        };

        // Mutual uniqueness: the counterpart must, in turn, have exactly one match (which
        // is necessarily this row). Otherwise the amount is ambiguous from *its* side — e.g.
        // one deposit with two possible source withdrawals: each withdrawal sees only the
        // one deposit, but the deposit doesn't uniquely identify a withdrawal, so linking
        // either would be a guess. Leave both for manual reconciliation.
        let counterpart_matches = sqlx::query_scalar!(
            // The counterpart's opposite is this row's own amount.
            "SELECT COUNT(*) FROM (SELECT id FROM transactions
              WHERE account_id <> ?1
                AND linked_transaction_id IS NULL
                AND amount_minor = ?2
                AND currency_code = ?3
                AND ABS(julianday(posted_at) - julianday(?4)) <= ?5
              LIMIT 2)",
            other_account,
            amount,
            currency,
            other_posted_at,
            window_days
        )
        .fetch_one(db)
        .await?;

        if counterpart_matches == 1 {
            link(
                db,
                id,
                LinkRequest {
                    linked_transaction_id: *other,
                },
            )
            .await?;
            linked += 1;
        }
    }
    Ok(linked)
}

// ---- helpers -------------------------------------------------------------

#[tracing::instrument(level = "debug", skip_all)]
async fn fetch(db: &Db, id: i64) -> AppResult<Transaction> {
    sqlx::query_as!(
        TransactionRow,
        r#"SELECT id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                  merchant, merchant_id, notes, category_id, is_one_off AS "is_one_off!: bool",
                  linked_transaction_id, provider, external_id, categorized_by_rule_id,
                  ownership, person_id, created_at, updated_at
             FROM transactions WHERE id = ?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("transaction"))?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
async fn account_currency(db: &Db, account_id: i64) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar!(
        "SELECT currency_code FROM accounts WHERE id = ?1",
        account_id
    )
    .fetch_optional(db)
    .await?)
}

#[tracing::instrument(level = "debug", skip_all)]
async fn resolve_currency(db: &Db, input: &SaveTransaction) -> AppResult<String> {
    match input.currency_code.as_deref().filter(|s| !s.is_empty()) {
        Some(c) => Ok(c.trim().to_uppercase()),
        None => account_currency(db, input.account_id)
            .await?
            .ok_or(AppError::validation("account does not exist")),
    }
}

#[tracing::instrument(level = "debug", skip_all)]
async fn validate_category(db: &Db, category_id: Option<i64>) -> AppResult<()> {
    if let Some(cid) = category_id {
        let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM categories WHERE id=?1", cid)
            .fetch_one(db)
            .await?;
        if exists == 0 {
            return Err(AppError::validation("category does not exist"));
        }
    }
    Ok(())
}

// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
// (CLAUDE.md rule 2's escape hatch) — the arm above is exhaustive over our own types.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account or currency does not exist")
        }
        other => AppError::from(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::transactions::BulkIds;
    use sure_core::{AccountKind, IsoDate, Money, SaveAccount, SaveTransaction};

    /// A batch the cap accepts. Panics rather than returning a `Result` so a test that
    /// accidentally exceeds the cap says so instead of quietly asserting on an error path.
    fn batch(ids: Vec<i64>) -> BulkIds {
        BulkIds::new(ids).expect("test batch should be within the bulk cap")
    }

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    async fn account(db: &Db, name: &str) -> i64 {
        crate::accounts::create(
            db,
            SaveAccount {
                name: name.to_string(),
                kind: AccountKind::Bank,
                // A bank account requires an institution, and every create requires an
                // opening balance; zero is the "started empty" case, which seeds no rows
                // (see `accounts::insert`) and so leaves these tests' ledgers to themselves.
                institution: Some("ANZ".to_string()),
                currency_code: "NZD".to_string(),
                metadata: None,
                archived: false,
                sort_order: 0,
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".to_string()),
                // These tests don't care who owns the account; joint needs no person row.
                ownership: sure_core::Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn tx(db: &Db, account_id: i64, posted_at: &str, amount_minor: i64) -> i64 {
        create(
            db,
            SaveTransaction {
                account_id,
                posted_at: IsoDate::parse(posted_at).unwrap(),
                amount_minor: Money::new(amount_minor).unwrap(),
                currency_code: Some("NZD".to_string()),
                description: "t".to_string(),
                merchant: None,
                notes: None,
                category_id: None,
                is_one_off: false,
                merchant_id: None,
                ownership: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn auto_links_a_single_unambiguous_opposite_match() {
        let db = test_db().await;
        let brokerage = account(&db, "Sharesies").await;
        let bank = account(&db, "Bank").await;

        // A deposit into the wallet (+100) and the bank's matching outflow (-100) 3 days
        // apart → one unambiguous pair.
        let deposit = tx(&db, brokerage, "2026-01-05", 10_000).await;
        let withdrawal = tx(&db, bank, "2026-01-08", -10_000).await;

        let linked = link_transfers(&db, 5).await.unwrap();
        assert_eq!(linked, 1);

        assert_eq!(
            fetch(&db, deposit).await.unwrap().linked_transaction_id,
            Some(withdrawal)
        );
        assert_eq!(
            fetch(&db, withdrawal).await.unwrap().linked_transaction_id,
            Some(deposit)
        );
    }

    /// A transfer's direction is normalised with `.abs()`, and the outflow leg is the negation
    /// of that. On the raw `i64` these fields used to be, `i64::MIN` made both steps lie (a
    /// debug panic, or two wrapped legs and a 201 in release); `sure_core::Money` makes the
    /// hostile value unconstructible, so the only thing left to prove here is that the
    /// normalisation is still *correct* — including from a caller who sent the source amount
    /// negative, and at the ceiling, where an `i64` was previously one negation from wrapping.
    #[tokio::test]
    async fn a_transfer_normalises_its_legs_at_any_legal_magnitude() {
        let db = test_db().await;
        let from = account(&db, "Bank").await;
        let to = account(&db, "Savings").await;

        for sent in [250_00, -250_00, sure_core::MAX_MONEY_MINOR] {
            let legs = create_transfer(
                &db,
                TransferRequest {
                    from_account_id: from,
                    to_account_id: to,
                    posted_at: IsoDate::parse("2026-02-01").unwrap(),
                    from_amount_minor: Money::new(sent).unwrap(),
                    to_amount_minor: None,
                    description: "t".to_string(),
                    category_id: None,
                },
            )
            .await
            .unwrap();
            assert_eq!(legs[0].amount_minor, -sent.abs(), "outflow leg for {sent}");
            assert_eq!(legs[1].amount_minor, sent.abs(), "inflow leg for {sent}");
        }

        // And the value that used to break it cannot be built in the first place, so there is
        // no `create_transfer` call to make with it.
        assert!(Money::new(i64::MIN).is_err());
    }

    #[tokio::test]
    async fn leaves_ambiguous_matches_untouched() {
        let db = test_db().await;
        let brokerage = account(&db, "Sharesies").await;
        let bank = account(&db, "Bank").await;

        let _deposit = tx(&db, brokerage, "2026-01-05", 10_000).await;
        // Two equally-valid bank counterparts → ambiguous, so nothing is linked.
        let _a = tx(&db, bank, "2026-01-05", -10_000).await;
        let _b = tx(&db, bank, "2026-01-06", -10_000).await;

        let linked = link_transfers(&db, 5).await.unwrap();
        assert_eq!(linked, 0);
    }

    async fn category(db: &Db, name: &str) -> i64 {
        crate::categories::create(
            db,
            sure_core::SaveCategory {
                name: name.to_string(),
                kind: sure_core::CategoryKind::Expense,
                parent_id: None,
                color: None,
                icon: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap()
        .id
    }

    /// The gap `category_id` cannot express: there is no id meaning "none", so without this
    /// filter the only way to reach the uncategorised rows is to pull the whole ledger and
    /// sift it client-side — which is exactly what the MCP tool must not do.
    #[tokio::test]
    async fn the_uncategorized_filter_selects_each_side_and_omitting_it_selects_both() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let groceries = category(&db, "Groceries").await;
        let bare = tx(&db, acc, "2026-01-01", -100).await;
        let filed = tx(&db, acc, "2026-01-02", -200).await;
        bulk_update(
            &db,
            BulkUpdate {
                ids: batch(vec![filed]),
                category_id: Some(Some(groceries)),
                merchant_id: None,
                is_one_off: None,
                ownership: None,
            },
        )
        .await
        .unwrap();

        let ids = |rows: Vec<Transaction>| rows.into_iter().map(|t| t.id).collect::<Vec<_>>();

        assert_eq!(
            ids(list(
                &db,
                TxQuery {
                    uncategorized: Some(true),
                    ..Default::default()
                }
            )
            .await
            .unwrap()),
            vec![bare]
        );
        assert_eq!(
            ids(list(
                &db,
                TxQuery {
                    uncategorized: Some(false),
                    ..Default::default()
                }
            )
            .await
            .unwrap()),
            vec![filed]
        );
        // Omitted is not "false": the default listing still shows everything.
        assert_eq!(
            ids(list(&db, TxQuery::default()).await.unwrap()).len(),
            2,
            "omitting the filter must not narrow the ledger"
        );
    }

    #[tokio::test]
    async fn bulk_update_patches_only_the_given_fields() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let groceries = category(&db, "Groceries").await;
        let a = tx(&db, acc, "2026-01-01", -100).await;
        let b = tx(&db, acc, "2026-01-02", -200).await;
        let untouched = tx(&db, acc, "2026-01-03", -300).await;

        // Set the category + one-off on two rows; leave merchant absent (unchanged).
        let affected = bulk_update(
            &db,
            BulkUpdate {
                ids: batch(vec![a, b]),
                category_id: Some(Some(groceries)),
                merchant_id: None,
                is_one_off: Some(true),
                ownership: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(affected, 2);

        for id in [a, b] {
            let t = fetch(&db, id).await.unwrap();
            assert_eq!(t.category_id, Some(groceries));
            assert!(t.is_one_off);
        }
        // The row not in `ids` is left alone.
        let t = fetch(&db, untouched).await.unwrap();
        assert_eq!(t.category_id, None);
        assert!(!t.is_one_off);
    }

    #[tokio::test]
    async fn bulk_update_clears_a_field_with_an_explicit_null() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let groceries = category(&db, "Groceries").await;
        let a = tx(&db, acc, "2026-01-01", -100).await;
        bulk_update(
            &db,
            BulkUpdate {
                ids: batch(vec![a]),
                category_id: Some(Some(groceries)),
                merchant_id: None,
                is_one_off: None,
                ownership: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(fetch(&db, a).await.unwrap().category_id, Some(groceries));

        // `Some(None)` (JSON `null`) clears it; `None` (omitted) would have left it.
        bulk_update(
            &db,
            BulkUpdate {
                ids: batch(vec![a]),
                category_id: Some(None),
                merchant_id: None,
                is_one_off: None,
                ownership: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(fetch(&db, a).await.unwrap().category_id, None);
    }

    #[tokio::test]
    async fn bulk_update_is_a_noop_with_no_fields_to_set() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let a = tx(&db, acc, "2026-01-01", -100).await;
        // An empty id list can no longer reach here at all — `BulkIds` refuses it while the
        // body is parsed (see `sure_core::transactions`), which is why this test no longer has
        // a "no ids" half. Ids, but nothing to set, is still a legitimate 0.
        assert_eq!(
            bulk_update(
                &db,
                BulkUpdate {
                    ids: batch(vec![a]),
                    category_id: None,
                    merchant_id: None,
                    is_one_off: None,
                    ownership: None,
                }
            )
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn bulk_delete_removes_the_listed_rows() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let a = tx(&db, acc, "2026-01-01", -100).await;
        let b = tx(&db, acc, "2026-01-02", -200).await;
        let c = tx(&db, acc, "2026-01-03", -300).await;

        assert_eq!(bulk_delete(&db, &[a, b]).await.unwrap(), 2);
        assert!(fetch(&db, a).await.is_err());
        assert!(fetch(&db, b).await.is_err());
        assert!(fetch(&db, c).await.is_ok());
        // Deleting an empty set / already-gone ids is a harmless 0.
        assert_eq!(bulk_delete(&db, &[]).await.unwrap(), 0);
        assert_eq!(bulk_delete(&db, &[a]).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_batch_at_the_cap_still_executes() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let real = tx(&db, acc, "2026-01-01", -100).await;
        // One real row plus filler ids that match nothing: what is under test is that a
        // statement with `MAX_BULK_IDS` binds prepares and runs, not what it matches.
        let mut ids = vec![real];
        ids.extend((1..MAX_BULK_IDS as i64).map(|i| 1_000_000 + i));
        assert_eq!(ids.len(), MAX_BULK_IDS);

        assert_eq!(
            bulk_update(
                &db,
                BulkUpdate {
                    ids: batch(ids.clone()),
                    category_id: None,
                    merchant_id: None,
                    is_one_off: Some(true),
                    ownership: None,
                }
            )
            .await
            .unwrap(),
            1
        );
        assert!(fetch(&db, real).await.unwrap().is_one_off);
        assert_eq!(bulk_delete(&db, &ids).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn bulk_delete_over_the_cap_is_a_validation_error_not_a_500() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let a = tx(&db, acc, "2026-01-01", -100).await;
        // `bulk_delete` takes a bare slice, so it is the one bulk entry point a caller can
        // hand an unbounded list to. Past SQLite's bind ceiling the *prepare* fails and the
        // client sees a scrubbed 500; the guard makes it a 422 naming the limit instead.
        let over: Vec<i64> = (0..=MAX_BULK_IDS as i64).collect();
        let err = bulk_delete(&db, &over).await.unwrap_err();
        assert_eq!(err.code(), "validation", "got {err:?}");
        let message = err.to_string();
        assert!(
            message.contains(&MAX_BULK_IDS.to_string())
                && message.contains(&over.len().to_string()),
            "message must name the limit and the offending count: {message}"
        );
        // Refused whole, not half-applied.
        assert!(fetch(&db, a).await.is_ok());
    }

    #[tokio::test]
    async fn does_not_match_outside_the_date_window_or_across_currencies() {
        let db = test_db().await;
        let brokerage = account(&db, "Sharesies").await;
        let bank = account(&db, "Bank").await;

        let _deposit = tx(&db, brokerage, "2026-01-05", 10_000).await;
        // Same amount but 40 days later → outside a 5-day window.
        let _far = tx(&db, bank, "2026-02-14", -10_000).await;

        assert_eq!(link_transfers(&db, 5).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn links_transfers_regardless_of_which_side_was_added_first() {
        // The Sharesies↔bank case: the bank side arrives (is synced) only after the
        // brokerage import already ran, yet a later global pass still pairs them.
        let db = test_db().await;
        let brokerage = account(&db, "Sharesies").await;
        let bank = account(&db, "Bank").await;

        let withdrawal = tx(&db, brokerage, "2025-11-02", -114_269_63).await;
        // First pass with only one side present: nothing to pair yet.
        assert_eq!(link_transfers(&db, 5).await.unwrap(), 0);

        // The bank deposit is synced later; the next pass links the pair.
        let deposit = tx(&db, bank, "2025-11-02", 114_269_63).await;
        assert_eq!(link_transfers(&db, 5).await.unwrap(), 1);
        assert_eq!(
            fetch(&db, withdrawal).await.unwrap().linked_transaction_id,
            Some(deposit)
        );

        // Idempotent: a further pass finds nothing new.
        assert_eq!(link_transfers(&db, 5).await.unwrap(), 0);
    }
    // --- attribution --------------------------------------------------------

    async fn person(db: &Db, name: &str) -> i64 {
        crate::people::create(
            db,
            sure_core::SavePerson {
                name: name.to_string(),
                color: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap()
        .id
    }

    /// An account owned by `ownership`, so the inheritance path has something to inherit.
    async fn owned_account(db: &Db, name: &str, ownership: Ownership) -> i64 {
        crate::accounts::create(
            db,
            SaveAccount {
                name: name.to_string(),
                kind: AccountKind::Bank,
                institution: Some("ANZ".to_string()),
                currency_code: "NZD".to_string(),
                metadata: None,
                archived: false,
                sort_order: 0,
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".to_string()),
                ownership,
            },
        )
        .await
        .unwrap()
        .id
    }

    async fn attributed(db: &Db, to: Ownership) -> Vec<String> {
        let mut names: Vec<String> = list(
            db,
            TxQuery {
                attributed_to: Some(to),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.description)
        .collect();
        names.sort();
        names
    }

    async fn described(db: &Db, account_id: i64, description: &str, ownership: Option<Ownership>) {
        create(
            db,
            SaveTransaction {
                account_id,
                posted_at: IsoDate::parse("2026-02-01").unwrap(),
                amount_minor: Money::new(-100).unwrap(),
                currency_code: Some("NZD".to_string()),
                description: description.to_string(),
                merchant: None,
                notes: None,
                category_id: None,
                is_one_off: false,
                merchant_id: None,
                ownership,
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_transaction_inherits_its_accounts_owner_and_an_override_wins() {
        let db = test_db().await;
        let alex = person(&db, "Alex").await;
        let sam = person(&db, "Sam").await;
        let alexs = owned_account(&db, "Alex's card", Ownership::Person { person_id: alex }).await;
        let shared = owned_account(&db, "Joint account", Ownership::Joint).await;

        described(&db, alexs, "alex inherited", None).await;
        described(&db, shared, "joint inherited", None).await;
        // The two cases an override exists for, in both directions.
        described(
            &db,
            shared,
            "sams share of the joint card",
            Some(Ownership::Person { person_id: sam }),
        )
        .await;
        described(
            &db,
            alexs,
            "a shared expense on alex's card",
            Some(Ownership::Joint),
        )
        .await;

        assert_eq!(
            attributed(&db, Ownership::Person { person_id: alex }).await,
            ["alex inherited"]
        );
        assert_eq!(
            attributed(&db, Ownership::Person { person_id: sam }).await,
            ["sams share of the joint card"]
        );
        assert_eq!(
            attributed(&db, Ownership::Joint).await,
            ["a shared expense on alex's card", "joint inherited"]
        );
    }

    /// Inheritance is by reference, not a copy taken at import time — so sorting out whose
    /// account is whose moves its whole history at once, which is the entire point.
    #[tokio::test]
    async fn re_attributing_an_account_moves_its_uneoverridden_history() {
        let db = test_db().await;
        let alex = person(&db, "Alex").await;
        let sam = person(&db, "Sam").await;
        let account = owned_account(&db, "Everyday", Ownership::Person { person_id: alex }).await;
        described(&db, account, "inherited", None).await;
        described(
            &db,
            account,
            "pinned to sam",
            Some(Ownership::Person { person_id: sam }),
        )
        .await;

        crate::accounts::set_ownership(&db, account, Ownership::Person { person_id: sam })
            .await
            .unwrap();

        // The inherited row followed the account; the override didn't move.
        assert!(
            attributed(&db, Ownership::Person { person_id: alex })
                .await
                .is_empty()
        );
        assert_eq!(
            attributed(&db, Ownership::Person { person_id: sam }).await,
            ["inherited", "pinned to sam"]
        );
    }

    #[tokio::test]
    async fn bulk_update_sets_and_clears_an_override() {
        let db = test_db().await;
        let alex = person(&db, "Alex").await;
        let account = owned_account(&db, "Joint", Ownership::Joint).await;
        described(&db, account, "one", None).await;
        described(&db, account, "two", None).await;
        let ids: Vec<i64> = list(&db, TxQuery::default())
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();

        let set = BulkUpdate {
            ids: batch(ids.clone()),
            category_id: None,
            merchant_id: None,
            is_one_off: None,
            ownership: Some(Some(Ownership::Person { person_id: alex })),
        };
        assert_eq!(bulk_update(&db, set).await.unwrap(), 2);
        assert_eq!(
            attributed(&db, Ownership::Person { person_id: alex }).await,
            ["one", "two"]
        );

        // A present `null` clears the override, so both go back to following the account.
        let clear = BulkUpdate {
            ids: batch(ids),
            category_id: None,
            merchant_id: None,
            is_one_off: None,
            ownership: Some(None),
        };
        assert_eq!(bulk_update(&db, clear).await.unwrap(), 2);
        assert_eq!(attributed(&db, Ownership::Joint).await, ["one", "two"]);
    }

    #[tokio::test]
    async fn an_override_naming_nobody_is_refused() {
        let db = test_db().await;
        let account = owned_account(&db, "Joint", Ownership::Joint).await;
        let err = create(
            &db,
            SaveTransaction {
                account_id: account,
                posted_at: IsoDate::parse("2026-02-01").unwrap(),
                amount_minor: Money::new(-100).unwrap(),
                currency_code: Some("NZD".to_string()),
                description: "x".to_string(),
                merchant: None,
                notes: None,
                category_id: None,
                is_one_off: false,
                merchant_id: None,
                ownership: Some(Ownership::Person { person_id: 404 }),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Validation(_)), "got {err:?}");
    }

    /// Imported rows, tagged as a provider's, so the cutover and undo have something to
    /// find. Goes through `providers::import_transactions` because that is the only writer
    /// that sets `provider`/`external_id`.
    async fn imported(db: &Db, account_id: i64, tag: &str, dates: &[&str]) {
        let rows: Vec<crate::providers::ImportRow> = dates
            .iter()
            .enumerate()
            .map(|(i, d)| crate::providers::ImportRow {
                // `idx_tx_provider_external` is not scoped by account, so the account has
                // to be in the id or the same tag on a second account silently dedupes
                // against the first.
                external_id: format!("{tag}-{account_id}-{i}"),
                posted_at: format!("{d}T12:00:00+00:00"),
                amount_minor: -1_00,
                currency_code: None,
                description: "imported".to_string(),
                merchant: None,
                category_name: None,
                category_group: None,
                category_kind: None,
                is_one_off: false,
            })
            .collect();
        crate::providers::import_transactions(db, account_id, "NZD", tag, &rows)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn the_cutover_is_the_earliest_row_another_feed_owns() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        imported(&db, acct, "akahu#10", &["2025-08-03", "2026-01-04"]).await;

        assert_eq!(
            earliest_posted_at_from_other_feed(&db, acct, "asb#1")
                .await
                .unwrap()
                .as_deref(),
            Some("2025-08-03T12:00:00+00:00")
        );
    }

    /// The exclusion that makes re-uploading safe: without it a second import would see its
    /// own 2020 rows, take those as the cutover, and hold back everything.
    #[tokio::test]
    async fn the_cutover_ignores_the_importers_own_earlier_rows() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        let tag = format!("asb#{acct}");
        imported(&db, acct, "akahu#10", &["2025-08-03"]).await;
        imported(&db, acct, &tag, &["2020-01-01", "2021-06-30"]).await;

        assert_eq!(
            earliest_posted_at_from_other_feed(&db, acct, &tag)
                .await
                .unwrap()
                .as_deref(),
            Some("2025-08-03T12:00:00+00:00")
        );
    }

    /// A hand-entered row (and the seeded opening balance) carries no `provider`. One of
    /// those must not be read as a feed covering everything after it.
    #[tokio::test]
    async fn a_manual_row_does_not_set_the_cutover() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        tx(&db, acct, "2019-01-01", -5_00).await;

        assert_eq!(
            earliest_posted_at_from_other_feed(&db, acct, "asb#1")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn no_feed_on_the_account_means_no_cutover() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        assert_eq!(
            earliest_posted_at_from_other_feed(&db, acct, "asb#1")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn the_earliest_row_is_found_whatever_wrote_it() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        assert_eq!(earliest_posted_at(&db, acct).await.unwrap(), None);

        imported(&db, acct, "asb#1", &["2020-06-01"]).await;
        tx(&db, acct, "2019-01-01", -5_00).await;
        // Counts the manual row too, unlike the cutover lookup.
        assert_eq!(
            earliest_posted_at(&db, acct).await.unwrap().as_deref(),
            Some("2019-01-01")
        );
    }

    /// An opening-balance row has to reach the balance reconstruction while staying out of
    /// income, which is what `is_one_off` is for — so the importer must be able to set it.
    #[tokio::test]
    async fn an_imported_row_can_be_marked_one_off() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        crate::providers::import_transactions(
            &db,
            acct,
            "NZD",
            "asb#1",
            &[
                crate::providers::ImportRow {
                    external_id: "opening".to_string(),
                    posted_at: "2019-12-31T12:00:00+00:00".to_string(),
                    amount_minor: 18_694_18,
                    currency_code: None,
                    description: "Opening balance".to_string(),
                    merchant: None,
                    category_name: None,
                    category_group: None,
                    category_kind: None,
                    is_one_off: true,
                },
                crate::providers::ImportRow {
                    external_id: "ordinary".to_string(),
                    posted_at: "2020-01-01T12:00:00+00:00".to_string(),
                    amount_minor: -1_00,
                    currency_code: None,
                    description: "Coffee".to_string(),
                    merchant: None,
                    category_name: None,
                    category_group: None,
                    category_kind: None,
                    is_one_off: false,
                },
            ],
        )
        .await
        .unwrap();

        let rows = list(&db, TxQuery::default()).await.unwrap();
        let by_desc: std::collections::HashMap<&str, &Transaction> =
            rows.iter().map(|t| (t.description.as_str(), t)).collect();
        assert!(by_desc["Opening balance"].is_one_off);
        assert_eq!(by_desc["Opening balance"].amount_minor, 18_694_18);
        assert!(!by_desc["Coffee"].is_one_off);
    }

    /// The durable memory a repeat upload routes itself by: the ids record which upstream
    /// account went to which local one, so the mapping survives without a schema for it.
    #[tokio::test]
    async fn sampling_external_ids_recovers_the_upstream_mapping() {
        let db = test_db().await;
        let chequing = account(&db, "Chequing").await;
        let savings = account(&db, "Savings").await;
        imported(&db, chequing, &format!("asb#{chequing}"), &["2020-01-01"]).await;
        imported(&db, savings, &format!("asb#{savings}"), &["2020-02-01"]).await;
        // A different importer's rows, and a manual row, are both out of scope.
        imported(&db, chequing, "akahu#10", &["2025-08-03"]).await;
        tx(&db, chequing, "2019-01-01", -5_00).await;

        let mut found = sample_external_ids(&db, "asb#").await.unwrap();
        found.sort_unstable();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, chequing);
        assert!(found[0].1.starts_with(&format!("asb#{chequing}-")));
        assert_eq!(found[1].0, savings);

        assert!(
            sample_external_ids(&db, "nothing#")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn undo_removes_only_that_importers_rows_on_that_account() {
        let db = test_db().await;
        let acct = account(&db, "Chequing").await;
        let other = account(&db, "Savings").await;
        imported(&db, acct, "asb#1", &["2020-01-01", "2020-02-01"]).await;
        imported(&db, acct, "akahu#10", &["2025-08-03"]).await;
        // Same tag on a different account: out of reach.
        imported(&db, other, "asb#1", &["2020-03-01"]).await;
        let manual = tx(&db, acct, "2019-01-01", -5_00).await;

        assert_eq!(delete_by_provider(&db, acct, "asb#1").await.unwrap(), 2);

        let left = list(&db, TxQuery::default()).await.unwrap();
        let mut kept: Vec<&str> = left.iter().map(|t| t.posted_at.as_str()).collect();
        kept.sort_unstable();
        assert_eq!(
            kept,
            [
                "2019-01-01",
                "2020-03-01T12:00:00+00:00",
                "2025-08-03T12:00:00+00:00"
            ]
        );
        assert!(left.iter().any(|t| t.id == manual));
        // Idempotent: nothing left to remove.
        assert_eq!(delete_by_provider(&db, acct, "asb#1").await.unwrap(), 0);
    }
}
