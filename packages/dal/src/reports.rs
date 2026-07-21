//! Read-only loaders backing the report endpoints. This layer only fetches rows; all
//! the aggregation (running balances, currency normalisation, category roll-ups, flow
//! graphs) lives in the API crate, which calls these loaders and crunches the numbers.

use sqlx::FromRow;
use sure_core::{AppError, AppResult};

use crate::Db;

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

/// A non-archived account, for the current-balances report.
#[derive(Debug, FromRow)]
pub struct ActiveAccount {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
}

/// A single asset account, for the equity-position report.
#[derive(Debug, FromRow)]
pub struct AssetAccount {
    pub id: i64,
    pub name: String,
    pub currency_code: String,
}

/// A liability secured against an asset.
#[derive(Debug, FromRow)]
pub struct SecuredLiabilityAccount {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
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

/// A category's shape, for building the parent/name/colour/kind lookups.
#[derive(Debug, FromRow)]
pub struct Category {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub kind: String,
}

/// A transaction with the fields the spend reports (pie + sankey) filter and roll up.
#[derive(Debug, FromRow)]
pub struct SpendTransaction {
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub linked_transaction_id: Option<i64>,
    pub account_kind: String,
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
    Ok(sqlx::query_as::<_, ActiveAccount>(
        "SELECT id, name, kind, currency_code FROM accounts WHERE archived=0 ORDER BY sort_order, name",
    )
    .fetch_all(db)
    .await?)
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
    Ok(sqlx::query_as::<_, SecuredLiabilityAccount>(
        "SELECT id, name, kind, currency_code FROM accounts
         WHERE secured_by_account_id=?1 ORDER BY sort_order, name",
    )
    .bind(asset_id)
    .fetch_all(db)
    .await?)
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
#[tracing::instrument(level = "debug", skip_all)]
pub async fn valuations(db: &Db) -> AppResult<Vec<LedgerValuation>> {
    Ok(sqlx::query_as::<_, LedgerValuation>(
        "SELECT account_id, as_of, value_minor, currency_code FROM valuations",
    )
    .fetch_all(db)
    .await?)
}

/// Every category's shape.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn categories(db: &Db) -> AppResult<Vec<Category>> {
    Ok(
        sqlx::query_as::<_, Category>("SELECT id, parent_id, name, color, kind FROM categories")
            .fetch_all(db)
            .await?,
    )
}

/// All transactions with the fields the spend reports need to filter and roll up.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn spend_transactions(db: &Db) -> AppResult<Vec<SpendTransaction>> {
    Ok(sqlx::query_as::<_, SpendTransaction>(
        "SELECT t.posted_at, t.amount_minor, t.currency_code, t.category_id, t.is_one_off,
                t.linked_transaction_id, a.kind AS account_kind
         FROM transactions t JOIN accounts a ON a.id = t.account_id",
    )
    .fetch_all(db)
    .await?)
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
