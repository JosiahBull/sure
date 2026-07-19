use std::collections::HashMap;

use serde_json::json;
use sqlx::FromRow;
use sure_core::{AppError, AppResult};
pub use sure_core::{
    LinkGroupMember, LinkProviderAccount, LinkProviderGroup, Provider, ProviderSync, SaveProvider,
    SyncRequest,
};

use crate::accounts::AccountRow;
use crate::Db;

/// New categories created from a provider's own classification default to "expense" —
/// most enrichment is spend-side (Akahu's NZFCC categories have no income group). A row
/// can override this via `ImportRow::category_kind` (a broker's dividend row → "income",
/// an internal wallet ↔ bank movement → "transfer").
const IMPORTED_CATEGORY_KIND: &str = "expense";

#[tracing::instrument(level = "debug", skip_all)]
async fn resolve_category(
    db: &Db,
    category_cache: &mut HashMap<(String, Option<String>), i64>,
    group_cache: &mut HashMap<String, i64>,
    name: &str,
    group: Option<&str>,
    kind: &str,
) -> AppResult<i64> {
    let key = (name.to_string(), group.map(str::to_string));
    if let Some(&id) = category_cache.get(&key) {
        return Ok(id);
    }
    let parent_id = match group {
        Some(g) => Some(match group_cache.get(g) {
            Some(&id) => id,
            None => {
                let id = crate::categories::find_or_create(db, g, None, kind).await?.id;
                group_cache.insert(g.to_string(), id);
                id
            }
        }),
        None => None,
    };
    let id = crate::categories::find_or_create(db, name, parent_id, kind).await?.id;
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

#[derive(Debug, FromRow)]
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
    /// Flow direction for a newly-created category (`"income"` | `"expense"` |
    /// `"transfer"`); `None` defaults to expense. Only affects creation — an existing
    /// category keeps its kind.
    pub category_kind: Option<String>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db) -> AppResult<Vec<Provider>> {
    let rows = sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers ORDER BY id")
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(Provider::from).collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<Provider> {
    let row = sqlx::query_as::<_, ProviderRow>("SELECT * FROM providers WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("provider"))?;
    Ok(row.into())
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
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
    // fetching outside the transaction); it's a plain lookup, not a mutation.
    let new_account_metadata = match &input.new_account {
        Some(a) => Some(crate::accounts::validate(db, a).await?),
        None => None,
    };

    let mut tx = db.begin().await?;

    let account_id = if let (Some(new_account), Some(metadata)) =
        (&input.new_account, &new_account_metadata)
    {
        let row = sqlx::query_as::<_, AccountRow>(
            "INSERT INTO accounts (name, kind, currency_code, institution, metadata, archived, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING *",
        )
        .bind(new_account.name.trim())
        .bind(new_account.kind)
        .bind(new_account.currency_code.trim().to_uppercase())
        .bind(&new_account.institution)
        .bind(metadata)
        .bind(new_account.archived)
        .bind(new_account.sort_order)
        .fetch_one(&mut *tx)
        .await?;
        row.id
    } else {
        let id = input
            .existing_account_id
            .expect("validated exactly-one above");
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts WHERE id=?1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if exists == 0 {
            return Err(AppError::NotFound("account"));
        }
        id
    };

    let config = json!({ "external_account_id": input.external_id }).to_string();
    let row = sqlx::query_as::<_, ProviderRow>(
        "INSERT INTO providers (name, kind, account_id, config, enabled)
         VALUES (?1,?2,?3,?4,1) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(&input.kind)
    .bind(account_id)
    .bind(config)
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
        return Err(AppError::validation("link group requires at least one member"));
    }

    let new_account_metadata = match &input.new_account {
        Some(a) => Some(crate::accounts::validate(db, a).await?),
        None => None,
    };

    let mut tx = db.begin().await?;

    let account_id = if let (Some(new_account), Some(metadata)) =
        (&input.new_account, &new_account_metadata)
    {
        let row = sqlx::query_as::<_, AccountRow>(
            "INSERT INTO accounts (name, kind, currency_code, institution, metadata, archived, sort_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING *",
        )
        .bind(new_account.name.trim())
        .bind(new_account.kind)
        .bind(new_account.currency_code.trim().to_uppercase())
        .bind(&new_account.institution)
        .bind(metadata)
        .bind(new_account.archived)
        .bind(new_account.sort_order)
        .fetch_one(&mut *tx)
        .await?;
        row.id
    } else {
        let id = input.existing_account_id.expect("validated exactly-one above");
        let exists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM accounts WHERE id=?1")
            .bind(id)
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
        let row = sqlx::query_as::<_, ProviderRow>(
            "INSERT INTO providers (name, kind, account_id, config, enabled)
             VALUES (?1,?2,?3,?4,1) RETURNING *",
        )
        .bind(member.name.trim())
        .bind(&input.kind)
        .bind(account_id)
        .bind(config)
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
    let res = sqlx::query("DELETE FROM providers WHERE id=?1")
        .bind(id)
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
        sqlx::query_scalar::<_, String>("SELECT currency_code FROM accounts WHERE id=?1")
            .bind(account_id)
            .fetch_one(db)
            .await?,
    )
}

/// Insert fetched transactions, deduping on (provider, external_id). Reuses (or creates)
/// a matching merchant/category per row from any source-supplied classification, so
/// providers that carry their own enrichment (e.g. Akahu's NZFCC categories) don't leave
/// every imported transaction uncategorized. Returns (imported, skipped).
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

    for t in rows {
        let ccy = t
            .currency_code
            .clone()
            .unwrap_or_else(|| account_currency.to_string());

        let category_id = match &t.category_name {
            Some(name) if !name.trim().is_empty() => Some(
                resolve_category(
                    db,
                    &mut category_cache,
                    &mut group_cache,
                    name,
                    t.category_group.as_deref(),
                    t.category_kind.as_deref().unwrap_or(IMPORTED_CATEGORY_KIND),
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

        let res = sqlx::query(
            "INSERT OR IGNORE INTO transactions
                (account_id, posted_at, amount_minor, currency_code, description, merchant,
                 merchant_id, category_id, provider, external_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        )
        .bind(account_id)
        .bind(&t.posted_at)
        .bind(t.amount_minor)
        .bind(&ccy)
        .bind(&t.description)
        .bind(&t.merchant)
        .bind(merchant_id)
        .bind(category_id)
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

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update_last_synced(db: &Db, id: i64) -> AppResult<()> {
    sqlx::query(
        "UPDATE providers SET last_synced_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
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

    fn new_account_input(name: &str) -> sure_core::SaveAccount {
        sure_core::SaveAccount {
            name: name.to_string(),
            kind: AccountKind::Bank,
            currency_code: "NZD".to_string(),
            institution: None,
            metadata: None,
            archived: false,
            sort_order: 0,
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

        let account = crate::accounts::get(&db, provider.account_id).await.unwrap();
        assert_eq!(account.name, "Everyday");
    }

    #[tokio::test]
    async fn links_an_existing_account() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Savings")).await.unwrap();

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
            metadata: None,
            archived: false,
            sort_order: 0,
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
                    LinkGroupMember { external_id: "acc_nzd".to_string(), name: "NZD Wallet".to_string() },
                    LinkGroupMember { external_id: "acc_usd".to_string(), name: "USD Wallet".to_string() },
                    LinkGroupMember { external_id: "acc_aud".to_string(), name: "AUD Wallet".to_string() },
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
            providers.iter().map(|p| p.config["external_account_id"].as_str().unwrap().to_string()).collect::<Vec<_>>(),
            vec!["acc_nzd", "acc_usd", "acc_aud"],
        );
    }

    #[tokio::test]
    async fn link_group_attaches_all_members_to_an_existing_account() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, brokerage_account_input("Sharesies")).await.unwrap();
        let providers = link_group(
            &db,
            LinkProviderGroup {
                kind: "akahu".to_string(),
                members: vec![
                    LinkGroupMember { external_id: "acc_nzd".to_string(), name: "NZD Wallet".to_string() },
                    LinkGroupMember { external_id: "acc_usd".to_string(), name: "USD Wallet".to_string() },
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
        sqlx::query_as::<_, sure_core::Transaction>(
            "SELECT * FROM transactions WHERE external_id = ?1",
        )
        .bind(external_id)
        .fetch_one(db)
        .await
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
        }
    }

    #[tokio::test]
    async fn import_finds_or_creates_a_nested_category_and_merchant() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Everyday")).await.unwrap();

        import_transactions(
            &db,
            account.id,
            "NZD",
            "akahu#1",
            &[enriched_row("trans_1", "The Roastery", "Cafes and restaurants", "Lifestyle")],
        )
        .await
        .unwrap();

        let txn = imported_row(&db, "trans_1").await;
        let merchant_id = txn.merchant_id.expect("merchant should be resolved");
        let category_id = txn.category_id.expect("category should be resolved");

        let cats = crate::categories::list(&db).await.unwrap();
        let category = cats.iter().find(|c| c.id == category_id).unwrap();
        assert_eq!(category.name, "Cafes and restaurants");
        let parent_id = category.parent_id.expect("category should be nested under a group");
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
        let account = crate::accounts::create(&db, new_account_input("Everyday")).await.unwrap();
        let existing = crate::categories::create(
            &db,
            sure_core::SaveCategory {
                name: "cafes and restaurants".to_string(),
                parent_id: None,
                kind: "expense".to_string(),
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
            }],
        )
        .await
        .unwrap();

        let txn = imported_row(&db, "trans_2").await;
        assert_eq!(txn.category_id, Some(existing.id));
        // No duplicate was created for the differently-cased name.
        let matches = crate::categories::list(&db)
            .await
            .unwrap()
            .into_iter()
            .filter(|c| c.name.eq_ignore_ascii_case("cafes and restaurants"))
            .count();
        assert_eq!(matches, 1);
    }

    #[tokio::test]
    async fn import_does_not_overwrite_an_existing_merchants_category() {
        let db = test_db().await;
        let account = crate::accounts::create(&db, new_account_input("Everyday")).await.unwrap();
        let user_category = crate::categories::create(
            &db,
            sure_core::SaveCategory {
                name: "My Own Coffee Budget".to_string(),
                parent_id: None,
                kind: "expense".to_string(),
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
            &[enriched_row("trans_3", "The Roastery", "Cafes and restaurants", "Lifestyle")],
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
