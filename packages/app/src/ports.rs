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

use sure_core::{
    Account, AccountEquity, AppResult, BulkUpdate, Category, CategoryNode, Cron, CronRun,
    CronRunResult, Currency, DividendDetail, EquityExercise, EquityGrant, HoldingLot,
    LinkProviderAccount, LinkProviderGroup, LinkRequest, Merchant, NewCurrency, NewValuation,
    Provider, ProviderSync, Rule, RuleApplicationDetail, RuleRun, RunResult, SaveAccount,
    SaveCategory, SaveCron, SaveExercise, SaveGrant, SaveHoldingLot, SaveMerchant, SaveProvider,
    SaveRule, SaveTransaction, Settings, StockPrice, Transaction, TransferRequest, TxQuery,
    UpdateSettings, Valuation, VestingStatus,
};

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

/// A parsed holding lot ready to persist (e.g. from a Sharesies export).
#[derive(Debug, Clone)]
pub struct HoldingImport {
    pub ticker: String,
    pub exchange: String,
    pub name: Option<String>,
    pub currency_code: String,
    pub trade_date: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: String,
    pub external_id: String,
}

#[derive(Debug, Clone)]
pub struct WithholdingImport {
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

#[derive(Debug, Clone)]
pub struct DividendImport {
    pub ticker: String,
    pub exchange: String,
    pub record_date: Option<String>,
    pub paid_date: String,
    pub shares_held: Option<f64>,
    pub gross_amount_minor: i64,
    pub net_amount_minor: i64,
    pub currency_code: String,
    pub external_id: String,
    pub withholdings: Vec<WithholdingImport>,
}

/// Counts from persisting one parsed brokerage export.
#[derive(Debug, Clone, Default)]
pub struct ImportCounts {
    pub transactions_imported: i64,
    pub transactions_skipped: i64,
    pub holdings_imported: i64,
    pub holdings_skipped: i64,
    pub dividends_imported: i64,
    pub dividends_skipped: i64,
}

// ---- repo ports ---------------------------------------------------------------

#[async_trait]
pub trait AccountRepo: Send + Sync {
    async fn list(&self, include_archived: bool) -> AppResult<Vec<Account>>;
    async fn get(&self, id: i64) -> AppResult<Account>;
    async fn create(&self, input: SaveAccount) -> AppResult<Account>;
    async fn update(&self, id: i64, input: SaveAccount) -> AppResult<Account>;
    async fn delete(&self, id: i64) -> AppResult<()>;
    async fn set_secured_by(&self, id: i64, target: Option<i64>) -> AppResult<Account>;
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
    async fn list_holdings(&self, account_id: i64) -> AppResult<Vec<HoldingLot>>;
    async fn create_holding(&self, account_id: i64, input: SaveHoldingLot)
        -> AppResult<HoldingLot>;
    async fn delete_holding(&self, id: i64) -> AppResult<()>;
    async fn list_dividends(&self, account_id: i64) -> AppResult<Vec<DividendDetail>>;
    #[allow(clippy::too_many_arguments)]
    async fn import_export(
        &self,
        account_id: i64,
        account_currency: &str,
        provider_tag: &str,
        wallet_rows: &[ImportRow],
        holdings: &[HoldingImport],
        dividends: &[DividendImport],
    ) -> AppResult<ImportCounts>;
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
    async fn list_for_account(&self, account_id: i64) -> AppResult<Vec<Valuation>>;
    async fn create(&self, account_id: i64, input: NewValuation) -> AppResult<Valuation>;
    async fn delete(&self, id: i64) -> AppResult<()>;
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
    async fn list(&self) -> AppResult<Vec<Rule>>;
    async fn enabled_rules(&self) -> AppResult<Vec<Rule>>;
    async fn get(&self, id: i64) -> AppResult<Rule>;
    async fn create(&self, input: SaveRule) -> AppResult<Rule>;
    async fn update(&self, id: i64, input: SaveRule) -> AppResult<Rule>;
    async fn delete(&self, id: i64) -> AppResult<()>;
    async fn list_runs(&self) -> AppResult<Vec<RuleRun>>;
    async fn run_applications(&self, run_id: i64) -> AppResult<Vec<RuleApplicationDetail>>;
    async fn undo_run(&self, run_id: i64) -> AppResult<RunResult>;
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
    async fn get(&self, id: i64) -> AppResult<Provider>;
    async fn create(&self, input: SaveProvider) -> AppResult<Provider>;
    async fn update(&self, id: i64, input: SaveProvider) -> AppResult<Provider>;
    async fn delete(&self, id: i64) -> AppResult<()>;
    async fn link(&self, input: LinkProviderAccount) -> AppResult<Provider>;
    async fn link_group(&self, input: LinkProviderGroup) -> AppResult<Vec<Provider>>;
    async fn list_syncs(&self, provider_id: i64) -> AppResult<Vec<ProviderSync>>;
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

// ---- thin-CRUD aggregate ports (Phase 3c) --------------------------------------
//
// These aggregates have no branching logic worth unit-testing in isolation — a
// handler calls one repo method and forwards the result — so there's no service
// struct here, just the port. `sure-api`'s routes hold `Arc<dyn Repo>` directly.

#[async_trait]
pub trait TransactionRepo: Send + Sync {
    async fn list(&self, q: TxQuery) -> AppResult<Vec<Transaction>>;
    async fn get(&self, id: i64) -> AppResult<Transaction>;
    async fn create(&self, input: SaveTransaction) -> AppResult<Transaction>;
    async fn update(&self, id: i64, input: SaveTransaction) -> AppResult<Transaction>;
    async fn delete(&self, id: i64) -> AppResult<()>;
    async fn bulk_update(&self, input: BulkUpdate) -> AppResult<i64>;
    async fn bulk_delete(&self, ids: &[i64]) -> AppResult<i64>;
    async fn link(&self, id: i64, req: LinkRequest) -> AppResult<Transaction>;
    async fn unlink(&self, id: i64) -> AppResult<Transaction>;
    async fn create_transfer(&self, req: TransferRequest) -> AppResult<Vec<Transaction>>;
}

#[async_trait]
pub trait CategoryRepo: Send + Sync {
    async fn list(&self) -> AppResult<Vec<Category>>;
    async fn tree(&self) -> AppResult<Vec<CategoryNode>>;
    async fn create(&self, input: SaveCategory) -> AppResult<Category>;
    async fn update(&self, id: i64, input: SaveCategory) -> AppResult<Category>;
    async fn delete(&self, id: i64) -> AppResult<()>;
}

#[async_trait]
pub trait MerchantRepo: Send + Sync {
    async fn list(&self) -> AppResult<Vec<Merchant>>;
    async fn create(&self, input: SaveMerchant) -> AppResult<Merchant>;
    async fn update(&self, id: i64, input: SaveMerchant) -> AppResult<Merchant>;
    async fn delete(&self, id: i64) -> AppResult<()>;
}

#[async_trait]
pub trait CurrencyRepo: Send + Sync {
    async fn list(&self) -> AppResult<Vec<Currency>>;
    async fn upsert(&self, input: NewCurrency) -> AppResult<Currency>;
    async fn delete(&self, code: &str) -> AppResult<()>;
}

#[async_trait]
pub trait SettingsRepo: Send + Sync {
    async fn get(&self) -> AppResult<Settings>;
    async fn update(&self, input: UpdateSettings) -> AppResult<Settings>;
}

#[async_trait]
pub trait EquityRepo: Send + Sync {
    async fn list_grants(&self, account_id: i64) -> AppResult<Vec<EquityGrant>>;
    async fn create_grant(&self, account_id: i64, input: SaveGrant) -> AppResult<EquityGrant>;
    async fn update_grant(&self, id: i64, input: SaveGrant) -> AppResult<EquityGrant>;
    async fn delete_grant(&self, id: i64) -> AppResult<()>;
    async fn list_exercises(&self, grant_id: i64) -> AppResult<Vec<EquityExercise>>;
    async fn create_exercise(
        &self,
        grant_id: i64,
        input: SaveExercise,
    ) -> AppResult<EquityExercise>;
    async fn delete_exercise(&self, id: i64) -> AppResult<()>;
    async fn grant_vesting(&self, id: i64, as_of: Option<&str>) -> AppResult<VestingStatus>;
    async fn account_equity(&self, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity>;
    async fn revalue(&self, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity>;
}

#[async_trait]
pub trait CronRepo: Send + Sync {
    async fn list(&self) -> AppResult<Vec<Cron>>;
    async fn create(&self, input: SaveCron) -> AppResult<Cron>;
    async fn update(&self, id: i64, input: SaveCron) -> AppResult<Cron>;
    async fn delete(&self, id: i64) -> AppResult<()>;
    async fn list_runs(&self, cron_id: i64) -> AppResult<Vec<CronRun>>;
    async fn run_one(&self, id: i64, to: Option<&str>) -> AppResult<CronRunResult>;
    async fn run_all(&self, to: Option<&str>) -> AppResult<CronRunResult>;
    async fn undo_run(&self, run_id: i64) -> AppResult<()>;
}

/// The config export/import blob is treated as opaque JSON at this boundary — its shape
/// (`sure_dal::snapshot::Snapshot`) is a DAL-internal persistence detail, not domain
/// vocabulary, so there's no plain port type to maintain here.
#[async_trait]
pub trait SnapshotRepo: Send + Sync {
    async fn export(&self) -> AppResult<serde_json::Value>;
    async fn import(&self, snapshot: serde_json::Value) -> AppResult<serde_json::Value>;
}
