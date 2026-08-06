use std::collections::HashMap;

use serde_json::json;
use sure_core::{AppError, AppResult, CategoryKind, SyncOutcome};
pub use sure_core::{
    LinkGroupMember, LinkProviderAccount, LinkProviderGroup, Provider, ProviderSync, SaveProvider,
    SyncRequest,
};

use crate::Db;

#[tracing::instrument(level = "debug", skip_all)]
async fn resolve_category(
    db: &Db,
    category_cache: &mut HashMap<(String, Option<String>), i64>,
    group_cache: &mut HashMap<String, i64>,
    name: &str,
    group: Option<&str>,
    kind: CategoryKind,
) -> AppResult<i64> {
    let key = (name.to_string(), group.map(str::to_string));
    if let Some(&id) = category_cache.get(&key) {
        return Ok(id);
    }
    let parent_id = match group {
        Some(g) => Some(match group_cache.get(g) {
            Some(&id) => id,
            None => {
                let id = crate::categories::find_or_create(db, g, None, kind)
                    .await?
                    .id;
                group_cache.insert(g.to_string(), id);
                id
            }
        }),
        None => None,
    };
    let id = crate::categories::find_or_create(db, name, parent_id, kind)
        .await?
        .id;
    category_cache.insert(key, id);
    Ok(id)
}

#[tracing::instrument(level = "debug", skip_all)]
async fn resolve_merchant(
    db: &Db,
    cache: &mut HashMap<String, i64>,
    name: &str,
    default_category_id: Option<i64>,
) -> AppResult<i64> {
    let key = name.to_lowercase();
    if let Some(&id) = cache.get(&key) {
        return Ok(id);
    }
    let id = crate::merchants::find_or_create(db, name, default_category_id)
        .await?
        .id;
    cache.insert(key, id);
    Ok(id)
}

#[derive(Debug)]
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
    /// The source's own classification for this transaction (e.g. Akahu's NZFCC
    /// category), if any — resolved to a Sure category (find-or-create) rather than
    /// left uncategorized. `category_group` becomes that category's parent.
    pub category_name: Option<String>,
    pub category_group: Option<String>,
    /// Excluded from spend/income reports, but still counted towards balances and net
    /// worth. What an opening-balance row needs: it moves the account's value without
    /// being money earned or spent.
    pub is_one_off: bool,
    /// Flow direction for a newly-created category; `None` defaults to expense (most
    /// enrichment is spend-side). Only affects creation — an existing category keeps
    /// its kind.
    pub category_kind: Option<CategoryKind>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Provider>> {
    let rows = sqlx::query_as!(
        ProviderRow,
        r#"SELECT id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                  last_synced_at, created_at, updated_at
             FROM providers ORDER BY id"#
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Provider::from).collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<Provider> {
    let row = sqlx::query_as!(
        ProviderRow,
        r#"SELECT id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                  last_synced_at, created_at, updated_at
             FROM providers WHERE id=?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("provider"))?;
    Ok(row.into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveProvider) -> AppResult<Provider> {
    let config = input.config.clone().unwrap_or_else(|| json!({}));
    let name = input.name.trim();
    let config = config.to_string();
    let row = sqlx::query_as!(
        ProviderRow,
        r#"INSERT INTO providers (name, kind, account_id, config, enabled)
           VALUES (?1,?2,?3,?4,?5)
           RETURNING id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                     last_synced_at, created_at, updated_at"#,
        name,
        input.kind,
        input.account_id,
        config,
        input.enabled
    )
    .fetch_one(db)
    .await
    .map_err(map_fk)?;
    Ok(row.into())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveProvider) -> AppResult<Provider> {
    let config = input.config.clone().unwrap_or_else(|| json!({}));
    let name = input.name.trim();
    let config = config.to_string();
    let row = sqlx::query_as!(
        ProviderRow,
        r#"UPDATE providers SET name=?2, kind=?3, account_id=?4, config=?5, enabled=?6,
              updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
           WHERE id=?1
           RETURNING id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                     last_synced_at, created_at, updated_at"#,
        id,
        name,
        input.kind,
        input.account_id,
        config,
        input.enabled
    )
    .fetch_optional(db)
    .await
    .map_err(map_fk)?
    .ok_or(AppError::NotFound("provider"))?;
    Ok(row.into())
}

/// Link an upstream account to a local one, creating the local account first if
/// requested — atomically, so a failed provider insert can't leave an orphaned account.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn link(db: &Db, input: LinkProviderAccount) -> AppResult<Provider> {
    if input.new_account.is_some() == input.existing_account_id.is_some() {
        return Err(AppError::validation(
            "link requires exactly one of 'new_account' or 'existing_account_id'",
        ));
    }

    // The currency-existence check reads the pool directly (mirrors `transactions::unlink`
    // fetching outside the transaction); it's a plain lookup, not a mutation. `Linked` mode
    // asks only for what an upstream feed can actually know — see `ValidationMode`.
    let new_account_metadata = match &input.new_account {
        Some(a) => Some(crate::accounts::validate(db, a, crate::accounts::Write::Linked).await?),
        None => None,
    };

    let mut tx = db.begin().await?;

    let account_id =
        if let (Some(new_account), Some(metadata)) = (&input.new_account, &new_account_metadata) {
            crate::accounts::insert(&mut tx, new_account, metadata)
                .await?
                .id
        } else {
            let id = input
                .existing_account_id
                .expect("validated exactly-one above");
            let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM accounts WHERE id=?1", id)
                .fetch_one(&mut *tx)
                .await?;
            if exists == 0 {
                return Err(AppError::NotFound("account"));
            }
            id
        };

    let config = json!({ "external_account_id": input.external_id }).to_string();
    let name = input.name.trim();
    let row = sqlx::query_as!(
        ProviderRow,
        r#"INSERT INTO providers (name, kind, account_id, config, enabled)
           VALUES (?1,?2,?3,?4,1)
           RETURNING id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                     last_synced_at, created_at, updated_at"#,
        name,
        input.kind,
        account_id,
        config
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_fk)?;

    tx.commit().await?;
    Ok(row.into())
}

/// Link several upstream accounts to one local account in a single transaction — see
/// [`LinkProviderGroup`]. Creates (or resolves) the account once, then inserts one
/// `providers` row per member. Returns the created rows in input order.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn link_group(db: &Db, input: LinkProviderGroup) -> AppResult<Vec<Provider>> {
    if input.new_account.is_some() == input.existing_account_id.is_some() {
        return Err(AppError::validation(
            "link requires exactly one of 'new_account' or 'existing_account_id'",
        ));
    }
    if input.members.is_empty() {
        return Err(AppError::validation(
            "link group requires at least one member",
        ));
    }

    let new_account_metadata = match &input.new_account {
        Some(a) => Some(crate::accounts::validate(db, a, crate::accounts::Write::Linked).await?),
        None => None,
    };

    let mut tx = db.begin().await?;

    let account_id =
        if let (Some(new_account), Some(metadata)) = (&input.new_account, &new_account_metadata) {
            crate::accounts::insert(&mut tx, new_account, metadata)
                .await?
                .id
        } else {
            let id = input
                .existing_account_id
                .expect("validated exactly-one above");
            let exists = sqlx::query_scalar!("SELECT COUNT(*) FROM accounts WHERE id=?1", id)
                .fetch_one(&mut *tx)
                .await?;
            if exists == 0 {
                return Err(AppError::NotFound("account"));
            }
            id
        };

    let mut providers = Vec::with_capacity(input.members.len());
    for member in &input.members {
        let config = json!({ "external_account_id": member.external_id }).to_string();
        let name = member.name.trim();
        let row = sqlx::query_as!(
            ProviderRow,
            r#"INSERT INTO providers (name, kind, account_id, config, enabled)
               VALUES (?1,?2,?3,?4,1)
               RETURNING id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                         last_synced_at, created_at, updated_at"#,
            name,
            input.kind,
            account_id,
            config
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_fk)?;
        providers.push(Provider::from(row));
    }

    tx.commit().await?;
    Ok(providers)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query!("DELETE FROM providers WHERE id=?1", id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("provider"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn account_currency(db: &Db, account_id: i64) -> AppResult<String> {
    Ok(
        sqlx::query_scalar!("SELECT currency_code FROM accounts WHERE id=?1", account_id)
            .fetch_one(db)
            .await?,
    )
}

/// Transactions written per SQL statement, and per transaction.
///
/// SQLite's ceiling is 32766 bound variables in one statement
/// (`SQLITE_MAX_VARIABLE_NUMBER`); the insert below binds 11 per row, so 1000 rows uses
/// 11 000 of them and leaves room for a dozen more columns before the limit is anywhere
/// near. The other half of the choice is failure cost: a chunk is one transaction, so a
/// rollback (or a [`crate::with_busy_retry`] replay) loses at most a thousand rows of work,
/// while a 100 000-row import still takes 100 write-lock acquisitions instead of 100 000.
const IMPORT_CHUNK_ROWS: usize = 1000;

/// Per-row values that had to be looked up (or created) before the row could be written:
/// the currency it settles in and the ids its merchant/category resolved to.
struct ResolvedRow {
    currency_code: String,
    merchant_id: Option<i64>,
    category_id: Option<i64>,
}

/// Insert fetched transactions, deduping on (provider, external_id). Reuses (or creates)
/// a matching merchant/category per row from any source-supplied classification, so
/// providers that carry their own enrichment (e.g. Akahu's NZFCC categories) don't leave
/// every imported transaction uncategorized. Returns (imported, skipped).
///
/// Writes in chunked transactions of [`IMPORT_CHUNK_ROWS`]. Row-at-a-time autocommit — what
/// this did before — took the database's write lock once per row, so a 100 000-row backfill
/// was 100 000 lock acquisitions and 100 000 fsync-able commits, each one a chance for a
/// concurrent writer to collide and (past `busy_timeout`) fail the whole import with a 500.
/// Every writer of a transaction row goes through here, including the ASB CSV importer and
/// the brokerage wallet import, so they all get the batching and the retry.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn import_transactions(
    db: &Db,
    account_id: i64,
    account_currency: &str,
    provider_tag: &str,
    rows: &[ImportRow],
) -> AppResult<(i64, i64)> {
    let mut imported = 0i64;
    let mut skipped = 0i64;
    // Caches so a sync of many transactions sharing a handful of merchants/categories
    // (the common case) doesn't re-look-up the same name on every row.
    let mut merchant_cache: HashMap<String, i64> = HashMap::new();
    let mut category_cache: HashMap<(String, Option<String>), i64> = HashMap::new();
    let mut group_cache: HashMap<String, i64> = HashMap::new();

    for chunk in rows.chunks(IMPORT_CHUNK_ROWS) {
        // Resolve merchants/categories *before* opening the write transaction. Find-or-create
        // is itself a query and sometimes an insert; doing it with the chunk's transaction
        // open would hold the single write lock across every one of those round trips.
        let mut resolved = Vec::with_capacity(chunk.len());
        for t in chunk {
            let category_id = match &t.category_name {
                Some(name) if !name.trim().is_empty() => Some(
                    resolve_category(
                        db,
                        &mut category_cache,
                        &mut group_cache,
                        name,
                        t.category_group.as_deref(),
                        t.category_kind.unwrap_or_default(),
                    )
                    .await?,
                ),
                _ => None,
            };
            let merchant_id = match &t.merchant {
                Some(name) if !name.trim().is_empty() => {
                    Some(resolve_merchant(db, &mut merchant_cache, name, category_id).await?)
                }
                _ => None,
            };
            resolved.push(ResolvedRow {
                currency_code: t
                    .currency_code
                    .clone()
                    .unwrap_or_else(|| account_currency.to_string()),
                merchant_id,
                category_id,
            });
        }

        // One transaction for the chunk, replayed if another writer held the lock. Safe to
        // replay: a refused transaction committed nothing, and the insert is
        // `OR IGNORE` on (provider, external_id) anyway.
        let inserted = crate::with_busy_retry("providers::import_transactions", || {
            insert_chunk(db, account_id, provider_tag, chunk, &resolved)
        })
        .await?;
        imported += inserted;
        skipped += chunk.len() as i64 - inserted;
    }
    Ok((imported, skipped))
}

/// Write one chunk of already-resolved rows in a single transaction, returning how many were
/// new.
///
/// `INSERT OR IGNORE` makes the dedupe on `(provider, external_id)` the database's job, and
/// `rows_affected` on the multi-row statement counts exactly the rows that were not already
/// there — which is what the caller reports as `imported`, the rest being `skipped`.
async fn insert_chunk(
    db: &Db,
    account_id: i64,
    provider_tag: &str,
    chunk: &[ImportRow],
    resolved: &[ResolvedRow],
) -> AppResult<i64> {
    debug_assert_eq!(chunk.len(), resolved.len());
    let mut tx = db.begin().await?;
    // The one query in this module the compile-time checker cannot see: the number of
    // `VALUES` tuples is the chunk's length, so the SQL text is only known at runtime and
    // `sqlx::query!` needs a literal. The column list and its eleven `push_bind`s below are
    // therefore matched by hand — keep them in step.
    let mut builder = sqlx::QueryBuilder::new(
        "INSERT OR IGNORE INTO transactions
            (account_id, posted_at, amount_minor, currency_code, description, merchant,
             merchant_id, category_id, provider, external_id, is_one_off) ",
    );
    builder.push_values(chunk.iter().zip(resolved), |mut row, (t, r)| {
        row.push_bind(account_id)
            .push_bind(t.posted_at.as_str())
            .push_bind(t.amount_minor)
            .push_bind(r.currency_code.as_str())
            .push_bind(t.description.as_str())
            .push_bind(t.merchant.as_deref())
            .push_bind(r.merchant_id)
            .push_bind(r.category_id)
            .push_bind(provider_tag)
            .push_bind(t.external_id.as_str())
            .push_bind(t.is_one_off);
    });
    let inserted = builder.build().execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(inserted as i64)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update_last_synced(db: &Db, id: i64) -> AppResult<()> {
    sqlx::query!(
        "UPDATE providers SET last_synced_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
        id
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Parse a stored `status` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `SyncOutcome::as_str`, so an unparseable value means the row
/// came from something else entirely and deserves a real error, not a silent default.
fn parse_status(status: String) -> AppResult<SyncOutcome> {
    status
        .parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

#[derive(Debug)]
struct ProviderSyncRow {
    id: i64,
    provider_id: i64,
    imported: i64,
    skipped: i64,
    status: String,
    detail: Option<String>,
    created_at: String,
}

impl TryFrom<ProviderSyncRow> for ProviderSync {
    type Error = AppError;

    fn try_from(r: ProviderSyncRow) -> AppResult<Self> {
        Ok(ProviderSync {
            status: parse_status(r.status)?,
            id: r.id,
            provider_id: r.provider_id,
            imported: r.imported,
            skipped: r.skipped,
            detail: r.detail,
            created_at: r.created_at,
        })
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn record_sync(
    db: &Db,
    provider_id: i64,
    imported: i64,
    skipped: i64,
    status: SyncOutcome,
    detail: Option<&str>,
) -> AppResult<ProviderSync> {
    let status = status.as_str();
    sqlx::query_as!(
        ProviderSyncRow,
        r#"INSERT INTO provider_syncs (provider_id, imported, skipped, status, detail)
           VALUES (?1,?2,?3,?4,?5)
           RETURNING id AS "id!", provider_id, imported, skipped, status, detail, created_at"#,
        provider_id,
        imported,
        skipped,
        status,
        detail
    )
    .fetch_one(db)
    .await?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_syncs(db: &Db, provider_id: i64) -> AppResult<Vec<ProviderSync>> {
    sqlx::query_as!(
        ProviderSyncRow,
        r#"SELECT id AS "id!", provider_id, imported, skipped, status, detail, created_at
             FROM provider_syncs WHERE provider_id=?1 ORDER BY id DESC"#,
        provider_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(ProviderSync::try_from)
    .collect()
}

// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
// (CLAUDE.md rule 2's escape hatch) — every other arm above is exhaustive over our own
// types.
#[allow(clippy::wildcard_enum_match_arm)]
fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account does not exist")
        }
        other => AppError::from(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::str::FromStr;
    use sure_core::AccountKind;

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    /// An input a *person* could have submitted: complete enough for the account form's
    /// rules, so the same helper works whether a test links it or creates it outright.
    fn new_account_input(name: &str) -> sure_core::SaveAccount {
        sure_core::SaveAccount {
            name: name.to_string(),
            kind: AccountKind::Bank,
            currency_code: "NZD".to_string(),
            institution: Some("ANZ".to_string()),
            metadata: None,
            archived: false,
            sort_order: 0,
            opening_balance_minor: Some(0),
            opening_balance_date: Some("2020-01-01".to_string()),
            // These tests don't care who owns the account; joint needs no person row.
            ownership: sure_core::Ownership::Joint,
        }
    }

    /// What the link path actually receives: a name, a kind and a currency, because that is
    /// all an upstream feed reports — no institution, no metadata, no opening balance.
    fn discovered_account_input(name: &str) -> sure_core::SaveAccount {
        sure_core::SaveAccount {
            name: name.to_string(),
            kind: AccountKind::Bank,
            currency_code: "NZD".to_string(),
            institution: None,
            metadata: None,
            archived: false,
            sort_order: 0,
            opening_balance_minor: None,
            opening_balance_date: None,
            // These tests don't care who owns the account; joint needs no person row.
            ownership: sure_core::Ownership::Joint,
        }
    }

    #[tokio::test]
    async fn links_a_new_account() {
        let db = test_db().await;
        let provider = link(
            &db,
            LinkProviderAccount {
                kind: "akahu".to_string(),
                external_id: "acc_123".to_string(),
                name: "Everyday".to_string(),
                new_account: Some(new_account_input("Everyday")),
                existing_account_id: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(provider.kind, "akahu");
        assert_eq!(provider.config["external_account_id"], "acc_123");

        let account = crate::accounts::get(&db, provider.account_id)
            .await
            .unwrap();
        assert_eq!(account.name, "Everyday");
    }

    /// The link path runs `ValidationMode::Linked`, which asks only for what a feed can
    /// know. A bank account created by hand needs an institution and an opening balance;
    /// a discovered one supplies neither, and must still link — and still read back.
    #[tokio::test]
    async fn links_an_account_a_feed_could_only_partly_describe() {
        let db = test_db().await;
        let provider = link(
            &db,
            LinkProviderAccount {
                kind: "akahu".to_string(),
                external_id: "acc_789".to_string(),
                name: "Everyday".to_string(),
                new_account: Some(discovered_account_input("Everyday")),
                existing_account_id: None,
            },
        )
        .await
        .unwrap();

        let account = crate::accounts::get(&db, provider.account_id)
            .await
            .unwrap();
        assert_eq!(account.institution, None);
        assert!(matches!(
            account.metadata,
            sure_core::AccountMetadata::Depository(_)
        ));
    }

    #[tokio::test]
    async fn links_an_existing_account() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Savings"))
            .await
            .unwrap();

        let provider = link(
            &db,
            LinkProviderAccount {
                kind: "akahu".to_string(),
                external_id: "acc_456".to_string(),
                name: "Savings feed".to_string(),
                new_account: None,
                existing_account_id: Some(account.id),
            },
        )
        .await
        .unwrap();

        assert_eq!(provider.account_id, account.id);
        // No second account was created.
        assert_eq!(list(&db).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejects_neither_target_set() {
        let db = test_db().await;
        let result = link(
            &db,
            LinkProviderAccount {
                kind: "akahu".to_string(),
                external_id: "acc_1".to_string(),
                name: "x".to_string(),
                new_account: None,
                existing_account_id: None,
            },
        )
        .await;
        assert!(matches!(result, Err(AppError::Validation(_))));
    }

    #[tokio::test]
    async fn rejects_both_targets_set() {
        let db = test_db().await;
        let result = link(
            &db,
            LinkProviderAccount {
                kind: "akahu".to_string(),
                external_id: "acc_1".to_string(),
                name: "x".to_string(),
                new_account: Some(new_account_input("x")),
                existing_account_id: Some(1),
            },
        )
        .await;
        assert!(matches!(result, Err(AppError::Validation(_))));
    }

    #[tokio::test]
    async fn rejects_a_missing_existing_account() {
        let db = test_db().await;
        let result = link(
            &db,
            LinkProviderAccount {
                kind: "akahu".to_string(),
                external_id: "acc_1".to_string(),
                name: "x".to_string(),
                new_account: None,
                existing_account_id: Some(999),
            },
        )
        .await;
        assert!(matches!(result, Err(AppError::NotFound(_))));
    }

    fn brokerage_account_input(name: &str) -> sure_core::SaveAccount {
        sure_core::SaveAccount {
            name: name.to_string(),
            kind: AccountKind::Brokerage,
            currency_code: "NZD".to_string(),
            institution: Some("Sharesies".to_string()),
            metadata: Some(sure_core::AccountMetadata::Brokerage(
                sure_core::BrokerageMeta {
                    broker: Some("Sharesies".to_string()),
                    ..Default::default()
                },
            )),
            archived: false,
            sort_order: 0,
            // The one kind with no opening balance — its value comes from the holdings ledger.
            opening_balance_minor: None,
            opening_balance_date: None,
            // These tests don't care who owns the account; joint needs no person row.
            ownership: sure_core::Ownership::Joint,
        }
    }

    #[tokio::test]
    async fn link_group_creates_one_account_and_a_provider_per_member() {
        let db = test_db().await;
        let providers = link_group(
            &db,
            LinkProviderGroup {
                kind: "akahu".to_string(),
                members: vec![
                    LinkGroupMember {
                        external_id: "acc_nzd".to_string(),
                        name: "NZD Wallet".to_string(),
                    },
                    LinkGroupMember {
                        external_id: "acc_usd".to_string(),
                        name: "USD Wallet".to_string(),
                    },
                    LinkGroupMember {
                        external_id: "acc_aud".to_string(),
                        name: "AUD Wallet".to_string(),
                    },
                ],
                new_account: Some(brokerage_account_input("Sharesies")),
                existing_account_id: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(providers.len(), 3);
        // All three provider rows point at the same, single new account.
        let account_id = providers[0].account_id;
        assert!(providers.iter().all(|p| p.account_id == account_id));
        let accounts = crate::accounts::list(&db, false).await.unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].kind, AccountKind::Brokerage);
        assert_eq!(
            providers
                .iter()
                .map(|p| p.config["external_account_id"]
                    .as_str()
                    .unwrap()
                    .to_string())
                .collect::<Vec<_>>(),
            vec!["acc_nzd", "acc_usd", "acc_aud"],
        );
    }

    #[tokio::test]
    async fn link_group_attaches_all_members_to_an_existing_account() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, brokerage_account_input("Sharesies"))
            .await
            .unwrap();
        let providers = link_group(
            &db,
            LinkProviderGroup {
                kind: "akahu".to_string(),
                members: vec![
                    LinkGroupMember {
                        external_id: "acc_nzd".to_string(),
                        name: "NZD Wallet".to_string(),
                    },
                    LinkGroupMember {
                        external_id: "acc_usd".to_string(),
                        name: "USD Wallet".to_string(),
                    },
                ],
                new_account: None,
                existing_account_id: Some(account.id),
            },
        )
        .await
        .unwrap();
        assert_eq!(providers.len(), 2);
        assert!(providers.iter().all(|p| p.account_id == account.id));
        assert_eq!(crate::accounts::list(&db, false).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn link_group_rejects_an_empty_member_list() {
        let db = test_db().await;
        let result = link_group(
            &db,
            LinkProviderGroup {
                kind: "akahu".to_string(),
                members: vec![],
                new_account: Some(brokerage_account_input("Sharesies")),
                existing_account_id: None,
            },
        )
        .await;
        assert!(matches!(result, Err(AppError::Validation(_))));
    }

    async fn imported_row(db: &Db, external_id: &str) -> sure_core::Transaction {
        sqlx::query_as!(
            crate::transactions::TransactionRow,
            r#"SELECT id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                      merchant, merchant_id, notes, category_id, is_one_off AS "is_one_off!: bool",
                      linked_transaction_id, provider, external_id, categorized_by_rule_id,
                      ownership, person_id, created_at, updated_at
                 FROM transactions WHERE external_id = ?1"#,
            external_id
        )
        .fetch_one(db)
        .await
        .unwrap()
        .try_into()
        .unwrap()
    }

    fn enriched_row(external_id: &str, merchant: &str, category: &str, group: &str) -> ImportRow {
        ImportRow {
            external_id: external_id.to_string(),
            posted_at: "2026-01-05T09:30:00+00:00".to_string(),
            amount_minor: -450,
            currency_code: None,
            description: "Flat White".to_string(),
            merchant: Some(merchant.to_string()),
            category_name: Some(category.to_string()),
            category_group: Some(group.to_string()),
            category_kind: None,
            is_one_off: false,
        }
    }

    #[tokio::test]
    async fn import_finds_or_creates_a_nested_category_and_merchant() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Everyday"))
            .await
            .unwrap();

        import_transactions(
            &db,
            account.id,
            "NZD",
            "akahu#1",
            &[enriched_row(
                "trans_1",
                "The Roastery",
                "Cafes and restaurants",
                "Lifestyle",
            )],
        )
        .await
        .unwrap();

        let txn = imported_row(&db, "trans_1").await;
        let merchant_id = txn.merchant_id.expect("merchant should be resolved");
        let category_id = txn.category_id.expect("category should be resolved");

        let cats = crate::categories::list(&db).await.unwrap();
        let category = cats.iter().find(|c| c.id == category_id).unwrap();
        assert_eq!(category.name, "Cafes and restaurants");
        let parent_id = category
            .parent_id
            .expect("category should be nested under a group");
        let group = cats.iter().find(|c| c.id == parent_id).unwrap();
        assert_eq!(group.name, "Lifestyle");

        let merchant = crate::merchants::list(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.id == merchant_id)
            .unwrap();
        assert_eq!(merchant.name, "The Roastery");
        // A newly-seen merchant gets the imported category as its suggested default.
        assert_eq!(merchant.category_id, Some(category_id));
    }

    #[tokio::test]
    async fn import_reuses_an_existing_category_case_insensitively() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Everyday"))
            .await
            .unwrap();
        let existing = crate::categories::create(
            &db,
            sure_core::SaveCategory {
                name: "cafes and restaurants".to_string(),
                parent_id: None,
                kind: sure_core::CategoryKind::Expense,
                color: None,
                icon: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        import_transactions(
            &db,
            account.id,
            "NZD",
            "akahu#1",
            // No group — the existing category is top-level, so it must match on
            // (name, parent) to be reused rather than creating a duplicate.
            &[ImportRow {
                external_id: "trans_2".to_string(),
                posted_at: "2026-01-05T09:30:00+00:00".to_string(),
                amount_minor: -450,
                currency_code: None,
                description: "Flat White".to_string(),
                merchant: Some("The Roastery".to_string()),
                category_name: Some("Cafes And Restaurants".to_string()),
                category_group: None,
                category_kind: None,
                is_one_off: false,
            }],
        )
        .await
        .unwrap();

        let txn = imported_row(&db, "trans_2").await;
        assert_eq!(txn.category_id, Some(existing.id));
        // No duplicate was created for the differently-cased name.
        //
        // Counted among *top-level* categories only, which is what the contract above actually says:
        // a category is identified by (name, parent), so a same-named one nested under a group is a
        // different category and not a duplicate of this one. The default-rules migration seeds
        // exactly that — "Cafes and restaurants" under "Lifestyle" — and a name-only count started
        // finding two, reporting a duplicate that was never created.
        let matches = crate::categories::list(&db)
            .await
            .unwrap()
            .into_iter()
            .filter(|c| {
                c.parent_id.is_none() && c.name.eq_ignore_ascii_case("cafes and restaurants")
            })
            .count();
        assert_eq!(matches, 1);
    }

    /// A plain row with no enrichment: what a chunking test needs many of.
    fn plain_row(external_id: &str, currency_code: Option<&str>) -> ImportRow {
        ImportRow {
            external_id: external_id.to_string(),
            posted_at: "2026-01-05T09:30:00+00:00".to_string(),
            amount_minor: -125,
            currency_code: currency_code.map(str::to_string),
            description: "Bus fare".to_string(),
            merchant: None,
            category_name: None,
            category_group: None,
            category_kind: None,
            is_one_off: false,
        }
    }

    /// W-30: an import spanning several chunks writes every row, still dedupes, and reports
    /// the same (imported, skipped) counts row-at-a-time inserts did — the batching must be
    /// invisible in the result and visible only in the number of transactions.
    #[tokio::test]
    async fn a_multi_chunk_import_writes_every_row_and_still_dedupes() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Everyday"))
            .await
            .unwrap();
        // Two full chunks and a partial one, so the last (short) `push_values` batch is
        // exercised alongside the full ones.
        let count = IMPORT_CHUNK_ROWS * 2 + 7;
        let rows: Vec<ImportRow> = (0..count)
            .map(|i| plain_row(&format!("chunked_{i}"), None))
            .collect();

        let (imported, skipped) = import_transactions(&db, account.id, "NZD", "akahu#1", &rows)
            .await
            .unwrap();
        assert_eq!((imported, skipped), (count as i64, 0));
        let stored = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM transactions WHERE provider=?1",
            "akahu#1"
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(stored, count as i64);

        // Re-importing the same export is still idempotent: `INSERT OR IGNORE` dedupes
        // inside a multi-row statement exactly as it did one row at a time.
        let (imported, skipped) = import_transactions(&db, account.id, "NZD", "akahu#1", &rows)
            .await
            .unwrap();
        assert_eq!((imported, skipped), (0, count as i64));
    }

    /// The chunk is the failure boundary: one transaction per chunk means a chunk that
    /// cannot be written rolls back whole, and the chunks already committed stay committed.
    /// (Which is safe precisely because a re-run dedupes — see the test above.)
    #[tokio::test]
    async fn a_failing_chunk_rolls_back_without_taking_the_earlier_ones_with_it() {
        // Foreign keys must actually be enforced for this, as they are in `connect`; the
        // shared `test_db` helper leaves SQLite's default (off).
        let options = sqlx::sqlite::SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .foreign_keys(true);
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        crate::migrate(&db).await.unwrap();
        let account = crate::accounts::create(&db, new_account_input("Everyday"))
            .await
            .unwrap();

        // A full first chunk, then a second chunk whose fifth row names a currency that does
        // not exist — an FK violation, which `OR IGNORE` does not suppress.
        let mut rows: Vec<ImportRow> = (0..IMPORT_CHUNK_ROWS)
            .map(|i| plain_row(&format!("good_{i}"), None))
            .collect();
        rows.extend((0..10).map(|i| plain_row(&format!("second_{i}"), None)));
        rows[IMPORT_CHUNK_ROWS + 5] = plain_row("second_5", Some("ZZZ"));

        let err = import_transactions(&db, account.id, "NZD", "akahu#1", &rows)
            .await
            .expect_err("a currency that does not exist cannot be written");
        // Not overload — a genuine constraint failure, and it must not be laundered into a
        // retryable 503.
        assert!(!err.is_overloaded(), "{err}");

        let committed = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM transactions WHERE external_id LIKE 'good_%'"
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            committed, IMPORT_CHUNK_ROWS as i64,
            "the chunk that succeeded should have committed"
        );
        let from_failed_chunk = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM transactions WHERE external_id LIKE 'second_%'"
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            from_failed_chunk, 0,
            "the failing chunk must roll back whole, not leave its first four rows behind"
        );
    }

    #[tokio::test]
    async fn import_does_not_overwrite_an_existing_merchants_category() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Everyday"))
            .await
            .unwrap();
        let user_category = crate::categories::create(
            &db,
            sure_core::SaveCategory {
                name: "My Own Coffee Budget".to_string(),
                parent_id: None,
                kind: sure_core::CategoryKind::Expense,
                color: None,
                icon: None,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        crate::merchants::create(
            &db,
            sure_core::SaveMerchant {
                name: "The Roastery".to_string(),
                category_id: Some(user_category.id),
                note: None,
            },
        )
        .await
        .unwrap();

        import_transactions(
            &db,
            account.id,
            "NZD",
            "akahu#1",
            &[enriched_row(
                "trans_3",
                "The Roastery",
                "Cafes and restaurants",
                "Lifestyle",
            )],
        )
        .await
        .unwrap();

        let merchant = crate::merchants::list(&db)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.name == "The Roastery")
            .unwrap();
        // The user's own default category choice is untouched...
        assert_eq!(merchant.category_id, Some(user_category.id));
        // ...even though this transaction is still categorized from the import.
        let txn = imported_row(&db, "trans_3").await;
        assert_ne!(txn.category_id, Some(user_category.id));
        assert!(txn.category_id.is_some());
    }
}
