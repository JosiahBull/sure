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
use rust_decimal::Decimal;
use serde_json::Value;

use sure_core::{
    Account, AccountEquity, AccountKind, AppResult, BulkUpdate, Category, CategoryKind,
    CategoryNode, Cron, CronRun, CronRunResult, Currency, DividendDetail, EquityExercise,
    EquityGrant, ForecastAssumption, ForecastEvent, ForecastTargetType, HoldingLot, ImportRecord,
    ImportSource, IncomeStream, LinkProviderAccount, LinkProviderGroup, LinkRequest, LotKind,
    McpMode, Merchant, NewCurrency, NewValuation, Ownership, Person, Provider, ProviderAccount,
    ProviderKind, ProviderSync, Rule, RuleApplicationDetail, RuleRun, RuleRunKind, RunResult,
    SaveAccount, SaveCategory, SaveCron, SaveExercise, SaveForecastAssumption, SaveForecastEvent,
    SaveGrant, SaveHoldingLot, SaveIncomeStream, SaveMerchant, SavePerson, SaveProvider, SaveRule,
    SaveTaxScale, SaveTransaction, Settings, StockPrice, StoredTaxScale, SyncOutcome, TaxScaleId,
    Transaction, TransferRequest, TxQuery, UpdateSettings, Valuation, ValuationQuery,
    VestingStatus,
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

// ---- provider ports ---------------------------------------------------------
// The seams for pulling data from external sources (banks, brokers, price/FX feeds). The
// trait definitions live here — the application core owns its ports — while the concrete
// adapters (CSV, Akahu, Yahoo Finance, Frankfurter) live in `sure-providers`, which
// depends on this crate to implement them. The composition root (`sure-server`) builds
// the adapters and injects them: a [`ProviderRegistry`] for the transaction providers, an
// `Arc<dyn StockPriceProvider>` / `Arc<dyn ExchangeRateProvider>` for the price/FX feeds.
// The API-surfaced DTOs (`ProviderAccount`, `ProviderKind`) live in `sure_core` with the
// other provider wire types; the internal ones below never leave the app boundary.

/// A normalized transaction pulled from an external source.
#[derive(Debug, Clone)]
pub struct ProviderTransaction {
    /// Stable identifier from the source, used to dedupe on re-sync.
    pub external_id: String,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    pub description: String,
    pub merchant: Option<String>,
    /// The source's own classification for this transaction (e.g. Akahu's NZFCC
    /// enrichment), if it has one — used to find-or-create a matching Sure category (and,
    /// for a newly-seen merchant, its default category) instead of leaving imported
    /// transactions uncategorized.
    pub category: Option<ProviderCategory>,
}

/// A merchant category as classified by the provider's own taxonomy.
#[derive(Debug, Clone)]
pub struct ProviderCategory {
    /// Specific category name (e.g. "Cafes and restaurants") — becomes a Sure category,
    /// nested under `group` when the source has one.
    pub name: String,
    /// Broader grouping (e.g. "Lifestyle"), if the source has one — becomes that
    /// category's parent.
    pub group: Option<String>,
    /// Flow direction hint applied when the category is first created. Most enrichment
    /// is spending, so `None` defaults to expense on the DAL side; a broker's dividend
    /// row sets `Income`, an internal wallet ↔ bank movement sets `Transfer` so it's
    /// excluded from spend/income reports.
    pub kind: Option<CategoryKind>,
}

/// Everything a provider needs to perform a sync. Cheap to copy (just references), so the
/// sync service can pass it to both [`TransactionProvider::fetch`] and
/// [`TransactionProvider::current_balance`].
#[derive(Debug, Clone, Copy)]
pub struct SyncContext<'a> {
    pub config: &'a Value,
    pub account_currency: &'a str,
    /// Optional inline payload supplied with the sync request (e.g. uploaded CSV).
    pub payload: Option<&'a str>,
    /// When this provider last completed a successful sync (RFC3339), if ever. Lets
    /// incremental providers avoid re-fetching full history on every run; providers that
    /// don't support incremental fetch (e.g. CSV) simply ignore it.
    pub last_synced_at: Option<&'a str>,
}

/// A point-in-time balance snapshot from an upstream source, plus whatever other
/// per-account facts happened to come back on the same fetch (a single-account refetch is
/// the natural place to also pick up slower-changing facts like a credit limit or an
/// institution name, rather than a separate round-trip for each).
#[derive(Debug, Clone)]
pub struct ProviderBalance {
    pub minor: i64,
    pub currency_code: String,
    /// Credit limit, in minor units, if the source reports one for this account (e.g. a
    /// credit card or revolving credit facility).
    pub limit_minor: Option<i64>,
    /// The financial institution's display name, if the source reports one and the local
    /// account doesn't already have one set (an existing value is never overwritten).
    pub institution: Option<String>,
    /// The original amount borrowed, in minor units, if the source reports one for this
    /// account (e.g. a mortgage or personal loan) — lets a paid-down percentage be shown.
    pub initial_principal_minor: Option<i64>,
}

/// The integration point for transaction sources. One method to fetch + normalize;
/// everything else (dedupe, persistence, audit) is handled generically by
/// [`crate::sync::SyncService`].
#[async_trait]
pub trait TransactionProvider: Send + Sync {
    /// Stable identifier used to select this provider (e.g. `"csv"`).
    fn kind(&self) -> &'static str;
    /// Human-facing description shown in the UI.
    fn description(&self) -> &'static str;
    /// Whether the provider expects an inline payload on sync (vs. fetching remotely).
    fn accepts_payload(&self) -> bool {
        false
    }
    /// Whether this provider can enumerate linkable upstream accounts (see
    /// [`Self::list_accounts`]).
    fn supports_account_discovery(&self) -> bool {
        false
    }
    /// Fetch and normalize transactions from the source.
    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>>;
    /// List upstream accounts available to link, for providers that support discovery.
    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> {
        Err(anyhow::anyhow!(
            "{} does not support discovering accounts",
            self.kind()
        ))
    }
    /// The upstream's live current balance for the account this sync is for, if this
    /// provider can report one. Used to keep the account's value accurate even when the
    /// transaction history alone doesn't reach back to when the account was opened (a
    /// mortgage's full term, say). Defaulting to `None` costs nothing for providers (like
    /// CSV) with no such concept.
    async fn current_balance(
        &self,
        _ctx: SyncContext<'_>,
    ) -> anyhow::Result<Option<ProviderBalance>> {
        Ok(None)
    }
}

/// The set of transaction-provider adapters the server knows about, injected into
/// [`crate::sync::SyncService`] and the poll task so the application core never names a
/// concrete provider. `sure-providers`' `Registry` is the implementation; the composition
/// root builds it.
pub trait ProviderRegistry: Send + Sync {
    /// The provider adapter registered for `kind`, if any.
    fn get(&self, kind: &str) -> Option<&dyn TransactionProvider>;
    /// Metadata for every registered provider kind (surfaced by the API).
    fn kinds(&self) -> Vec<ProviderKind>;
}

/// The integration point for *file* imports, as [`TransactionProvider`] is for feeds. One
/// method to recognise a blob and one to turn it into rows; everything after that — routing
/// each item to an account, deriving its cutover, reconciling, previewing, persisting — is
/// handled generically by [`crate::import::ImportService`].
///
/// The asymmetry with `TransactionProvider` is deliberate: `fetch` is `async` because it
/// waits on someone else's server, whereas parsing is pure CPU over bytes already in hand.
/// Keeping it synchronous means the service can hand the whole thing to one
/// `spawn_blocking` instead of every adapter remembering to.
pub trait ImportAdapter: Send + Sync {
    /// Which source this adapter reads.
    fn source(&self) -> ImportSource;
    /// Whether this blob looks like one of ours. Cheap — magic bytes, a zip's entry names, a
    /// file's first line — because every adapter's `sniff` may be called before one claims
    /// it. Never a full parse: a wrong guess here costs a better error message, not
    /// correctness, since [`Self::parse`] still has to succeed.
    fn sniff(&self, bytes: &[u8]) -> bool;
    /// Read the blob. Pure: no I/O, no clock, no database — which is what lets the same call
    /// serve a preview and a commit.
    fn parse(&self, bytes: &[u8]) -> anyhow::Result<ParsedUpload>;
    /// Recover the [`ParsedItem::source_account`] from an `external_id` this adapter once
    /// wrote, so a re-upload finds the account it went to last time.
    ///
    /// Needed because the ids are the only durable record of that pairing for everything
    /// imported before the `imports` table existed. `None` for a source whose ids don't
    /// encode it (a hand-written CSV chooses its own), which simply means that routing tier
    /// doesn't fire.
    fn source_account_of(&self, external_id: &str) -> Option<String>;
}

/// The set of import adapters the server knows about, injected into
/// [`crate::import::ImportService`] so the application core never names a concrete parser.
/// `sure-providers`' `ImportRegistry` is the implementation; the composition root builds it.
pub trait ImportRegistry: Send + Sync {
    /// The adapter for a source the caller named outright.
    fn get(&self, source: ImportSource) -> Option<&dyn ImportAdapter>;
    /// The first adapter that recognises these bytes, in the registry's own order — which
    /// must run the specific sources before the general one, since a bare CSV reader would
    /// otherwise claim every bank export too.
    fn detect(&self, bytes: &[u8]) -> Option<&dyn ImportAdapter>;
}

/// A single day's closing price for a ticker.
#[derive(Debug, Clone)]
pub struct StockPriceQuote {
    /// The trading day this close is for (daily resolution is all Sure needs).
    pub as_of: NaiveDate,
    pub close: Decimal,
    /// The currency the price is quoted in (e.g. the exchange's listing currency).
    pub currency_code: String,
}

/// The integration point for pulling historical daily stock prices from an upstream
/// source. Implemented by `sure-providers`' `YahooFinanceProvider`.
#[async_trait]
pub trait StockPriceProvider: Send + Sync {
    /// Stable identifier for this source (e.g. `"yahoo_finance"`).
    fn kind(&self) -> &'static str;
    /// Human-facing description.
    fn description(&self) -> &'static str;
    /// Fetch daily closes for `ticker` between `from` and `to` (inclusive). `exchange` is
    /// a free-text hint (e.g. `"NZX"`, from an account's `SharesMeta.exchange`) used to
    /// resolve exchange-specific symbol conventions; `None` or an unrecognised value falls
    /// back to the bare ticker.
    async fn fetch_daily_prices(
        &self,
        ticker: &str,
        exchange: Option<&str>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<StockPriceQuote>>;
}

/// A single quoted rate: 1 unit of the requested base currency equals `rate` units of
/// `quote_code`.
#[derive(Debug, Clone)]
pub struct ExchangeRateQuote {
    pub quote_code: String,
    pub rate: Decimal,
    /// ISO-8601 date the rate was quoted as of (the upstream's reference date, not the
    /// time it was fetched).
    pub as_of: String,
}

/// The integration point for pulling live currency exchange rates from an upstream source.
/// Implemented by `sure-providers`' `FrankfurterProvider`.
#[async_trait]
pub trait ExchangeRateProvider: Send + Sync {
    /// Stable identifier for this source (e.g. `"frankfurter"`).
    fn kind(&self) -> &'static str;
    /// Human-facing description.
    fn description(&self) -> &'static str;
    /// Fetch every available rate quoted against `base` (an ISO 4217 code).
    async fn fetch_rates(&self, base: &str) -> anyhow::Result<Vec<ExchangeRateQuote>>;
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

/// One lot, reduced to what the average-cost-basis walk needs (see `crate::brokerage`).
#[derive(Debug, Clone)]
pub struct CostLotRow {
    pub ticker: String,
    pub exchange: String,
    pub currency_code: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: LotKind,
}

/// Rolling 30-days-to-`as_of` activity — see [`sure_core::BrokerageActivity30d`].
#[derive(Debug, Clone, Default)]
pub struct Activity30dRow {
    pub contributions_minor: i64,
    pub withdrawals_minor: i64,
    pub trades: i64,
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

/// The current exchange rate for one pair — the latest dated row of the series. `rate` is
/// exact decimal text.
#[derive(Debug, Clone)]
pub struct ExchangeRateRow {
    pub base_code: String,
    pub quote_code: String,
    pub rate: String,
    /// Upstream's reference date for this rate (ISO-8601 date). Carried across the port
    /// because the poller only writes on success: without a date, a feed that has been down
    /// for a year is indistinguishable from one that polled this morning, and every figure
    /// derived from it reads as current. [`crate::fx::Fx::rates_as_of`] surfaces the newest
    /// of these on the reports that convert.
    pub as_of: String,
}

/// An account and its currency (all accounts, including archived) — for net-worth history.
#[derive(Debug, Clone)]
pub struct AccountCurrency {
    pub id: i64,
    pub currency_code: String,
    pub ownership: Ownership,
    /// Carried, never applied here. Dropping the account is the *report's* decision — see
    /// [`crate::reports::ReportService`], which filters at its three call sites so each can
    /// answer differently (net worth omits it, balances lists it without totalling it).
    pub excluded_from_net_worth: bool,
}

/// A non-archived account, for the current-balances report.
#[derive(Debug, Clone)]
pub struct ActiveAccount {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
    pub ownership: Ownership,
    /// The balances report lists this row either way; only the roll-up leaves it out.
    pub excluded_from_net_worth: bool,
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
    pub kind: AccountKind,
    pub currency_code: String,
}

/// A transaction reduced to what a running balance needs.
///
/// `currency_code` is part of that: one `accounts` row can hold several currencies (a
/// brokerage with an NZD, an AUD and a USD wallet, each exposed by the upstream as its own
/// account), and adding those amounts without their currency is adding USD cents to NZD cents.
/// `sure_app::reports::account_value_at` totals each currency separately and converts the
/// subtotals.
#[derive(Debug, Clone)]
pub struct LedgerTx {
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
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
    pub kind: CategoryKind,
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
    pub account_id: i64,
    pub account_name: String,
    pub account_kind: AccountKind,
    pub merchant_id: Option<i64>,
    /// The merchant record's name, or — where the row has no merchant record — whatever
    /// payee text the feed wrote. Already coalesced, so a caller grouping by payee sees one
    /// field rather than having to decide between an id and a string per row.
    pub merchant: Option<String>,
    /// Already-resolved effective attribution (override, else the account's owner).
    pub attribution: Ownership,
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
    pub account_kind: AccountKind,
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
    pub category_kind: Option<CategoryKind>,
    /// Excluded from spend/income reports, but still counted towards balances and net worth
    /// (see `sure_app::reports::load_ledger`, which filters nothing). What an opening-balance
    /// row needs: it moves the account's value without being money earned or spent.
    pub is_one_off: bool,
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
    pub kind: LotKind,
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

/// What one [`ImportAdapter::parse`] made of a blob: the things it found, and anything
/// worth saying about the blob as a whole (an entry that wasn't an export, a file that
/// wouldn't read while its neighbours did).
#[derive(Debug, Clone)]
pub struct ParsedUpload {
    pub source: ImportSource,
    /// One per thing the upload described — one bank account's export, one loan, one
    /// brokerage. Several only where a source can carry several (an ASB zip).
    pub items: Vec<ParsedItem>,
    pub warnings: Vec<String>,
}

/// One thing inside a parsed upload, with everything the pipeline needs to place it.
///
/// The optional fields are how an adapter opts into a capability rather than declaring one:
/// a source "supports" reconciliation by knowing its `stated_closing_minor`, and "supports"
/// holdings by returning [`ParsedExtras::Brokerage`]. No adapter answers a question about
/// itself that its output can answer instead.
#[derive(Debug, Clone)]
pub struct ParsedItem {
    /// How the source names this thing (`12-3456-0000123-50`, `012-345-678-SLS004`). The key
    /// an assignment addresses, and what the previous-import tier matches on.
    pub source_account: String,
    /// The source's own label for it, where it has one — ASB's product name.
    pub label: Option<String>,
    /// Whose the source says it is, where the source says so at all. Distinct from
    /// [`Self::label`] even where an adapter fills both from one value: this one is *matched*
    /// against the household roster (see `crate::import::routing::match_by_holder`), so a
    /// product name must never be put here.
    ///
    /// Only myIR sets it today, and only because it has to: an SLS account id appears nowhere
    /// in Sure, so with two student loans in a household nothing else in the file tells them
    /// apart.
    pub holder: Option<String>,
    /// The file(s) in the upload these rows came from.
    pub sources: Vec<String>,
    /// Every row, ids assigned, before anything is held back. The pipeline does the holding
    /// back, because only it knows the target account's cutover.
    pub rows: Vec<ImportRow>,
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    /// The balance the export states it closes at, where it states one. Present turns on the
    /// reconciliation check, which needs a known figure to compare against.
    pub stated_closing_minor: Option<i64>,
    /// A row putting the balance the account held immediately *before* the first row onto the
    /// ledger, for a source whose export says (or implies) what that was.
    ///
    /// Built by the adapter, not derived here, for two reasons. It has to be worked out over
    /// **every** row in the file including any the cutover later holds back — those movements
    /// still happened, they are just already on the ledger from the live feed — and this layer
    /// only ever sees the rows that survived. And a source that *states* an opening balance
    /// outright (ASB's running-balance shape does) should use that figure rather than arithmetic
    /// that can only agree with it.
    ///
    /// Whether it is actually written is this layer's call: the caller can opt out, and an
    /// account with history from before that date gets none, because then it would not be an
    /// opening balance but a large invented movement in the middle of the ledger.
    pub opening_balance: Option<ImportRow>,
    /// Records other than transactions.
    pub extras: ParsedExtras,
    pub warnings: Vec<String>,
}

/// The non-transaction records a source produces. A closed set, matched exhaustively where
/// the pipeline decides what to write, so a fifth source that brings something new is a
/// compile error at that one place rather than rows silently dropped.
#[derive(Debug, Clone)]
pub enum ParsedExtras {
    /// Transactions only — every source but Sharesies.
    None,
    Brokerage {
        holdings: Vec<HoldingImport>,
        dividends: Vec<DividendImport>,
    },
}

/// One import to record. Written only for an item that was actually placed and committed — a dry
/// run and an unrouted item both leave nothing behind to describe.
#[derive(Debug, Clone)]
pub struct NewImport {
    pub account_id: i64,
    pub source: ImportSource,
    pub provider_tag: String,
    pub source_account: Option<String>,
    pub filenames: Vec<String>,
    pub imported: i64,
    pub skipped: i64,
    pub held_back: i64,
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    pub cutover: Option<String>,
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
    /// Attribute one account to a household member (or to the household, or to nobody).
    async fn set_ownership(&self, id: i64, ownership: Ownership) -> AppResult<Account>;
    /// Keep an account's balance out of the household's net worth, or put it back. Its own
    /// method rather than a field on `SaveAccount` — see `sure_core::SetExcludedFromNetWorth`.
    async fn set_excluded_from_net_worth(&self, id: i64, excluded: bool) -> AppResult<Account>;
    /// The same, for many accounts at once; returns how many were changed.
    async fn set_ownership_bulk(&self, ids: &[i64], ownership: Ownership) -> AppResult<u64>;
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
    /// Record the account number a feed reports, leaving an existing one alone. It is the only
    /// identifier two accounts at one bank don't share, and what the ASB import routes an
    /// export by — see [`crate::sync::SyncService::adopt_account_numbers`].
    async fn set_account_number_if_unset(
        &self,
        account_id: i64,
        account_number: &str,
    ) -> AppResult<()>;
}

#[async_trait]
pub trait PersonRepo: Send + Sync {
    async fn list(&self) -> AppResult<Vec<Person>>;
    async fn get(&self, id: i64) -> AppResult<Person>;
    async fn create(&self, input: SavePerson) -> AppResult<Person>;
    async fn update(&self, id: i64, input: SavePerson) -> AppResult<Person>;
    /// Refused with a conflict while any account is still attributed to them.
    async fn delete(&self, id: i64) -> AppResult<()>;
}

#[async_trait]
pub trait BrokerageRepo: Send + Sync {
    async fn positions_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<HoldingRow>>;
    async fn wallet_balances_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<WalletRow>>;
    async fn lots_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<CostLotRow>>;
    async fn activity_30d(&self, account_id: i64, as_of: &str) -> AppResult<Activity30dRow>;
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
    /// Delete every holding lot on this account that `provider_tag` imported — the holdings
    /// half of undoing a bulk upload, alongside
    /// [`TransactionRepo::delete_by_provider`]. Returns how many went.
    async fn delete_holdings_by_provider(
        &self,
        account_id: i64,
        provider_tag: &str,
    ) -> AppResult<i64>;
    /// The same for dividends. Their withholdings go with them (`ON DELETE CASCADE`), because a
    /// withholding is a detail of the dividend rather than a record in its own right.
    async fn delete_dividends_by_provider(
        &self,
        account_id: i64,
        provider_tag: &str,
    ) -> AppResult<i64>;
}

/// The log of what file imports have done — `provider_syncs` for uploads.
#[async_trait]
pub trait ImportHistoryRepo: Send + Sync {
    async fn record(&self, entry: NewImport) -> AppResult<()>;
    /// Newest first. `account_id` narrows it to one account, which is the only read a panel does.
    async fn list(&self, account_id: Option<i64>) -> AppResult<Vec<ImportRecord>>;
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
    /// An account's valuations, newest first, narrowed by `q`. Unfiltered (`Default`) is the
    /// right call for anything deriving movements from the series — see
    /// `crate::tasks::balance_delta`, which needs every row or it derives the wrong deltas.
    async fn list_for_account(
        &self,
        account_id: i64,
        q: ValuationQuery,
    ) -> AppResult<Vec<Valuation>>;
    async fn create(&self, account_id: i64, input: NewValuation) -> AppResult<Valuation>;
    /// Edit a valuation someone entered by hand. Refuses a derived one — see
    /// `sure_dal::valuations::update`.
    async fn update(&self, id: i64, input: NewValuation) -> AppResult<Valuation>;
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
    /// One row per currency pair, each already the latest date on record — the reduction
    /// belongs in SQL, not in a caller re-scanning a growing dated series.
    async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>>;
}

#[async_trait]
pub trait RuleRepo: Send + Sync {
    async fn load_contexts(&self) -> AppResult<Vec<TxCtx>>;
    /// The same, narrowed to transactions that have no category yet.
    ///
    /// What the unattended pass after an import or a sync evaluates
    /// (`sure_app::rules::RuleService::categorize_new`). Narrowing it in SQL rather than
    /// filtering [`Self::load_contexts`] is the whole point: that one materialises every
    /// transaction ever recorded, and this runs on every sync of every provider.
    ///
    /// A row already carrying a category is excluded rather than merely left unchanged, so
    /// the automatic pass has no way to reach one — see `categorize_new` for why that
    /// bound, and not `overwrite_manual`, is what makes running it unattended safe.
    async fn load_uncategorized_contexts(&self) -> AppResult<Vec<TxCtx>>;
    async fn persist_run(
        &self,
        rule_id: Option<i64>,
        kind: RuleRunKind,
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
    /// Every account, archived **and** excluded-from-net-worth included. The exclusion is a
    /// fact carried on the row rather than applied here, so a caller that wants the household
    /// total must drop them itself — which is what `net_worth_inputs` does.
    async fn account_currencies(&self) -> AppResult<Vec<AccountCurrency>>;
    /// The transactions needed to value every account on any date from `from` onwards.
    ///
    /// `Some(from)` is a *window*, not a filter on what the caller may see: an implementation
    /// must return enough for `sure_app::reports::account_value_at` to answer, for every date
    /// `d >= from`, both "what is the running total of everything posted on or before `d`" and
    /// "had this account been posted to before `d`". Returning every row (as an in-memory fake
    /// does) satisfies that; so does returning the rows on/after `from` plus **one seed row per
    /// account and currency** whose amount is the sum of everything before it in that currency
    /// and whose date is the latest of them — which is what `SqliteStore` does, and is the
    /// difference between a report touching its window and a report materialising a 500k-row
    /// ledger 64 times over. Seeding per account alone would collapse a multi-currency
    /// account's history into one meaningless sum; see [`LedgerTx`].
    ///
    /// There is no upper bound by design: the valuation-anchor reconstruction reads *forward*
    /// from the date being reported on to the account's earliest valuation, which is routinely
    /// later than the window's end. See `sure_dal::reports::transactions`.
    ///
    /// `None` means the whole table, for the forecast — it fits trends over all of history.
    async fn transactions(&self, from: Option<NaiveDate>) -> AppResult<Vec<LedgerTx>>;
    /// The valuations needed from `from` onwards: every row as of it or later, plus the latest
    /// one before it per account (a valuation is a level that carries forward, so the newest
    /// earlier row is the account's opening value — see `sure_dal::reports::valuations`).
    /// `None` means the whole table.
    async fn valuations(&self, from: Option<NaiveDate>) -> AppResult<Vec<LedgerValuation>>;
    async fn categories(&self) -> AppResult<Vec<ReportCategory>>;
    /// Transactions posted within `from ..= to`. A plain window: the spend reports total the
    /// movements inside the period and never look outside it. Implementations may return a
    /// superset — `sure_app::reports::load_spend` re-checks every parsed date.
    async fn spend_transactions(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> AppResult<Vec<SpendTransaction>>;
    /// The earliest transaction date on record, for defaulting an unbounded report window.
    async fn earliest_transaction_date(&self) -> AppResult<Option<String>>;
    /// The earliest valuation date on record. Net worth's default window start is the earlier
    /// of this and [`Self::earliest_transaction_date`] — an account can be valued before it is
    /// ever transacted on, and that day belongs in the series.
    async fn earliest_valuation_date(&self) -> AppResult<Option<String>>;
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
        status: SyncOutcome,
        detail: Option<&str>,
    ) -> AppResult<ProviderSync>;
}

/// Backs the `ExchangeRateTask` scheduled poller.
#[async_trait]
pub trait ExchangeRateRepo: Send + Sync {
    async fn base_currency(&self) -> AppResult<String>;
    async fn known_currency_codes(&self) -> AppResult<std::collections::HashSet<String>>;
    /// Record one pair's rate for one date, into the same table [`FxRatesRepo`] reads. The
    /// two used to address different tables, so polling had no effect on any figure —
    /// `sure_dal::exchange_rates`' port-crossing test exists to keep them joined.
    ///
    /// `as_of` precedes `rate` to match the `exchange_rates` column order, the DAL function
    /// behind this, and `upsert_stock_price`'s identical date-then-value shape. They are both
    /// `&str`, so a transposition would compile and store the date as the rate: keeping every
    /// layer in one order is what stops that, rather than a comment at the adapter.
    async fn upsert_rate(
        &self,
        base_code: &str,
        quote_code: &str,
        as_of: &str,
        rate: &str,
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
    /// The earliest `posted_at` on this account owned by a feed other than
    /// `exclude_provider` — the cutover a manual historical import must stop at, so one
    /// movement isn't posted twice by two sources.
    async fn earliest_posted_at_from_other_feed(
        &self,
        account_id: i64,
        exclude_provider: &str,
    ) -> AppResult<Option<String>>;
    /// Delete every transaction on this account that `provider_tag` imported. Undo for a
    /// bulk upload.
    async fn delete_by_provider(&self, account_id: i64, provider_tag: &str) -> AppResult<i64>;
    /// Every amount on this account, summed — against its recorded balance, whether the
    /// ledger reconciles.
    async fn sum_amount_minor(&self, account_id: i64) -> AppResult<i64>;
    /// The earliest `posted_at` on this account, from any source at all. Tells an importer
    /// whether it would be placing an opening balance ahead of the ledger or into the middle
    /// of it.
    async fn earliest_posted_at(&self, account_id: i64) -> AppResult<Option<String>>;
    /// One `external_id` per account, over the rows tagged by a provider whose tag starts
    /// with `provider_prefix`. Lets a manual importer recover which upstream account it
    /// previously imported into which local one, from the ids it wrote.
    async fn sample_external_ids(&self, provider_prefix: &str) -> AppResult<Vec<(i64, String)>>;
    /// `(account_id, date, amount_minor)` for every transaction on these accounts, dates as
    /// `YYYY-MM-DD`. The raw material for matching an uploaded bank export to the account it
    /// belongs to when nothing recorded that account's number: over the window both cover, a
    /// run of dated amounts is close to a fingerprint. Capped at `limit` rows in total, oldest
    /// first, so one enormous account can't make the comparison unbounded.
    async fn amounts_for_matching(
        &self,
        account_ids: &[i64],
        limit: i64,
    ) -> AppResult<Vec<(i64, String, i64)>>;
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
    /// Just the stored MCP mode. Read on every MCP request, which is why it is a method of
    /// its own rather than a field plucked off [`Self::get`].
    async fn mcp_mode(&self) -> AppResult<McpMode>;
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

/// Forecast assumption overrides, plus the one read query nothing else exposes
/// (`trailing_dividends_minor`, for the dividend-yield default). The resolution logic —
/// which knob wins between an override, an existing cron's rate, and a historical
/// default — lives in `crate::forecast::ForecastService`, which also depends on
/// `ReportRepo`, `AccountRepo`, `CronRepo`, and `FxRatesRepo` for the read side.
#[async_trait]
pub trait ForecastRepo: Send + Sync {
    async fn list_assumptions(&self) -> AppResult<Vec<ForecastAssumption>>;
    async fn upsert_assumption(
        &self,
        input: SaveForecastAssumption,
    ) -> AppResult<ForecastAssumption>;
    async fn clear_assumption(
        &self,
        target_type: ForecastTargetType,
        target_id: i64,
    ) -> AppResult<()>;
    /// Sum of dividend cash paid to `account_id` on or after `since` (ISO-8601 date).
    async fn trailing_dividends_minor(&self, account_id: i64, since: &str) -> AppResult<i64>;
    /// Every event with its effects and relations attached, soonest first.
    async fn list_events(&self) -> AppResult<Vec<ForecastEvent>>;
    async fn get_event(&self, id: i64) -> AppResult<ForecastEvent>;
    /// Create the event, its effects and its relations in one transaction, refusing a relation set
    /// that would close a cycle.
    async fn create_event(&self, input: SaveForecastEvent) -> AppResult<ForecastEvent>;
    /// Full replace, effects and relations included.
    async fn update_event(&self, id: i64, input: SaveForecastEvent) -> AppResult<ForecastEvent>;
    /// Refused with a conflict when something only happens *if* this does.
    async fn delete_event(&self, id: i64) -> AppResult<()>;

    // ---- per-person income streams -------------------------------------------------
    //
    // On `ForecastRepo` rather than a port of their own: nothing outside the forecast reads a
    // stream, and one port per aggregate is the rule this file already follows. If a household
    // *income report* ever wants them, that is when to extract an `IncomeRepo`.

    /// Every stream with its dated pay-scale steps attached, by person then sort order.
    async fn list_income_streams(&self) -> AppResult<Vec<IncomeStream>>;
    async fn get_income_stream(&self, id: i64) -> AppResult<IncomeStream>;
    /// Create the stream and its whole step schedule in one transaction.
    async fn create_income_stream(
        &self,
        person_id: i64,
        input: SaveIncomeStream,
    ) -> AppResult<IncomeStream>;
    /// Full replace, steps included — a step omitted from `input` is deleted.
    async fn update_income_stream(
        &self,
        id: i64,
        input: SaveIncomeStream,
    ) -> AppResult<IncomeStream>;
    /// Refused with a conflict naming the forecast changes whose effects target it.
    async fn delete_income_stream(&self, id: i64) -> AppResult<()>;

    /// Every stored tax scale, oldest first.
    ///
    /// The forecast reads these rather than `sure_core::tax`'s constants, which are now only a seed
    /// and a fallback — otherwise editing a rate in settings would change nothing, which is the
    /// whole point of storing them.
    async fn list_tax_scales(&self) -> AppResult<Vec<StoredTaxScale>>;
    async fn create_tax_scale(
        &self,
        scale_id: TaxScaleId,
        input: SaveTaxScale,
    ) -> AppResult<StoredTaxScale>;
    async fn update_tax_scale(&self, id: i64, input: SaveTaxScale) -> AppResult<StoredTaxScale>;
    /// Refused when it is the last one: an empty table taxes every gross salary at nothing.
    async fn delete_tax_scale(&self, id: i64) -> AppResult<()>;
    /// Throw the stored scales away and re-seed from the built-in figures.
    async fn restore_tax_scales(&self) -> AppResult<Vec<StoredTaxScale>>;

    /// Money *into* an account since `from`, for finding a salary already in the ledger.
    ///
    /// One narrow method here rather than a dependency on the whole `TransactionRepo`: the forecast
    /// wants one query, and taking the twelve-method port for it would make every test fake carry
    /// eleven `unreachable!()`s that say nothing.
    async fn income_transactions(
        &self,
        from: &str,
        account_id: Option<i64>,
    ) -> AppResult<Vec<Transaction>>;
}

/// The config export/import blob is treated as opaque JSON at this boundary — its shape
/// (`sure_dal::snapshot::Snapshot`) is a DAL-internal persistence detail, not domain
/// vocabulary, so there's no plain port type to maintain here.
#[async_trait]
pub trait SnapshotRepo: Send + Sync {
    /// The whole snapshot as **already-serialised JSON bytes**, to be sent as the response body
    /// verbatim.
    ///
    /// Not a `serde_json::Value`, deliberately: the blob is a full copy of the database, and
    /// building the intermediate tree meant holding roughly three copies of it at once (the
    /// rows, the `Value`, the response body) for every concurrent request. The bytes let the
    /// implementation write each table out and drop it — see `sure_dal::snapshot::export_bytes`,
    /// which also states the residual peak.
    async fn export(&self) -> AppResult<Vec<u8>>;
    async fn import(&self, snapshot: serde_json::Value) -> AppResult<serde_json::Value>;
}
