//! Repository ports the application core depends on, plus the [`Clock`] abstraction —
//! the seam that lets the logic-heavy services (brokerage, reports, rules, sync) be
//! unit-tested against in-memory fakes instead of a real database and the wall clock.
//!
//! `sure-dal`'s `SqliteStore` implements every trait here by delegating to its existing
//! free functions — the SQL itself is untouched. Because `sure-dal` must depend on
//! `sure-app` to see these traits, `sure-app` cannot depend back on `sure-dal` (Cargo
//! forbids the cycle): every row shape a port returns is therefore a plain type owned by
//! this module, not one of `sure-dal`'s internal `FromRow` structs — the adapter maps
//! between the two. Where a shape is already part of the shared domain vocabulary
//! (`Account`, `Valuation`, `Provider`, ...) the ports reuse `sure_core` directly instead
//! of duplicating it.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use sure_core::{Account, AppResult, Provider, ProviderSync, RunResult, StockPrice};

// ---- Clock ------------------------------------------------------------------

/// Abstracts the wall clock so day-by-day logic (brokerage backfill, stock-price
/// polling, report windows) is deterministic in tests instead of reading `Utc::now()`
/// directly.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn today(&self) -> NaiveDate {
        self.now().date_naive()
    }
}

/// The real clock, used by the composition root.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

// ---- shared row shapes --------------------------------------------------------
// Plain data the SQLite adapter maps its own row types into. Not persistence types —
// see the module doc comment for why these are freshly defined here rather than reused
// from `sure_dal`.

/// A ticker position as of a date, before pricing (see `crate::brokerage`).
#[derive(Debug, Clone)]
pub struct HoldingRow {
    pub ticker: String,
    pub exchange: String,
    pub currency_code: String,
    pub name: Option<String>,
    pub quantity: f64,
}

/// A cash balance in one currency, as of a date.
#[derive(Debug, Clone)]
pub struct WalletRow {
    pub currency_code: String,
    pub amount_minor: i64,
}

/// A `(ticker, exchange)` pair a shares/brokerage account holds.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SharesTicker {
    pub ticker: String,
    pub exchange: String,
}

/// A currency's minor-unit scale, for converting minor units to major.
#[derive(Debug, Clone)]
pub struct CurrencyDecimals {
    pub code: String,
    pub decimal_places: i32,
}

/// A stored (historical) exchange rate. `rate` is exact decimal text.
#[derive(Debug, Clone)]
pub struct ExchangeRateRow {
    pub base_code: String,
    pub quote_code: String,
    pub rate: String,
}

/// An account and its currency (all accounts, including archived) — for net-worth history.
#[derive(Debug, Clone)]
pub struct AccountCurrency {
    pub id: i64,
    pub currency_code: String,
}

/// A non-archived account, for the current-balances report.
#[derive(Debug, Clone)]
pub struct ActiveAccount {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
}

/// A single asset account, for the equity-position report.
#[derive(Debug, Clone)]
pub struct AssetAccount {
    pub id: i64,
    pub name: String,
    pub currency_code: String,
}

/// A liability secured against an asset.
#[derive(Debug, Clone)]
pub struct SecuredLiabilityAccount {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
}

/// A transaction reduced to what a running balance needs.
#[derive(Debug, Clone)]
pub struct LedgerTx {
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
}

/// A point-in-time valuation reduced to what a running balance needs.
#[derive(Debug, Clone)]
pub struct LedgerValuation {
    pub account_id: i64,
    pub as_of: String,
    pub value_minor: i64,
    pub currency_code: String,
}

/// A category's shape, for building the parent/name/colour/kind lookups.
#[derive(Debug, Clone)]
pub struct ReportCategory {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub kind: String,
}

/// A transaction with the fields the spend reports (pie + sankey) filter and roll up.
#[derive(Debug, Clone)]
pub struct SpendTransaction {
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub linked_transaction_id: Option<i64>,
}

/// A transaction row denormalised for rule evaluation.
#[derive(Debug, Clone)]
pub struct TxCtx {
    pub id: i64,
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub decimal_places: i64,
    pub description: String,
    pub merchant: Option<String>,
    pub merchant_id: Option<i64>,
    pub notes: Option<String>,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub categorized_by_rule_id: Option<i64>,
    pub account_name: String,
    pub account_kind: String,
}

/// One decided change from a rule evaluation, ready to be persisted.
#[derive(Debug, Clone)]
pub struct PlannedApplication {
    pub rule_id: i64,
    pub transaction_id: i64,
    pub prev_category_id: Option<i64>,
    pub new_category_id: Option<i64>,
    pub prev_categorized_by_rule_id: Option<i64>,
    pub new_categorized_by_rule_id: Option<i64>,
    pub prev_one_off: bool,
    pub new_one_off: bool,
    pub prev_merchant_id: Option<i64>,
    pub new_merchant_id: Option<i64>,
}

/// A normalised transaction handed from a provider to be imported (dedupe on external id).
#[derive(Debug, Clone)]
pub struct ImportRow {
    pub external_id: String,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    pub description: String,
    pub merchant: Option<String>,
    pub category_name: Option<String>,
    pub category_group: Option<String>,
    pub category_kind: Option<String>,
}

// ---- repo ports ---------------------------------------------------------------

#[async_trait]
pub trait AccountRepo: Send + Sync {
    async fn get(&self, id: i64) -> AppResult<Account>;
    /// Single-ticker shares accounts (see `SharesMeta`).
    async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>>;
    /// Distinct tickers ever traded on any brokerage account's holdings ledger.
    async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>>;
    async fn set_credit_limit(&self, account_id: i64, credit_limit_minor: i64) -> AppResult<()>;
    async fn set_original_amount(
        &self,
        account_id: i64,
        original_amount_minor: i64,
    ) -> AppResult<()>;
    async fn set_institution_if_unset(&self, account_id: i64, institution: &str) -> AppResult<()>;
}

#[async_trait]
pub trait BrokerageRepo: Send + Sync {
    async fn positions_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<HoldingRow>>;
    async fn wallet_balances_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<WalletRow>>;
    async fn account_tickers(&self, account_id: i64) -> AppResult<Vec<(String, String)>>;
    async fn earliest_activity_date(&self, account_id: i64) -> AppResult<Option<String>>;
}

#[async_trait]
pub trait StockPriceCacheRepo: Send + Sync {
    async fn get_at(
        &self,
        ticker: &str,
        exchange: &str,
        as_of: &str,
    ) -> AppResult<Option<StockPrice>>;
    async fn upsert(
        &self,
        ticker: &str,
        exchange: &str,
        as_of: &str,
        close: &str,
        ccy: &str,
    ) -> AppResult<()>;
}

#[async_trait]
pub trait ValuationRepo: Send + Sync {
    async fn upsert_from_brokerage(
        &self,
        account_id: i64,
        as_of: &str,
        value_minor: i64,
        ccy: &str,
    ) -> AppResult<()>;
    async fn upsert_from_provider(
        &self,
        account_id: i64,
        as_of: &str,
        value_minor: i64,
        ccy: &str,
    ) -> AppResult<()>;
}

/// The `currency_decimals` + `exchange_rates` loaders `crate::fx::Fx::load` uses.
#[async_trait]
pub trait FxRatesRepo: Send + Sync {
    async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>>;
    async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>>;
}

#[async_trait]
pub trait RuleRepo: Send + Sync {
    async fn load_contexts(&self) -> AppResult<Vec<TxCtx>>;
    async fn persist_run(
        &self,
        rule_id: Option<i64>,
        kind: &str,
        matched: i64,
        applications: Vec<PlannedApplication>,
    ) -> AppResult<RunResult>;
}

#[async_trait]
pub trait ReportRepo: Send + Sync {
    async fn base_currency(&self) -> AppResult<String>;
    async fn account_currencies(&self) -> AppResult<Vec<AccountCurrency>>;
    async fn transactions(&self) -> AppResult<Vec<LedgerTx>>;
    async fn valuations(&self) -> AppResult<Vec<LedgerValuation>>;
    async fn categories(&self) -> AppResult<Vec<ReportCategory>>;
    async fn spend_transactions(&self) -> AppResult<Vec<SpendTransaction>>;
    async fn active_accounts(&self) -> AppResult<Vec<ActiveAccount>>;
    async fn account(&self, id: i64) -> AppResult<AssetAccount>;
    async fn secured_liabilities(&self, asset_id: i64) -> AppResult<Vec<SecuredLiabilityAccount>>;
}

#[async_trait]
pub trait ProviderRepo: Send + Sync {
    async fn list(&self) -> AppResult<Vec<Provider>>;
    async fn account_currency(&self, account_id: i64) -> AppResult<String>;
    async fn import_transactions(
        &self,
        account_id: i64,
        account_currency: &str,
        provider_tag: &str,
        rows: &[ImportRow],
    ) -> AppResult<(i64, i64)>;
    async fn update_last_synced(&self, id: i64) -> AppResult<()>;
    async fn record_sync(
        &self,
        provider_id: i64,
        imported: i64,
        skipped: i64,
        status: &str,
        detail: Option<&str>,
    ) -> AppResult<ProviderSync>;
}

/// Backs the `ExchangeRateTask` scheduled poller.
#[async_trait]
pub trait ExchangeRateRepo: Send + Sync {
    async fn base_currency(&self) -> AppResult<String>;
    async fn known_currency_codes(&self) -> AppResult<std::collections::HashSet<String>>;
    async fn upsert_rate(
        &self,
        base_code: &str,
        quote_code: &str,
        rate: &str,
        as_of: &str,
    ) -> AppResult<()>;
}

/// Backs the `TransferLinkTask` scheduled poller.
#[async_trait]
pub trait TransferRepo: Send + Sync {
    async fn link_transfers(&self, window_days: i64) -> AppResult<i64>;
}
