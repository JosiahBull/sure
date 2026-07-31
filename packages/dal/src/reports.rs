//! Read-only loaders backing the report endpoints. This layer only fetches rows; all
//! the aggregation (running balances, currency normalisation, category roll-ups, flow
//! graphs) lives in the API crate, which calls these loaders and crunches the numbers.

use sqlx::FromRow;
use sure_core::{AccountKind, AppError, AppResult, CategoryKind};

use crate::Db;

/// Parse a stored `kind` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `AccountKind::as_str`, so an unparseable value means the row came
/// from something else entirely and deserves a real error, not a silent default.
fn parse_kind(kind: String) -> AppResult<AccountKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

/// A currency's minor-unit scale, for converting minor units to major.
#[derive(Debug, FromRow)]
pub struct CurrencyDecimals {
    pub code: String,
    pub decimal_places: i64,
}

/// A stored exchange rate. `rate` is kept as text (exact decimal) and parsed by the caller.
#[derive(Debug, FromRow)]
pub struct ExchangeRate {
    pub base_code: String,
    pub quote_code: String,
    pub rate: String,
}

/// An account and its currency (all accounts, including archived) — for net-worth history.
#[derive(Debug, FromRow)]
pub struct AccountCurrency {
    pub id: i64,
    pub currency_code: String,
}

/// The raw row shape for [`ActiveAccount`] — `kind` as stored, before parsing.
#[derive(Debug, FromRow)]
struct ActiveAccountRow {
    id: i64,
    name: String,
    kind: String,
    currency_code: String,
}

/// A non-archived account, for the current-balances report.
#[derive(Debug)]
pub struct ActiveAccount {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
}

impl TryFrom<ActiveAccountRow> for ActiveAccount {
    type Error = AppError;

    fn try_from(r: ActiveAccountRow) -> AppResult<Self> {
        Ok(ActiveAccount {
            kind: parse_kind(r.kind)?,
            id: r.id,
            name: r.name,
            currency_code: r.currency_code,
        })
    }
}

/// A single asset account, for the equity-position report.
#[derive(Debug, FromRow)]
pub struct AssetAccount {
    pub id: i64,
    pub name: String,
    pub currency_code: String,
}

/// The raw row shape for [`SecuredLiabilityAccount`] — `kind` as stored, before parsing.
#[derive(Debug, FromRow)]
struct SecuredLiabilityAccountRow {
    id: i64,
    name: String,
    kind: String,
    currency_code: String,
}

/// A liability secured against an asset.
#[derive(Debug)]
pub struct SecuredLiabilityAccount {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
}

impl TryFrom<SecuredLiabilityAccountRow> for SecuredLiabilityAccount {
    type Error = AppError;

    fn try_from(r: SecuredLiabilityAccountRow) -> AppResult<Self> {
        Ok(SecuredLiabilityAccount {
            kind: parse_kind(r.kind)?,
            id: r.id,
            name: r.name,
            currency_code: r.currency_code,
        })
    }
}

/// A transaction reduced to what a running balance needs.
#[derive(Debug, FromRow)]
pub struct LedgerTx {
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
}

/// A point-in-time valuation reduced to what a running balance needs.
#[derive(Debug, FromRow)]
pub struct LedgerValuation {
    pub account_id: i64,
    pub as_of: String,
    pub value_minor: i64,
    pub currency_code: String,
}

/// The raw row shape for [`Category`] — `kind` as stored, before parsing.
#[derive(Debug, FromRow)]
struct CategoryRow {
    id: i64,
    parent_id: Option<i64>,
    name: String,
    color: Option<String>,
    kind: String,
}

/// A category's shape, for building the parent/name/colour/kind lookups.
#[derive(Debug)]
pub struct Category {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub kind: CategoryKind,
}

impl TryFrom<CategoryRow> for Category {
    type Error = AppError;

    fn try_from(r: CategoryRow) -> AppResult<Self> {
        Ok(Category {
            kind: parse_category_kind(r.kind)?,
            id: r.id,
            parent_id: r.parent_id,
            name: r.name,
            color: r.color,
        })
    }
}

/// Parse a stored `kind` TEXT column into the domain enum — see [`parse_kind`]'s doc.
fn parse_category_kind(kind: String) -> AppResult<CategoryKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

/// The raw row shape for [`SpendTransaction`] — `account_kind` as stored, before parsing.
#[derive(Debug, FromRow)]
struct SpendTransactionRow {
    posted_at: String,
    amount_minor: i64,
    currency_code: String,
    category_id: Option<i64>,
    is_one_off: bool,
    linked_transaction_id: Option<i64>,
    account_kind: String,
}

/// A transaction with the fields the spend reports (pie + sankey) filter and roll up.
#[derive(Debug)]
pub struct SpendTransaction {
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub linked_transaction_id: Option<i64>,
    pub account_kind: AccountKind,
}

impl TryFrom<SpendTransactionRow> for SpendTransaction {
    type Error = AppError;

    fn try_from(r: SpendTransactionRow) -> AppResult<Self> {
        Ok(SpendTransaction {
            account_kind: parse_kind(r.account_kind)?,
            posted_at: r.posted_at,
            amount_minor: r.amount_minor,
            currency_code: r.currency_code,
            category_id: r.category_id,
            is_one_off: r.is_one_off,
            linked_transaction_id: r.linked_transaction_id,
        })
    }
}

/// Every currency's decimal scale.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn currency_decimals(db: &Db) -> AppResult<Vec<CurrencyDecimals>> {
    Ok(
        sqlx::query_as::<_, CurrencyDecimals>("SELECT code, decimal_places FROM currencies")
            .fetch_all(db)
            .await?,
    )
}

/// All stored exchange rates, oldest first (so callers can let later rows win).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn exchange_rates(db: &Db) -> AppResult<Vec<ExchangeRate>> {
    Ok(sqlx::query_as::<_, ExchangeRate>(
        "SELECT base_code, quote_code, rate FROM exchange_rates ORDER BY as_of",
    )
    .fetch_all(db)
    .await?)
}

/// Every account's id + currency (net-worth history spans archived accounts too).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn account_currencies(db: &Db) -> AppResult<Vec<AccountCurrency>> {
    Ok(
        sqlx::query_as::<_, AccountCurrency>("SELECT id, currency_code FROM accounts")
            .fetch_all(db)
            .await?,
    )
}

/// Non-archived accounts in display order.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn active_accounts(db: &Db) -> AppResult<Vec<ActiveAccount>> {
    sqlx::query_as::<_, ActiveAccountRow>(
        "SELECT id, name, kind, currency_code FROM accounts WHERE archived=0 ORDER BY sort_order, name",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(ActiveAccount::try_from)
    .collect()
}

/// One account by id (NotFound if it doesn't exist).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn account(db: &Db, id: i64) -> AppResult<AssetAccount> {
    sqlx::query_as::<_, AssetAccount>("SELECT id, name, currency_code FROM accounts WHERE id=?1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound("account"))
}

/// Liabilities secured against `asset_id`, in display order.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn secured_liabilities(
    db: &Db,
    asset_id: i64,
) -> AppResult<Vec<SecuredLiabilityAccount>> {
    sqlx::query_as::<_, SecuredLiabilityAccountRow>(
        "SELECT id, name, kind, currency_code FROM accounts
         WHERE secured_by_account_id=?1 ORDER BY sort_order, name",
    )
    .bind(asset_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(SecuredLiabilityAccount::try_from)
    .collect()
}

/// All transactions, reduced to (account, date, amount) for running balances.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn transactions(db: &Db) -> AppResult<Vec<LedgerTx>> {
    Ok(sqlx::query_as::<_, LedgerTx>(
        "SELECT account_id, posted_at, amount_minor FROM transactions",
    )
    .fetch_all(db)
    .await?)
}

/// All valuations, reduced to (account, date, value, currency).
///
/// Ordered by `(as_of, id)` because two valuations can legitimately share a date — creating a
/// property seeds one at its purchase price and another at its opening market value, which may
/// be the same day — and `sure_app::reports::account_value_at` picks the *last* of equally-dated
/// rows (`Iterator::max_by_key`). Without an ORDER BY, which of the two it reads is up to
/// SQLite; with it, the one entered last wins, every time.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn valuations(db: &Db) -> AppResult<Vec<LedgerValuation>> {
    Ok(sqlx::query_as::<_, LedgerValuation>(
        "SELECT account_id, as_of, value_minor, currency_code FROM valuations
         ORDER BY as_of, id",
    )
    .fetch_all(db)
    .await?)
}

/// Every category's shape.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn categories(db: &Db) -> AppResult<Vec<Category>> {
    sqlx::query_as::<_, CategoryRow>("SELECT id, parent_id, name, color, kind FROM categories")
        .fetch_all(db)
        .await?
        .into_iter()
        .map(Category::try_from)
        .collect()
}

/// All transactions with the fields the spend reports need to filter and roll up.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn spend_transactions(db: &Db) -> AppResult<Vec<SpendTransaction>> {
    sqlx::query_as::<_, SpendTransactionRow>(
        "SELECT t.posted_at, t.amount_minor, t.currency_code, t.category_id, t.is_one_off,
                t.linked_transaction_id, a.kind AS account_kind
         FROM transactions t JOIN accounts a ON a.id = t.account_id",
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(SpendTransaction::try_from)
    .collect()
}

/// The earliest transaction date on record, for defaulting an unbounded report window.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn earliest_transaction_date(db: &Db) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, Option<String>>("SELECT MIN(posted_at) FROM transactions")
            .fetch_one(db)
            .await?,
    )
}
