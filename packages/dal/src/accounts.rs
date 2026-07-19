use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
pub use sure_core::{Account, SaveAccount, SetSecuredBy};
use sure_core::{AccountKind, AccountMetadata, AppError, AppResult};

use crate::Db;

/// Decode stored account metadata for a `kind`, coercing the `profile` discriminant to
/// the one the kind requires. This lets legacy `{}` rows, a hand-edited blob, or a
/// changed account kind still decode as the correct variant; anything unrecognised
/// falls back to an empty value for the kind.
fn metadata_from_stored(kind: AccountKind, stored: &str) -> AccountMetadata {
    let expected = AccountMetadata::profile_for(kind);
    let mut value: Value = serde_json::from_str(stored).unwrap_or_else(|_| json!({}));
    match value {
        Value::Object(ref mut map) => {
            map.insert("profile".into(), Value::String(expected.to_string()));
        }
        _ => value = json!({ "profile": expected }),
    }
    serde_json::from_value(value).unwrap_or_else(|_| AccountMetadata::default_for(kind))
}

#[derive(Debug, FromRow)]
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

impl From<AccountRow> for Account {
    fn from(r: AccountRow) -> Self {
        Account {
            class: r.kind.class(),
            metadata: metadata_from_stored(r.kind, &r.metadata),
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

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub include_archived: Option<bool>,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list(db: &Db, include_archived: bool) -> AppResult<Vec<Account>> {
    let sql = if include_archived {
        "SELECT * FROM accounts ORDER BY sort_order, name"
    } else {
        "SELECT * FROM accounts WHERE archived = 0 ORDER BY sort_order, name"
    };
    let rows = sqlx::query_as::<_, AccountRow>(sql).fetch_all(db).await?;
    Ok(rows.into_iter().map(Account::from).collect())
}

/// A distinct ticker/exchange pair in use by a `shares_nz`/`shares_us` account, for
/// keeping the stock price cache warm (see `sure_api::stock_prices::StockPriceTask`).
/// `shares_private` holdings are excluded — there's no market ticker to fetch a price
/// for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SharesTicker {
    pub ticker: String,
    pub exchange: String,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_shares_tickers(db: &Db) -> AppResult<Vec<SharesTicker>> {
    let accounts = list(db, false).await?;
    let tickers: std::collections::HashSet<SharesTicker> = accounts
        .into_iter()
        .filter(|a| matches!(a.kind, AccountKind::SharesNz | AccountKind::SharesUs))
        .filter_map(|a| {
            let AccountMetadata::Shares(meta) = a.metadata else {
                return None;
            };
            let ticker = meta.ticker?.trim().to_uppercase();
            if ticker.is_empty() {
                return None;
            }
            Some(SharesTicker {
                ticker,
                exchange: meta.exchange.unwrap_or_default().trim().to_string(),
            })
        })
        .collect();
    Ok(tickers.into_iter().collect())
}

/// Distinct `(ticker, exchange)` pairs ever traded on any brokerage account's holdings
/// ledger — the multi-holding counterpart to [`list_shares_tickers`], used by the
/// stock-price poller to keep every held ticker's price cache warm.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_brokerage_tickers(db: &Db) -> AppResult<Vec<SharesTicker>> {
    let rows =
        sqlx::query_as::<_, (String, String)>("SELECT DISTINCT ticker, exchange FROM holdings")
            .fetch_all(db)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(ticker, exchange)| {
            let ticker = ticker.trim().to_uppercase();
            if ticker.is_empty() {
                return None;
            }
            Some(SharesTicker {
                ticker,
                exchange: exchange.trim().to_string(),
            })
        })
        .collect())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db, id: i64) -> AppResult<Account> {
    let row = sqlx::query_as::<_, AccountRow>("SELECT * FROM accounts WHERE id = ?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("account"))?;
    Ok(row.into())
}

/// Validate input and return the metadata JSON to persist (as a string).
pub(crate) async fn validate(db: &Db, input: &SaveAccount) -> AppResult<String> {
    if input.name.trim().is_empty() {
        return Err(AppError::validation("account name is required"));
    }
    let currency = input.currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &currency).await? {
        return Err(AppError::validation(format!(
            "unknown currency '{currency}'"
        )));
    }
    let expected = AccountMetadata::profile_for(input.kind);
    let metadata = match &input.metadata {
        Some(m) if m.profile() != expected => {
            return Err(AppError::validation(format!(
                "metadata profile '{}' does not match account kind (expected '{expected}')",
                m.profile()
            )));
        }
        Some(m) => m.clone(),
        None => AccountMetadata::default_for(input.kind),
    };
    serde_json::to_string(&metadata).map_err(|e| AppError::Internal(e.into()))
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveAccount) -> AppResult<Account> {
    let metadata = validate(db, &input).await?;
    let mut tx = db.begin().await?;
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
    .fetch_one(&mut *tx)
    .await?;
    let account: Account = row.into();

    // A property's purchase price/date *is* an initial valuation — seed one so
    // net-worth/equity calculations (which only ever read the `valuations` table, never
    // metadata directly) have a real starting value from day one instead of reading as
    // $0 until the user separately remembers to add one by hand.
    if let AccountMetadata::Property(ref p) = account.metadata {
        if let (Some(price), Some(date)) = (p.purchase_price_minor, &p.purchase_date) {
            sqlx::query(
                "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
                 VALUES (?1, ?2, ?3, ?4, 'manual', ?5)",
            )
            .bind(account.id)
            .bind(date)
            .bind(price)
            .bind(&account.currency_code)
            .bind("Initial valuation from purchase price")
            .execute(&mut *tx)
            .await?;
        }
    }

    tx.commit().await?;
    Ok(account)
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[tracing::instrument(level = "debug", skip_all)]
pub async fn delete(db: &Db, id: i64) -> AppResult<()> {
    // An asset is a "parent" for the debts secured against it. Refuse to delete it while
    // any remain, so we never silently orphan them — the caller must unlink or delete
    // those debts first.
    let dependents = sqlx::query_scalar::<_, String>(
        "SELECT name FROM accounts WHERE secured_by_account_id = ?1 ORDER BY sort_order, name",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    if !dependents.is_empty() {
        return Err(AppError::conflict(format!(
            "Unlink or delete the debt secured against this account first: {}",
            dependents.join(", ")
        )));
    }
    let res = sqlx::query("DELETE FROM accounts WHERE id = ?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("account"));
    }
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
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

/// Update just the credit-limit hint on a depository-profile account's metadata (used by
/// providers that can report a live limit, e.g. Akahu's `balance.limit` for a credit
/// card), leaving every other metadata field untouched. A no-op if the account isn't
/// depository-profiled — a mortgage/loan/etc. has no such concept.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn set_credit_limit(db: &Db, account_id: i64, credit_limit_minor: i64) -> AppResult<()> {
    let account = get(db, account_id).await?;
    let AccountMetadata::Depository(mut meta) = account.metadata else {
        return Ok(());
    };
    meta.credit_limit_minor = Some(credit_limit_minor);
    write_metadata(db, account_id, &AccountMetadata::Depository(meta)).await
}

/// Update just the original-borrowed-amount hint on a mortgage/loan account's metadata
/// (used by providers that can report it, e.g. Akahu's `loan_details.initial_principal`,
/// which lets a paid-down percentage be derived from the current balance), leaving every
/// other metadata field untouched. A no-op for any other account kind.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn set_original_amount(
    db: &Db,
    account_id: i64,
    original_amount_minor: i64,
) -> AppResult<()> {
    let account = get(db, account_id).await?;
    let metadata = match account.metadata {
        AccountMetadata::Mortgage(mut meta) => {
            meta.original_amount_minor = Some(original_amount_minor);
            AccountMetadata::Mortgage(meta)
        }
        AccountMetadata::Loan(mut meta) => {
            meta.original_amount_minor = Some(original_amount_minor);
            AccountMetadata::Loan(meta)
        }
        _ => return Ok(()),
    };
    write_metadata(db, account_id, &metadata).await
}

/// Backfill an account's institution from a provider, but only if it doesn't already
/// have one — a user's own edit (e.g. shortening "ASB Bank Limited" to "ASB") is never
/// overwritten by a later sync, unlike the numeric provider-sourced fields above which
/// always refresh to stay in sync with the live source.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn set_institution_if_unset(
    db: &Db,
    account_id: i64,
    institution: &str,
) -> AppResult<()> {
    sqlx::query(
        "UPDATE accounts SET institution=?2, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 AND (institution IS NULL OR institution = '')",
    )
    .bind(account_id)
    .bind(institution)
    .execute(db)
    .await?;
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
async fn write_metadata(db: &Db, account_id: i64, metadata: &AccountMetadata) -> AppResult<()> {
    let json = serde_json::to_string(metadata).map_err(|e| AppError::Internal(e.into()))?;
    sqlx::query(
        "UPDATE accounts SET metadata=?2, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=?1",
    )
    .bind(account_id)
    .bind(json)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{DepositoryMeta, SharesMeta};

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn sets_a_credit_limit_without_touching_other_metadata() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                name: "Visa".to_string(),
                kind: AccountKind::CreditCard,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: Some(AccountMetadata::Depository(DepositoryMeta {
                    account_number: Some("••1234".to_string()),
                    credit_limit_minor: None,
                    url: None,
                    notes: Some("keep me".to_string()),
                })),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        set_credit_limit(&db, account.id, 1_000_000).await.unwrap();

        let updated = get(&db, account.id).await.unwrap();
        let AccountMetadata::Depository(meta) = updated.metadata else {
            panic!("expected depository metadata");
        };
        assert_eq!(meta.credit_limit_minor, Some(1_000_000));
        // Untouched.
        assert_eq!(meta.account_number.as_deref(), Some("••1234"));
        assert_eq!(meta.notes.as_deref(), Some("keep me"));
    }

    #[tokio::test]
    async fn is_a_no_op_for_a_non_depository_account() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                name: "Mortgage".to_string(),
                kind: AccountKind::Mortgage,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        // Must not error and must not somehow turn a mortgage's metadata into
        // depository-shaped data.
        set_credit_limit(&db, account.id, 1_000_000).await.unwrap();
        let updated = get(&db, account.id).await.unwrap();
        assert!(matches!(updated.metadata, AccountMetadata::Mortgage(_)));
    }

    #[tokio::test]
    async fn sets_a_mortgages_original_amount_without_touching_other_metadata() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                name: "Prime Housing Lending".to_string(),
                kind: AccountKind::Mortgage,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: Some(AccountMetadata::Mortgage(sure_core::MortgageMeta {
                    lender: Some("ASB".to_string()),
                    original_amount_minor: None,
                    ..Default::default()
                })),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        set_original_amount(&db, account.id, 48_500_000)
            .await
            .unwrap();

        let updated = get(&db, account.id).await.unwrap();
        let AccountMetadata::Mortgage(meta) = updated.metadata else {
            panic!("expected mortgage metadata");
        };
        assert_eq!(meta.original_amount_minor, Some(48_500_000));
        assert_eq!(meta.lender.as_deref(), Some("ASB")); // untouched
    }

    #[tokio::test]
    async fn sets_a_loans_original_amount_too() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                name: "Student Loan".to_string(),
                kind: AccountKind::StudentLoan,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        set_original_amount(&db, account.id, 3_000_000)
            .await
            .unwrap();

        let updated = get(&db, account.id).await.unwrap();
        let AccountMetadata::Loan(meta) = updated.metadata else {
            panic!("expected loan metadata");
        };
        assert_eq!(meta.original_amount_minor, Some(3_000_000));
    }

    #[tokio::test]
    async fn original_amount_is_a_no_op_for_a_non_loan_account() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                name: "Everyday".to_string(),
                kind: AccountKind::Bank,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        set_original_amount(&db, account.id, 1_000_000)
            .await
            .unwrap();
        let updated = get(&db, account.id).await.unwrap();
        assert!(matches!(updated.metadata, AccountMetadata::Depository(_)));
    }

    #[tokio::test]
    async fn backfills_institution_only_when_unset() {
        let db = test_db().await;
        let no_institution = create(
            &db,
            SaveAccount {
                name: "Everyday".to_string(),
                kind: AccountKind::Bank,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        let has_institution = create(
            &db,
            SaveAccount {
                name: "Savings".to_string(),
                kind: AccountKind::Savings,
                currency_code: "NZD".to_string(),
                institution: Some("My Custom Label".to_string()),
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        set_institution_if_unset(&db, no_institution.id, "ASB")
            .await
            .unwrap();
        set_institution_if_unset(&db, has_institution.id, "ASB")
            .await
            .unwrap();

        assert_eq!(
            get(&db, no_institution.id).await.unwrap().institution,
            Some("ASB".to_string())
        );
        // The user's own label is never overwritten.
        assert_eq!(
            get(&db, has_institution.id).await.unwrap().institution,
            Some("My Custom Label".to_string())
        );
    }

    #[tokio::test]
    async fn creating_a_property_with_a_purchase_price_seeds_its_initial_valuation() {
        let db = test_db().await;
        let house = create(
            &db,
            SaveAccount {
                name: "Family Home".to_string(),
                kind: AccountKind::RealEstate,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: Some(AccountMetadata::Property(sure_core::PropertyMeta {
                    address: Some("12 Rimu Street".to_string()),
                    purchase_date: Some("2025-12-12".to_string()),
                    purchase_price_minor: Some(77_000_000),
                    ..Default::default()
                })),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        let vals = crate::valuations::list_for_account(&db, house.id)
            .await
            .unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].as_of, "2025-12-12");
        assert_eq!(vals[0].value_minor, 77_000_000);
        assert_eq!(vals[0].currency_code, "NZD");
    }

    #[tokio::test]
    async fn a_property_without_a_purchase_price_or_date_gets_no_valuation() {
        let db = test_db().await;
        let house = create(
            &db,
            SaveAccount {
                name: "Family Home".to_string(),
                kind: AccountKind::RealEstate,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        assert!(crate::valuations::list_for_account(&db, house.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn a_non_property_account_never_gets_an_auto_seeded_valuation() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                name: "Everyday".to_string(),
                kind: AccountKind::Bank,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        assert!(crate::valuations::list_for_account(&db, account.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn lists_distinct_tickers_from_market_shares_accounts_only() {
        let db = test_db().await;
        let shares = |ticker: &str, exchange: &str| {
            Some(AccountMetadata::Shares(SharesMeta {
                broker: None,
                ticker: Some(ticker.to_string()),
                exchange: Some(exchange.to_string()),
                url: None,
                notes: None,
            }))
        };
        create(
            &db,
            SaveAccount {
                name: "Meridian".to_string(),
                kind: AccountKind::SharesNz,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: shares("mel", "nzx"),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        // A second account holding the same ticker shouldn't produce a duplicate entry.
        create(
            &db,
            SaveAccount {
                name: "Meridian (also)".to_string(),
                kind: AccountKind::SharesNz,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: shares("mel", "nzx"),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        create(
            &db,
            SaveAccount {
                name: "Apple".to_string(),
                kind: AccountKind::SharesUs,
                currency_code: "USD".to_string(),
                institution: None,
                metadata: shares("aapl", "nasdaq"),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        // Private holdings have no market ticker to fetch a price for.
        create(
            &db,
            SaveAccount {
                name: "Startup equity".to_string(),
                kind: AccountKind::SharesPrivate,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: shares("n/a", ""),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();
        // No ticker set — excluded.
        create(
            &db,
            SaveAccount {
                name: "Undecided holding".to_string(),
                kind: AccountKind::SharesUs,
                currency_code: "USD".to_string(),
                institution: None,
                metadata: None,
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap();

        let mut tickers = list_shares_tickers(&db).await.unwrap();
        tickers.sort_by(|a, b| a.ticker.cmp(&b.ticker));

        assert_eq!(tickers.len(), 2);
        assert_eq!(tickers[0].ticker, "AAPL");
        assert_eq!(tickers[0].exchange, "nasdaq");
        assert_eq!(tickers[1].ticker, "MEL");
        assert_eq!(tickers[1].exchange, "nzx");
    }
}
