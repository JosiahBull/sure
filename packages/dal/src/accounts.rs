use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
pub use sure_core::{Account, SaveAccount, SetSecuredBy};
use sure_core::{
    AccountClass, AccountKind, AccountMetadata, AppError, AppResult, ValidationMode,
    ValuationSource,
};

use crate::Db;

/// Decode stored account metadata for a `kind`, coercing the `profile` discriminant to
/// the one the kind requires. This lets legacy `{}` rows, a hand-edited blob, or a
/// changed account kind still decode as the correct variant; anything unrecognised
/// falls back to an empty value for the kind.
fn metadata_from_stored(kind: AccountKind, stored: &str) -> AccountMetadata {
    let expected = AccountMetadata::profile_for(kind);
    let mut value: Value = serde_json::from_str(stored).unwrap_or_else(|_| json!({}));
    // Not a `match` (`serde_json::Value` has a closed, non-`#[non_exhaustive]` set of
    // variants, but "is it an object" is the only distinction that matters here) — an
    // `if let`/`else` says exactly that without a wildcard arm to justify.
    if let Value::Object(ref mut map) = value {
        map.insert("profile".into(), Value::String(expected.to_string()));
    } else {
        value = json!({ "profile": expected });
    }
    serde_json::from_value(value).unwrap_or_else(|_| AccountMetadata::default_for(kind))
}

#[derive(Debug, FromRow)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
    pub institution: Option<String>,
    pub metadata: String,
    pub archived: bool,
    pub sort_order: i64,
    pub secured_by_account_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl TryFrom<AccountRow> for Account {
    type Error = AppError;

    fn try_from(r: AccountRow) -> AppResult<Self> {
        // The column has no CHECK constraint (sqlite's is limited), but every writer
        // goes through `AccountKind::as_str`, so a value that doesn't parse means the
        // row was written by something else entirely — surface it as a real error
        // rather than panicking the request.
        let kind: AccountKind = r
            .kind
            .parse()
            .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(Account {
            class: kind.class(),
            metadata: metadata_from_stored(kind, &r.metadata),
            id: r.id,
            name: r.name,
            kind,
            currency_code: r.currency_code,
            institution: r.institution,
            archived: r.archived,
            sort_order: r.sort_order,
            secured_by_account_id: r.secured_by_account_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
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
    rows.into_iter().map(Account::try_from).collect()
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
    row.try_into()
}

/// Which write is being validated. [`ValidationMode`] draws the human-vs-provider line for
/// *metadata*; this adds the create-vs-update distinction the opening-balance rules need,
/// so a caller passes one value instead of two flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Write {
    /// `POST /api/accounts`.
    Create,
    /// `PUT /api/accounts/{id}`.
    Update,
    /// The provider-link path (always a create) — see [`ValidationMode::Linked`].
    Linked,
}

impl Write {
    fn mode(self) -> ValidationMode {
        match self {
            Write::Linked => ValidationMode::Linked,
            Write::Create | Write::Update => ValidationMode::Manual,
        }
    }
}

/// The kinds where the institution is part of identifying the account at all: two cards
/// both called "Visa" are told apart by their bank. Cash in hand has no institution, and
/// every other kind names its counterparty in metadata instead (`lender`, `broker`).
const INSTITUTION_REQUIRED: &[AccountKind] = &[
    AccountKind::Bank,
    AccountKind::Savings,
    AccountKind::CreditCard,
    AccountKind::RevolvingCredit,
];

/// Validate input and return the metadata JSON to persist (as a string).
///
/// Problems are gathered rather than returned one at a time: a half-filled form comes back
/// as a single 422 naming every field it still needs, not one per round trip. That includes
/// the name/currency/profile checks — a blank name and a missing metadata field are both
/// reported together, not one 422 per round trip.
pub(crate) async fn validate(db: &Db, input: &SaveAccount, write: Write) -> AppResult<String> {
    let mut problems = Vec::new();

    if input.name.trim().is_empty() {
        problems.push("account name is required".to_string());
    }

    let currency = input.currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &currency).await? {
        problems.push(format!("unknown currency '{currency}'"));
    }

    let expected = AccountMetadata::profile_for(input.kind);
    let metadata = match &input.metadata {
        Some(m) if m.profile() != expected => {
            problems.push(format!(
                "metadata profile '{}' does not match account kind (expected '{expected}')",
                m.profile()
            ));
            // The fields on a mismatched profile don't describe this kind at all, so there's
            // nothing meaningful to check them against — fall back to an empty value for the
            // *right* profile so the required-field checks below still run and the caller
            // hears about everything else wrong with the save too, not just the profile.
            AccountMetadata::default_for(input.kind)
        }
        Some(m) => m.clone(),
        None => AccountMetadata::default_for(input.kind),
    };

    problems.extend(
        metadata
            .validate_for(input.kind, write.mode())
            .err()
            .unwrap_or_default(),
    );
    if write.mode() == ValidationMode::Manual
        && INSTITUTION_REQUIRED.contains(&input.kind)
        && input
            .institution
            .as_deref()
            .is_none_or(|i| i.trim().is_empty())
    {
        problems.push("institution is required".to_string());
    }
    problems.extend(opening_balance_problems(input, write));
    if !problems.is_empty() {
        return Err(AppError::validation(problems.join("; ")));
    }

    serde_json::to_string(&metadata).map_err(|e| AppError::Internal(e.into()))
}

/// The opening balance's date, trimmed, treating blank as absent.
fn opening_balance_date(input: &SaveAccount) -> Option<&str> {
    input
        .opening_balance_date
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
}

/// Whether `s` is the ISO-8601 date `opening_balance_date` is documented to be. It's bound
/// straight into `transactions.posted_at` / `valuations.as_of`, and every report reads dates
/// back with the same `%Y-%m-%d` shape (see `sure_app::reports::parse_date`), so a value in
/// any other format — `31/07/2026`, a datetime, garbage — would silently fail to parse there
/// and the row would simply never show up in a report.
fn is_iso_date(s: &str) -> bool {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

fn opening_balance_problems(input: &SaveAccount, write: Write) -> Vec<String> {
    let amount = input.opening_balance_minor;
    let date = opening_balance_date(input);

    // Once the account exists its balance is maintained through transactions/valuations, so
    // accepting an opening balance on an edit would quietly stamp a second "Opening balance"
    // row into the middle of its history every time the form was saved.
    if write == Write::Update {
        return if amount.is_some() || date.is_some() {
            vec!["opening balance can only be set when creating an account".to_string()]
        } else {
            Vec::new()
        };
    }

    let mut problems = Vec::new();

    // A brokerage account's value is computed entirely from its holdings ledger (see
    // `crate::brokerage`), so an opening balance there wouldn't just be redundant, it would
    // double-count — refuse a supplied value outright, in every mode (manual create or
    // provider link), rather than silently seeding a valuation that then fights the ledger
    // for the "true" balance.
    if input.kind == AccountKind::Brokerage {
        if amount.is_some() || date.is_some() {
            problems.push(
                "opening_balance_minor/opening_balance_date are not accepted for a brokerage \
                 account; its value comes from the holdings ledger"
                    .to_string(),
            );
        }
        return problems;
    }

    // A linked account gets its balance from the feed on the first sync, so neither field is
    // *required* on that path — but half a pair is a caller bug in every mode, and dropping
    // it silently would lose a balance the user really did enter.
    let required = write == Write::Create;
    if amount.is_none() && (required || date.is_some()) {
        problems.push("opening_balance_minor is required".to_string());
    }
    match date {
        None if required || amount.is_some() => {
            problems.push("opening_balance_date is required".to_string());
        }
        Some(d) if !is_iso_date(d) => {
            problems.push(format!(
                "opening_balance_date '{d}' is not a valid ISO-8601 date (expected YYYY-MM-DD)"
            ));
        }
        _ => {}
    }

    // net_worth (and every balance report) buckets an account purely by the sign of its
    // value, so a liability's opening figure has to already be negative (or zero) — a
    // positive mortgage would land in assets instead of debt, and there both never would
    // and never should end up corrected by a later valuation (see `insert`, defect 1).
    if let Some(amount) = amount {
        let convention = "liabilities are negative in this app; a debt's opening balance must \
                           be zero or negative";
        if input.kind.class() == AccountClass::Liability {
            if amount > 0 {
                problems.push(format!(
                    "opening_balance_minor must be zero or negative for a liability account ({convention})"
                ));
            }
        } else if amount < 0 {
            problems.push(format!(
                "opening_balance_minor must be zero or positive for this account ({convention})"
            ));
        }
    }

    problems
}

/// Insert an account row — plus every row its input implies (a property's purchase-price
/// valuation, the opening balance) — inside an existing SQLite transaction, so a caller
/// creating an account as part of a bigger unit of work (the provider-link path) gets
/// exactly the same account as `POST /api/accounts` would build.
pub(crate) async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &SaveAccount,
    metadata: &str,
) -> AppResult<Account> {
    let row = sqlx::query_as::<_, AccountRow>(
        "INSERT INTO accounts (name, kind, currency_code, institution, metadata, archived, sort_order)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING *",
    )
    .bind(input.name.trim())
    .bind(input.kind.as_str())
    .bind(input.currency_code.trim().to_uppercase())
    .bind(&input.institution)
    .bind(metadata)
    .bind(input.archived)
    .bind(input.sort_order)
    .fetch_one(&mut **tx)
    .await?;
    let account: Account = row.try_into()?;

    // A property's purchase price/date *is* an initial valuation — seed one so
    // net-worth/equity calculations (which only ever read the `valuations` table, never
    // metadata directly) have a real starting value from day one instead of reading as
    // $0 until the user separately remembers to add one by hand.
    if let AccountMetadata::Property(ref p) = account.metadata {
        if let (Some(price), Some(date)) = (p.purchase_price_minor, &p.purchase_date) {
            insert_valuation(
                tx,
                &account,
                date,
                price,
                "Initial valuation from purchase price",
            )
            .await?;
        }
    }

    // The opening balance is seeded here, in the same transaction as the account itself, so
    // an account can never end up without the balance the user gave us — the SPA used to
    // create the account and *then* fire a second request, which left it empty whenever that
    // second call failed.
    if let (Some(amount), Some(date)) = (input.opening_balance_minor, opening_balance_date(input)) {
        // A zero opening balance is the absence of one, and both derivations below already
        // read "no rows" as zero, so writing it would only clutter the ledger of every
        // account that started empty.
        if amount != 0 {
            match opening_balance_ledger(account.kind) {
                // A brokerage account has no ledger to seed — see the fn's doc comment.
                None => {}
                Some(OpeningBalanceLedger::Transaction) => {
                    sqlx::query(
                        "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code,
                            description, is_one_off)
                         VALUES (?1, ?2, ?3, ?4, 'Opening balance', 1)",
                    )
                    .bind(account.id)
                    .bind(date)
                    .bind(amount)
                    .bind(&account.currency_code)
                    .execute(&mut **tx)
                    .await?;
                }
                Some(OpeningBalanceLedger::Valuation) => {
                    insert_valuation(tx, &account, date, amount, "Opening balance").await?;
                }
            }
        }
    }

    Ok(account)
}

/// Which ledger an account's opening balance is seeded into.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpeningBalanceLedger {
    /// A one-off transaction (kept out of the spend/income reports), for a kind that already
    /// accumulates its balance from its own transaction stream.
    Transaction,
    /// A manual valuation, for a kind with no transaction stream of its own.
    Valuation,
}

/// Where `insert` seeds a kind's nonzero opening balance.
///
/// `sure_app::reports::account_value_at` returns the most recent valuation at or before a
/// date *verbatim* and never looks at a transaction after it, so seeding a valuation onto a
/// kind that otherwise accumulates from its own transactions would freeze it at that opening
/// figure forever. Every loan-shaped liability is exactly such a kind: a mortgage/student
/// loan/personal loan/credit card/revolving-credit/generic-liability balance moves through
/// drawdowns and repayments the same way a bank account's moves through deposits and
/// withdrawals, so it belongs on the transaction side alongside the cash-like kinds — not on
/// the valuation side, where a manually seeded opening balance can never be corrected (there
/// is no valuation editor for a liability in the SPA, and the real bug this fixes: every
/// loan-shaped account in production data carries both a provider valuation *and*
/// transactions, and the valuation was permanently pinning the balance). Everything left
/// (property, vehicle, other asset, a manually tracked share/crypto holding) has no
/// transaction stream at all — there's nowhere for its opening figure to go but a valuation.
///
/// `Brokerage` answers `None`: its value comes entirely from the holdings ledger
/// (`crate::brokerage`), so there is no ledger to seed. `opening_balance_problems` already
/// refuses any opening balance for it outright, in every write mode, so `insert` should never
/// reach here with a nonzero brokerage amount — but "should never" is not a reason to panic on
/// a request path, and silently seeding nothing is the correct behaviour anyway. The arm is
/// still named explicitly to keep this match exhaustive against a future `AccountKind`
/// variant (CLAUDE.md rule 2: no `_ =>` over a domain enum).
fn opening_balance_ledger(kind: AccountKind) -> Option<OpeningBalanceLedger> {
    match kind {
        AccountKind::Cash
        | AccountKind::Bank
        | AccountKind::Savings
        | AccountKind::CreditCard
        | AccountKind::RevolvingCredit
        | AccountKind::Mortgage
        | AccountKind::StudentLoan
        | AccountKind::Loan
        | AccountKind::Liability => Some(OpeningBalanceLedger::Transaction),
        AccountKind::RealEstate
        | AccountKind::Vehicle
        | AccountKind::Asset
        | AccountKind::SharesNz
        | AccountKind::SharesUs
        | AccountKind::SharesPrivate
        | AccountKind::Crypto => Some(OpeningBalanceLedger::Valuation),
        AccountKind::Brokerage => None,
    }
}

async fn insert_valuation(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    account: &Account,
    as_of: &str,
    value_minor: i64,
    note: &str,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO valuations (account_id, as_of, value_minor, currency_code, source, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(account.id)
    .bind(as_of)
    .bind(value_minor)
    .bind(&account.currency_code)
    .bind(ValuationSource::Manual.as_str())
    .bind(note)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create(db: &Db, input: SaveAccount) -> AppResult<Account> {
    let metadata = validate(db, &input, Write::Create).await?;
    let mut tx = db.begin().await?;
    let account = insert(&mut tx, &input, &metadata).await?;
    tx.commit().await?;
    Ok(account)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, id: i64, input: SaveAccount) -> AppResult<Account> {
    let metadata = validate(db, &input, Write::Update).await?;
    let row = sqlx::query_as::<_, AccountRow>(
        "UPDATE accounts SET name=?2, kind=?3, currency_code=?4, institution=?5, metadata=?6,
            archived=?7, sort_order=?8, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=?1 RETURNING *",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(input.kind.as_str())
    .bind(input.currency_code.trim().to_uppercase())
    .bind(&input.institution)
    .bind(metadata)
    .bind(input.archived)
    .bind(input.sort_order)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("account"))?;
    row.try_into()
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
    row.try_into()
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
        // No other profile has an "original amount borrowed" concept at all.
        AccountMetadata::Depository(_)
        | AccountMetadata::Property(_)
        | AccountMetadata::Vehicle(_)
        | AccountMetadata::Shares(_)
        | AccountMetadata::Brokerage(_)
        | AccountMetadata::Crypto(_)
        | AccountMetadata::Generic(_) => return Ok(()),
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
    use sure_core::{
        BrokerageMeta, CryptoMeta, DepositoryMeta, GenericMeta, LoanMeta, MortgageMeta,
        PropertyMeta, RateType, SharesMeta, TaxTreatment, VehicleMeta,
    };

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    // --- input builders -----------------------------------------------------
    //
    // Saving an account now means satisfying its kind's required fields, which would
    // otherwise be restated by every test here. These builders supply exactly the required
    // set and nothing else, so each test can override the one field it is about and any
    // failure it sees is the rule it meant to exercise.

    fn property_meta() -> PropertyMeta {
        PropertyMeta {
            subtype: Some("single_family_home".to_string()),
            address_line1: Some("12 Rimu Street".to_string()),
            city: Some("Wellington".to_string()),
            country: Some("New Zealand".to_string()),
            ..Default::default()
        }
    }

    fn vehicle_meta() -> VehicleMeta {
        VehicleMeta {
            make: Some("Toyota".to_string()),
            model: Some("RAV4".to_string()),
            year: Some(2021),
            ..Default::default()
        }
    }

    fn mortgage_meta() -> MortgageMeta {
        MortgageMeta {
            lender: Some("ASB".to_string()),
            original_amount_minor: Some(48_500_000),
            interest_rate_bps: Some(549),
            rate_type: Some(RateType::Fixed),
            ..Default::default()
        }
    }

    fn loan_meta() -> LoanMeta {
        LoanMeta {
            subtype: Some("student".to_string()),
            lender: Some("StudyLink".to_string()),
            original_amount_minor: Some(3_000_000),
            interest_rate_bps: Some(0),
            ..Default::default()
        }
    }

    fn shares_meta() -> SharesMeta {
        SharesMeta {
            broker: Some("Sharesies".to_string()),
            ticker: Some("MEL".to_string()),
            exchange: Some("NZX".to_string()),
            ..Default::default()
        }
    }

    fn crypto_meta() -> CryptoMeta {
        CryptoMeta {
            subtype: Some("wallet".to_string()),
            tax_treatment: Some(TaxTreatment::Taxable),
            ..Default::default()
        }
    }

    fn required_metadata(kind: AccountKind) -> AccountMetadata {
        use AccountKind::*;
        match kind {
            Cash | Bank | Savings => AccountMetadata::Depository(DepositoryMeta::default()),
            // Only the revolving kinds need a limit.
            CreditCard | RevolvingCredit => AccountMetadata::Depository(DepositoryMeta {
                credit_limit_minor: Some(500_000),
                ..Default::default()
            }),
            RealEstate => AccountMetadata::Property(property_meta()),
            Mortgage => AccountMetadata::Mortgage(mortgage_meta()),
            Loan | StudentLoan => AccountMetadata::Loan(loan_meta()),
            Vehicle => AccountMetadata::Vehicle(vehicle_meta()),
            SharesNz | SharesUs | SharesPrivate => AccountMetadata::Shares(shares_meta()),
            Brokerage => AccountMetadata::Brokerage(BrokerageMeta {
                broker: Some("Sharesies".to_string()),
                ..Default::default()
            }),
            Crypto => AccountMetadata::Crypto(crypto_meta()),
            Asset | Liability => AccountMetadata::Generic(GenericMeta::default()),
        }
    }

    /// An input the account form would accept for `kind`: required metadata, an institution
    /// where one is required, and a zero opening balance — zero seeds no ledger row (see
    /// [`insert`]), so a test's own transactions/valuations are the only ones present.
    fn valid(name: &str, kind: AccountKind) -> SaveAccount {
        let needs_opening_balance = kind != AccountKind::Brokerage;
        SaveAccount {
            name: name.to_string(),
            kind,
            currency_code: "NZD".to_string(),
            institution: INSTITUTION_REQUIRED
                .contains(&kind)
                .then(|| "ANZ".to_string()),
            metadata: Some(required_metadata(kind)),
            archived: false,
            sort_order: 0,
            opening_balance_minor: needs_opening_balance.then_some(0),
            opening_balance_date: needs_opening_balance.then(|| "2020-01-01".to_string()),
        }
    }

    /// Insert an account row straight into SQLite, bypassing validation — how the rows that
    /// predate a required field (and provider-linked ones, which only ever went through
    /// [`ValidationMode::Linked`]) actually look on disk.
    async fn insert_legacy(db: &Db, name: &str, kind: AccountKind, metadata: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO accounts (name, kind, currency_code, metadata) VALUES (?1,?2,'NZD',?3)
             RETURNING id",
        )
        .bind(name)
        .bind(kind.as_str())
        .bind(metadata)
        .fetch_one(db)
        .await
        .unwrap()
    }

    /// The message of a validation failure, or a panic naming what came back instead.
    fn validation_message<T: std::fmt::Debug>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::Validation(msg)) => msg,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    // --- provider-driven metadata writes ------------------------------------

    #[tokio::test]
    async fn sets_a_credit_limit_without_touching_other_metadata() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Depository(DepositoryMeta {
                    account_number: Some("••1234".to_string()),
                    credit_limit_minor: Some(500_000),
                    notes: Some("keep me".to_string()),
                    ..Default::default()
                })),
                ..valid("Visa", AccountKind::CreditCard)
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
        let account = create(&db, valid("Mortgage", AccountKind::Mortgage))
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
                metadata: Some(AccountMetadata::Mortgage(MortgageMeta {
                    original_amount_minor: Some(50_000_000),
                    ..mortgage_meta()
                })),
                ..valid("Prime Housing Lending", AccountKind::Mortgage)
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
        let account = create(&db, valid("Student Loan", AccountKind::StudentLoan))
            .await
            .unwrap();

        set_original_amount(&db, account.id, 3_500_000)
            .await
            .unwrap();

        let updated = get(&db, account.id).await.unwrap();
        let AccountMetadata::Loan(meta) = updated.metadata else {
            panic!("expected loan metadata");
        };
        assert_eq!(meta.original_amount_minor, Some(3_500_000));
    }

    #[tokio::test]
    async fn original_amount_is_a_no_op_for_a_non_loan_account() {
        let db = test_db().await;
        let account = create(&db, valid("Everyday", AccountKind::Bank))
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
        // Cash is the depository kind with no institution requirement (notes in a drawer
        // have no bank); a provider-linked bank account reaches the same state, because the
        // link path validates in `ValidationMode::Linked`.
        let no_institution = create(&db, valid("Wallet", AccountKind::Cash))
            .await
            .unwrap();
        let has_institution = create(
            &db,
            SaveAccount {
                institution: Some("My Custom Label".to_string()),
                ..valid("Savings", AccountKind::Savings)
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

    // --- required fields ----------------------------------------------------

    #[tokio::test]
    async fn a_property_must_name_its_subtype_and_where_it_is() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Property(PropertyMeta::default())),
                ..valid("Family Home", AccountKind::RealEstate)
            },
        )
        .await;

        // Every gap in one answer, not one per round trip.
        let msg = validation_message(result);
        for field in ["subtype", "address_line1", "city", "country"] {
            assert!(msg.contains(field), "{msg} should name {field}");
        }
    }

    #[tokio::test]
    async fn a_vehicle_must_name_its_make_model_and_year() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Vehicle(VehicleMeta::default())),
                ..valid("Family Car", AccountKind::Vehicle)
            },
        )
        .await;

        let msg = validation_message(result);
        for field in ["make", "model", "year"] {
            assert!(msg.contains(field), "{msg} should name {field}");
        }
    }

    #[tokio::test]
    async fn a_mortgage_must_carry_its_lender_principal_and_rate() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Mortgage(MortgageMeta::default())),
                ..valid("Home Loan", AccountKind::Mortgage)
            },
        )
        .await;

        let msg = validation_message(result);
        for field in [
            "lender",
            "original_amount_minor",
            "interest_rate_bps",
            "rate_type",
        ] {
            assert!(msg.contains(field), "{msg} should name {field}");
        }
    }

    #[tokio::test]
    async fn a_loan_must_carry_its_subtype_lender_principal_and_rate() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Loan(LoanMeta::default())),
                ..valid("Car Loan", AccountKind::Loan)
            },
        )
        .await;

        let msg = validation_message(result);
        for field in [
            "subtype",
            "lender",
            "original_amount_minor",
            "interest_rate_bps",
        ] {
            assert!(msg.contains(field), "{msg} should name {field}");
        }
        // A loan's rate *type* stays optional, unlike a mortgage's.
        assert!(
            !msg.contains("rate_type"),
            "{msg} should not name rate_type"
        );
    }

    #[tokio::test]
    async fn an_interest_free_loan_is_allowed_but_a_negative_rate_is_not() {
        let db = test_db().await;
        // 0 bps is a real rate — an interest-free family loan — so it must pass.
        create(&db, valid("Student Loan", AccountKind::StudentLoan))
            .await
            .unwrap();

        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Loan(LoanMeta {
                    interest_rate_bps: Some(-100),
                    ..loan_meta()
                })),
                ..valid("Odd Loan", AccountKind::Loan)
            },
        )
        .await;
        assert!(validation_message(result).contains("interest_rate_bps cannot be negative"));
    }

    #[tokio::test]
    async fn a_required_amount_of_zero_is_not_an_answer() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Mortgage(MortgageMeta {
                    original_amount_minor: Some(0),
                    ..mortgage_meta()
                })),
                ..valid("Home Loan", AccountKind::Mortgage)
            },
        )
        .await;
        assert!(
            validation_message(result).contains("original_amount_minor must be greater than zero")
        );
    }

    #[tokio::test]
    async fn an_investment_account_must_name_its_broker() {
        let db = test_db().await;
        for (kind, metadata) in [
            (
                AccountKind::SharesPrivate,
                AccountMetadata::Shares(SharesMeta {
                    broker: None,
                    ..shares_meta()
                }),
            ),
            (
                AccountKind::Brokerage,
                AccountMetadata::Brokerage(BrokerageMeta::default()),
            ),
        ] {
            let result = create(
                &db,
                SaveAccount {
                    metadata: Some(metadata),
                    ..valid("Holdings", kind)
                },
            )
            .await;
            assert!(validation_message(result).contains("broker is required"));
        }
    }

    #[tokio::test]
    async fn a_crypto_account_must_say_where_it_is_held_and_how_it_is_taxed() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Crypto(CryptoMeta::default())),
                ..valid("Cold Wallet", AccountKind::Crypto)
            },
        )
        .await;

        let msg = validation_message(result);
        assert!(msg.contains("subtype"), "{msg} should name subtype");
        assert!(
            msg.contains("tax_treatment"),
            "{msg} should name tax_treatment"
        );
    }

    #[tokio::test]
    async fn the_free_form_kinds_require_no_metadata_at_all() {
        let db = test_db().await;
        // "Other asset"/"other liability" are catch-alls, and our `kind` already says
        // everything a depository subtype would; cash needs no institution either.
        for (name, kind) in [
            ("Boat share", AccountKind::Asset),
            ("Owed to Mum", AccountKind::Liability),
            ("Wallet", AccountKind::Cash),
        ] {
            create(
                &db,
                SaveAccount {
                    metadata: None,
                    ..valid(name, kind)
                },
            )
            .await
            .unwrap();
        }
    }

    // --- kind-conditional requirements --------------------------------------

    #[tokio::test]
    async fn a_card_needs_a_credit_limit_but_a_savings_account_does_not() {
        let db = test_db().await;
        for kind in [AccountKind::CreditCard, AccountKind::RevolvingCredit] {
            let result = create(
                &db,
                SaveAccount {
                    metadata: Some(AccountMetadata::Depository(DepositoryMeta::default())),
                    ..valid("Visa", kind)
                },
            )
            .await;
            assert!(validation_message(result).contains("credit_limit_minor is required"));
        }

        // A limit is meaningless on a savings account, so none is asked for.
        create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Depository(DepositoryMeta::default())),
                ..valid("Rainy Day", AccountKind::Savings)
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_listed_holding_needs_a_ticker_and_exchange_but_a_private_one_does_not() {
        let db = test_db().await;
        for kind in [AccountKind::SharesNz, AccountKind::SharesUs] {
            let result = create(
                &db,
                SaveAccount {
                    metadata: Some(AccountMetadata::Shares(SharesMeta {
                        ticker: None,
                        exchange: None,
                        ..shares_meta()
                    })),
                    ..valid("Holdings", kind)
                },
            )
            .await;
            let msg = validation_message(result);
            assert!(msg.contains("ticker"), "{msg} should name ticker");
            assert!(msg.contains("exchange"), "{msg} should name exchange");
        }

        // An unlisted holding has neither.
        create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Shares(SharesMeta {
                    ticker: None,
                    exchange: None,
                    ..shares_meta()
                })),
                ..valid("Startup equity", AccountKind::SharesPrivate)
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_bank_account_needs_an_institution() {
        let db = test_db().await;
        for kind in [
            AccountKind::Bank,
            AccountKind::Savings,
            AccountKind::CreditCard,
            AccountKind::RevolvingCredit,
        ] {
            let result = create(
                &db,
                SaveAccount {
                    // Blank counts as absent: whitespace is not an answer.
                    institution: Some("   ".to_string()),
                    ..valid("Everyday", kind)
                },
            )
            .await;
            assert!(validation_message(result).contains("institution is required"));
        }
    }

    #[tokio::test]
    async fn a_blank_name_and_missing_metadata_are_both_reported_at_once() {
        let db = test_db().await;
        // `validate` used to `return` as soon as it hit the blank name, so a caller fixing
        // just that got a *second* 422 for the metadata gap on the very next try. Both have
        // to come back together.
        let result = create(
            &db,
            SaveAccount {
                name: "   ".to_string(),
                metadata: Some(AccountMetadata::Property(PropertyMeta::default())),
                ..valid("Family Home", AccountKind::RealEstate)
            },
        )
        .await;
        let msg = validation_message(result);
        assert!(msg.contains("account name is required"), "{msg}");
        for field in ["subtype", "address_line1", "city", "country"] {
            assert!(msg.contains(field), "{msg} should also name {field}");
        }
    }

    #[tokio::test]
    async fn a_subtype_outside_the_curated_list_is_rejected() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Property(PropertyMeta {
                    subtype: Some("castle".to_string()),
                    ..property_meta()
                })),
                ..valid("Family Home", AccountKind::RealEstate)
            },
        )
        .await;

        let msg = validation_message(result);
        assert!(msg.contains("castle"), "{msg} should quote the bad value");
        assert!(
            msg.contains("single_family_home"),
            "{msg} should list the legal values"
        );
    }

    /// The one metadata rule that also holds on the provider-link path: a feed cannot know a
    /// property's city, but a value it *does* send has to mean something.
    #[test]
    fn an_illegal_subtype_is_rejected_even_when_linking() {
        let metadata = AccountMetadata::Loan(LoanMeta {
            subtype: Some("payday".to_string()),
            ..Default::default()
        });
        let problems = metadata
            .validate_for(AccountKind::Loan, ValidationMode::Linked)
            .expect_err("an unknown subtype is structural, not a gap a sync could fill");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("payday"));

        // ...while everything a sync could fill in later is left alone.
        AccountMetadata::Loan(LoanMeta::default())
            .validate_for(AccountKind::Loan, ValidationMode::Linked)
            .unwrap();
    }

    // --- opening balance ----------------------------------------------------

    #[tokio::test]
    async fn an_opening_balance_is_required_when_creating_an_account() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                opening_balance_minor: None,
                opening_balance_date: None,
                ..valid("Everyday", AccountKind::Bank)
            },
        )
        .await;

        let msg = validation_message(result);
        assert!(msg.contains("opening_balance_minor"), "{msg}");
        assert!(msg.contains("opening_balance_date"), "{msg}");
    }

    #[tokio::test]
    async fn half_an_opening_balance_is_refused() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                opening_balance_date: None,
                opening_balance_minor: Some(10_000),
                ..valid("Everyday", AccountKind::Bank)
            },
        )
        .await;
        assert!(validation_message(result).contains("opening_balance_date is required"));
    }

    #[tokio::test]
    async fn a_brokerage_account_is_not_asked_for_an_opening_balance() {
        let db = test_db().await;
        // Its value is computed from the holdings ledger, so a seeded balance would
        // double-count.
        let account = create(&db, valid("Sharesies", AccountKind::Brokerage))
            .await
            .unwrap();
        assert!(crate::valuations::list_for_account(&db, account.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_opening_balance_seeds_a_transaction_for_a_cash_like_account() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(250_000),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Everyday", AccountKind::Bank)
            },
        )
        .await
        .unwrap();

        // A valuation would freeze the account at this figure (see `insert`), so it has to
        // be a transaction — and a one-off, to stay out of the spend/income reports.
        let txs = crate::transactions::list(
            &db,
            sure_core::TxQuery {
                account_id: Some(account.id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].posted_at, "2024-03-01");
        assert_eq!(txs[0].amount_minor, 250_000);
        assert_eq!(txs[0].description, "Opening balance");
        assert!(txs[0].is_one_off);
        assert!(crate::valuations::list_for_account(&db, account.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_opening_balance_seeds_a_valuation_for_a_valued_account() {
        let db = test_db().await;
        let account = create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(82_000_000),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Family Home", AccountKind::RealEstate)
            },
        )
        .await
        .unwrap();

        let vals = crate::valuations::list_for_account(&db, account.id)
            .await
            .unwrap();
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0].as_of, "2024-03-01");
        assert_eq!(vals[0].value_minor, 82_000_000);
        assert_eq!(vals[0].note.as_deref(), Some("Opening balance"));
    }

    #[tokio::test]
    async fn a_zero_opening_balance_seeds_nothing() {
        let db = test_db().await;
        let account = create(&db, valid("Everyday", AccountKind::Bank))
            .await
            .unwrap();

        // Zero is the absence of a balance, and both derivations read "no rows" as zero, so
        // the ledger stays empty rather than gaining a $0 row.
        assert!(crate::transactions::list(
            &db,
            sure_core::TxQuery {
                account_id: Some(account.id),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .is_empty());
        assert!(crate::valuations::list_for_account(&db, account.id)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_opening_balance_seeds_a_transaction_for_every_loan_shaped_liability() {
        let db = test_db().await;
        // The defect this guards against: `account_value_at` (`sure_app::reports`) returns the
        // most recent valuation verbatim and never consults transactions, so a valuation would
        // pin a loan-shaped account at its opening figure forever. Every kind whose balance
        // moves through drawdowns/repayments — not just the plain cash-like kinds — has to
        // seed a transaction instead.
        for kind in [
            AccountKind::Mortgage,
            AccountKind::StudentLoan,
            AccountKind::Loan,
            AccountKind::CreditCard,
            AccountKind::RevolvingCredit,
            AccountKind::Liability,
        ] {
            let account = create(
                &db,
                SaveAccount {
                    opening_balance_minor: Some(-56_000_00),
                    opening_balance_date: Some("2024-03-01".to_string()),
                    ..valid("Loan", kind)
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{kind:?} should be accepted: {e:?}"));

            let txs = crate::transactions::list(
                &db,
                sure_core::TxQuery {
                    account_id: Some(account.id),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(txs.len(), 1, "{kind:?} should seed a transaction");
            assert_eq!(txs[0].amount_minor, -56_000_00);
            assert_eq!(txs[0].description, "Opening balance");
            assert!(
                crate::valuations::list_for_account(&db, account.id)
                    .await
                    .unwrap()
                    .is_empty(),
                "{kind:?} should not also seed a valuation"
            );
        }
    }

    #[tokio::test]
    async fn an_opening_balance_seeds_a_valuation_for_every_valued_asset_kind() {
        let db = test_db().await;
        // The other half of the split: kinds with no transaction stream of their own still
        // seed a valuation, exactly as before — a property already covers this above; the
        // remaining valued kinds (vehicle, other asset, a manually tracked share/crypto
        // holding) must land in the same place.
        for kind in [
            AccountKind::Vehicle,
            AccountKind::Asset,
            AccountKind::SharesNz,
            AccountKind::SharesUs,
            AccountKind::SharesPrivate,
            AccountKind::Crypto,
        ] {
            let account = create(
                &db,
                SaveAccount {
                    opening_balance_minor: Some(15_000_00),
                    opening_balance_date: Some("2024-03-01".to_string()),
                    ..valid("Holding", kind)
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{kind:?} should be accepted: {e:?}"));

            let vals = crate::valuations::list_for_account(&db, account.id)
                .await
                .unwrap();
            assert_eq!(vals.len(), 1, "{kind:?} should seed a valuation");
            assert_eq!(vals[0].value_minor, 15_000_00);
            assert!(
                crate::transactions::list(
                    &db,
                    sure_core::TxQuery {
                        account_id: Some(account.id),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
                .is_empty(),
                "{kind:?} should not also seed a transaction"
            );
        }
    }

    #[tokio::test]
    async fn a_liability_opening_balance_cannot_be_positive() {
        let db = test_db().await;
        // Net worth buckets purely by sign, so a positive mortgage would land in assets
        // instead of debt — and a valuation-seeded manual loan can never be corrected
        // afterwards (no liability-class kind exposes a valuation editor in the SPA).
        let result = create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(1_000_00),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Home Loan", AccountKind::Mortgage)
            },
        )
        .await;
        let msg = validation_message(result);
        assert!(
            msg.contains("opening_balance_minor must be zero or negative"),
            "{msg}"
        );
        assert!(
            msg.contains("liabilities are negative in this app"),
            "{msg}"
        );

        // Zero and negative both remain fine.
        create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Home Loan (zero)", AccountKind::Mortgage)
            },
        )
        .await
        .unwrap();
        create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(-56_000_00),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Home Loan (negative)", AccountKind::Mortgage)
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_non_liability_opening_balance_cannot_be_negative() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(-1_000_00),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Everyday", AccountKind::Bank)
            },
        )
        .await;
        let msg = validation_message(result);
        assert!(
            msg.contains("opening_balance_minor must be zero or positive"),
            "{msg}"
        );
        assert!(
            msg.contains("liabilities are negative in this app"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn a_brokerage_opening_balance_is_rejected_outright() {
        let db = test_db().await;
        // Dropped from *required*, not from *accepted*: a brokerage account's value comes
        // entirely from its holdings ledger, so a supplied value must still be refused rather
        // than silently seeded (which would double-count against the ledger).
        let result = create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(10_000),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Sharesies", AccountKind::Brokerage)
            },
        )
        .await;
        assert!(validation_message(result).contains("not accepted for a brokerage account"));
    }

    #[tokio::test]
    async fn a_malformed_opening_balance_date_is_rejected() {
        let db = test_db().await;
        let result = create(
            &db,
            SaveAccount {
                opening_balance_minor: Some(10_000),
                opening_balance_date: Some("31/07/2026".to_string()),
                ..valid("Everyday", AccountKind::Bank)
            },
        )
        .await;
        let msg = validation_message(result);
        assert!(msg.contains("opening_balance_date"), "{msg}");
        assert!(msg.contains("not a valid ISO-8601 date"), "{msg}");
    }

    #[tokio::test]
    async fn an_opening_balance_cannot_be_set_when_updating() {
        let db = test_db().await;
        let account = create(&db, valid("Everyday", AccountKind::Bank))
            .await
            .unwrap();

        let result = update(
            &db,
            account.id,
            SaveAccount {
                opening_balance_minor: Some(999_000),
                opening_balance_date: Some("2024-03-01".to_string()),
                ..valid("Everyday", AccountKind::Bank)
            },
        )
        .await;
        assert!(validation_message(result)
            .contains("opening balance can only be set when creating an account"));

        // The same edit without it goes through.
        update(
            &db,
            account.id,
            SaveAccount {
                opening_balance_minor: None,
                opening_balance_date: None,
                ..valid("Everyday (joint)", AccountKind::Bank)
            },
        )
        .await
        .unwrap();
    }

    // --- reading is never blocked -------------------------------------------

    #[tokio::test]
    async fn a_row_written_before_a_field_was_required_still_reads() {
        let db = test_db().await;
        // The shape a legacy property row really has: an address under the old single key
        // and nothing else.
        let id = insert_legacy(
            &db,
            "Old Home",
            AccountKind::RealEstate,
            r#"{"address":"9 Legacy Lane"}"#,
        )
        .await;

        let account = get(&db, id).await.unwrap();
        let AccountMetadata::Property(meta) = account.metadata else {
            panic!("expected property metadata");
        };
        assert_eq!(meta.address_line1.as_deref(), Some("9 Legacy Lane"));
        assert_eq!(meta.city, None);
        // It lists, too — a required field can never make an account unreadable.
        assert_eq!(list(&db, false).await.unwrap().len(), 1);

        // Only *saving* it is blocked, which is the prompt to fill the gaps in.
        let result = update(
            &db,
            id,
            SaveAccount {
                metadata: Some(AccountMetadata::Property(PropertyMeta {
                    address_line1: Some("9 Legacy Lane".to_string()),
                    ..Default::default()
                })),
                opening_balance_minor: None,
                opening_balance_date: None,
                ..valid("Old Home", AccountKind::RealEstate)
            },
        )
        .await;
        assert!(validation_message(result).contains("city is required"));

        // ...and filling them in saves normally: the account is never left uneditable.
        let saved = update(
            &db,
            id,
            SaveAccount {
                opening_balance_minor: None,
                opening_balance_date: None,
                ..valid("Old Home", AccountKind::RealEstate)
            },
        )
        .await
        .unwrap();
        let AccountMetadata::Property(meta) = saved.metadata else {
            panic!("expected property metadata");
        };
        assert_eq!(meta.city.as_deref(), Some("Wellington"));
    }

    // --- property purchase price --------------------------------------------

    #[tokio::test]
    async fn creating_a_property_with_a_purchase_price_seeds_its_initial_valuation() {
        let db = test_db().await;
        let house = create(
            &db,
            SaveAccount {
                metadata: Some(AccountMetadata::Property(PropertyMeta {
                    purchase_date: Some("2025-12-12".to_string()),
                    purchase_price_minor: Some(77_000_000),
                    ..property_meta()
                })),
                ..valid("Family Home", AccountKind::RealEstate)
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
        let house = create(&db, valid("Family Home", AccountKind::RealEstate))
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
        let account = create(&db, valid("Everyday", AccountKind::Bank))
            .await
            .unwrap();

        assert!(crate::valuations::list_for_account(&db, account.id)
            .await
            .unwrap()
            .is_empty());
    }

    // --- ticker discovery ---------------------------------------------------

    #[tokio::test]
    async fn lists_distinct_tickers_from_market_shares_accounts_only() {
        let db = test_db().await;
        let shares = |ticker: &str, exchange: &str| {
            Some(AccountMetadata::Shares(SharesMeta {
                ticker: Some(ticker.to_string()),
                exchange: Some(exchange.to_string()),
                ..shares_meta()
            }))
        };
        create(
            &db,
            SaveAccount {
                metadata: shares("mel", "nzx"),
                ..valid("Meridian", AccountKind::SharesNz)
            },
        )
        .await
        .unwrap();
        // A second account holding the same ticker shouldn't produce a duplicate entry.
        create(
            &db,
            SaveAccount {
                metadata: shares("mel", "nzx"),
                ..valid("Meridian (also)", AccountKind::SharesNz)
            },
        )
        .await
        .unwrap();
        create(
            &db,
            SaveAccount {
                currency_code: "USD".to_string(),
                metadata: shares("aapl", "nasdaq"),
                ..valid("Apple", AccountKind::SharesUs)
            },
        )
        .await
        .unwrap();
        // Private holdings have no market ticker to fetch a price for.
        create(
            &db,
            SaveAccount {
                metadata: shares("n/a", ""),
                ..valid("Startup equity", AccountKind::SharesPrivate)
            },
        )
        .await
        .unwrap();
        // A legacy row with no ticker at all — excluded rather than fetched as "".
        insert_legacy(&db, "Undecided holding", AccountKind::SharesUs, "{}").await;

        let mut tickers = list_shares_tickers(&db).await.unwrap();
        tickers.sort_by(|a, b| a.ticker.cmp(&b.ticker));

        assert_eq!(tickers.len(), 2);
        assert_eq!(tickers[0].ticker, "AAPL");
        assert_eq!(tickers[0].exchange, "nasdaq");
        assert_eq!(tickers[1].ticker, "MEL");
        assert_eq!(tickers[1].exchange, "nzx");
    }
}
