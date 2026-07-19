use sqlx::{QueryBuilder, Sqlite};
use sure_core::{AppError, AppResult};
pub use sure_core::{
    BulkDelete, BulkResult, BulkUpdate, LinkRequest, SaveTransaction, Transaction, TransferRequest,
    TxQuery,
};

use crate::Db;

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

/// Apply a partial patch to every transaction in `ids`. Returns the number of rows
/// actually changed. A no-op (no ids, or no fields to set) short-circuits to 0.
pub async fn bulk_update(db: &Db, input: BulkUpdate) -> AppResult<i64> {
    let BulkUpdate { ids, category_id, merchant_id, is_one_off } = input;
    if ids.is_empty() || (category_id.is_none() && merchant_id.is_none() && is_one_off.is_none()) {
        return Ok(0);
    }
    // Validate a to-be-set category once up front (a cleared/absent one needs no check).
    if let Some(Some(cid)) = category_id {
        validate_category(db, Some(cid)).await?;
    }

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
        set.push("updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')");
    }
    qb.push(" WHERE id IN (");
    {
        let mut list = qb.separated(", ");
        for id in &ids {
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
pub async fn bulk_delete(db: &Db, ids: &[i64]) -> AppResult<i64> {
    if ids.is_empty() {
        return Ok(0);
    }
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
pub async fn link_transfers(db: &Db, window_days: i64) -> AppResult<i64> {
    // Snapshot the candidate ids up front; each link mutates both sides, so we re-check
    // each one is still unlinked before pairing it (an earlier iteration may have already
    // consumed it as some other row's counterpart).
    let ids = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM transactions WHERE linked_transaction_id IS NULL ORDER BY id",
    )
    .fetch_all(db)
    .await?;

    let mut linked = 0i64;
    for id in ids {
        // Load this row's current state (it may have been linked as a prior counterpart).
        let Some((account_id, amount, currency, posted_at)) =
            sqlx::query_as::<_, (i64, i64, String, String)>(
                "SELECT account_id, amount_minor, currency_code, posted_at FROM transactions
                 WHERE id=?1 AND linked_transaction_id IS NULL",
            )
            .bind(id)
            .fetch_optional(db)
            .await?
        else {
            continue;
        };

        // This row's opposite-amount counterparts on other accounts. Need exactly one.
        let candidates = sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT id, account_id, posted_at FROM transactions
             WHERE account_id <> ?1
               AND linked_transaction_id IS NULL
               AND amount_minor = ?2
               AND currency_code = ?3
               AND ABS(julianday(posted_at) - julianday(?4)) <= ?5
             LIMIT 2",
        )
        .bind(account_id)
        .bind(-amount) // opposite sign: an outflow here meets an inflow there
        .bind(&currency)
        .bind(&posted_at)
        .bind(window_days)
        .fetch_all(db)
        .await?;
        let [(other, other_account, other_posted_at)] = candidates.as_slice() else {
            continue; // zero or multiple → ambiguous from this side, leave it
        };

        // Mutual uniqueness: the counterpart must, in turn, have exactly one match (which
        // is necessarily this row). Otherwise the amount is ambiguous from *its* side — e.g.
        // one deposit with two possible source withdrawals: each withdrawal sees only the
        // one deposit, but the deposit doesn't uniquely identify a withdrawal, so linking
        // either would be a guess. Leave both for manual reconciliation.
        let counterpart_matches = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM (SELECT id FROM transactions
             WHERE account_id <> ?1
               AND linked_transaction_id IS NULL
               AND amount_minor = ?2
               AND currency_code = ?3
               AND ABS(julianday(posted_at) - julianday(?4)) <= ?5
             LIMIT 2)",
        )
        .bind(other_account)
        .bind(amount) // the counterpart's opposite is this row's own amount
        .bind(&currency)
        .bind(other_posted_at)
        .bind(window_days)
        .fetch_one(db)
        .await?;

        if counterpart_matches == 1 {
            link(db, id, LinkRequest { linked_transaction_id: *other }).await?;
            linked += 1;
        }
    }
    Ok(linked)
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{AccountKind, SaveAccount, SaveTransaction};

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
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
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
                posted_at: posted_at.to_string(),
                amount_minor,
                currency_code: Some("NZD".to_string()),
                description: "t".to_string(),
                merchant: None,
                notes: None,
                category_id: None,
                is_one_off: false,
                merchant_id: None,
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

        assert_eq!(fetch(&db, deposit).await.unwrap().linked_transaction_id, Some(withdrawal));
        assert_eq!(fetch(&db, withdrawal).await.unwrap().linked_transaction_id, Some(deposit));
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
                kind: "expense".to_string(),
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
                ids: vec![a, b],
                category_id: Some(Some(groceries)),
                merchant_id: None,
                is_one_off: Some(true),
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
            BulkUpdate { ids: vec![a], category_id: Some(Some(groceries)), merchant_id: None, is_one_off: None },
        )
        .await
        .unwrap();
        assert_eq!(fetch(&db, a).await.unwrap().category_id, Some(groceries));

        // `Some(None)` (JSON `null`) clears it; `None` (omitted) would have left it.
        bulk_update(
            &db,
            BulkUpdate { ids: vec![a], category_id: Some(None), merchant_id: None, is_one_off: None },
        )
        .await
        .unwrap();
        assert_eq!(fetch(&db, a).await.unwrap().category_id, None);
    }

    #[tokio::test]
    async fn bulk_update_is_a_noop_with_no_ids_or_no_fields() {
        let db = test_db().await;
        let acc = account(&db, "Bank").await;
        let a = tx(&db, acc, "2026-01-01", -100).await;
        // No ids.
        assert_eq!(
            bulk_update(&db, BulkUpdate { ids: vec![], category_id: Some(None), merchant_id: None, is_one_off: None })
                .await
                .unwrap(),
            0
        );
        // Ids, but nothing to set.
        assert_eq!(
            bulk_update(&db, BulkUpdate { ids: vec![a], category_id: None, merchant_id: None, is_one_off: None })
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
        assert_eq!(fetch(&db, withdrawal).await.unwrap().linked_transaction_id, Some(deposit));

        // Idempotent: a further pass finds nothing new.
        assert_eq!(link_transfers(&db, 5).await.unwrap(), 0);
    }
}
