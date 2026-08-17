use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::forecast::ForecastService;
use sure_app::import::ImportService;
use sure_app::income_match::IncomeMatchService;
use sure_app::ports::{
    AccountRepo, CategoryRepo, CronRepo, CurrencyRepo, EquityRepo, IncomeRepo, MerchantRepo,
    PersonRepo, PropertyEstimateProvider, ProviderRegistry, ProviderRepo, SettingsRepo,
    SnapshotRepo, StockPriceCacheRepo, StockPriceProvider, TransactionRepo, ValuationRepo,
};
use sure_app::reports::ReportService;
use sure_app::rules::RuleService;
use sure_app::sync::SyncService;

/// Shared application state handed to every handler. Cheap to clone — every field is an
/// `Arc` (the four logic-heavy services, or a repo port trait object directly for thin
/// CRUD aggregates that have no orchestration logic worth a named service). Every field
/// type is defined by `sure-app` (a service struct or a `sure_app::ports` trait object),
/// so this struct — and `sure-api` as a whole — never names `sure_dal` or `sqlx`.
/// Built by the composition root (the `sure-server` crate), which constructs the one
/// `SqliteStore` behind every field via a struct literal (every field here is `pub`).
#[derive(Clone)]
pub struct AppState {
    pub brokerage: Arc<BrokerageService>,
    /// The one pipeline every file upload goes through — sniff, parse, route, hold back,
    /// reconcile, write. It owns the import adapters (injected by the composition root), which
    /// is why `sure-api` no longer names a parser.
    pub import: Arc<ImportService>,
    pub reports: Arc<ReportService>,
    pub forecast: Arc<ForecastService>,
    pub rules: Arc<RuleService>,
    pub sync: Arc<SyncService>,
    pub stock_prices: Arc<dyn StockPriceCacheRepo>,
    pub accounts: Arc<dyn AccountRepo>,
    pub transactions: Arc<dyn TransactionRepo>,
    pub categories: Arc<dyn CategoryRepo>,
    pub merchants: Arc<dyn MerchantRepo>,
    pub people: Arc<dyn PersonRepo>,
    pub currencies: Arc<dyn CurrencyRepo>,
    pub settings: Arc<dyn SettingsRepo>,
    pub valuations: Arc<dyn ValuationRepo>,
    pub equity: Arc<dyn EquityRepo>,
    /// Income streams, tax scales and matched payments — thin CRUD plus the matcher's storage,
    /// so it is the repo directly.
    pub income: Arc<dyn IncomeRepo>,
    /// The matching/reconstruction logic behind the manual-link and rematch endpoints — the
    /// same pass the scheduled `income_match` task runs on its timer.
    pub income_match: Arc<IncomeMatchService>,
    pub crons: Arc<dyn CronRepo>,
    pub snapshot: Arc<dyn SnapshotRepo>,
    pub providers: Arc<dyn ProviderRepo>,
    /// The transaction-provider adapters (CSV, Akahu), injected so provider routes never
    /// name a concrete adapter or `Registry::new()`.
    pub provider_registry: Arc<dyn ProviderRegistry>,
    /// The stock-price feed (Yahoo Finance), injected for the on-demand price lookups the
    /// brokerage/stock-price routes drive.
    pub stock_price_provider: Arc<dyn StockPriceProvider>,
    /// The property-estimate feed (House Pricer), injected for the pre-flight lookup that gates
    /// the opt-in in `routes::property_estimates`. The monthly poll holds its own handle on the
    /// same adapter; this one exists because the *confirm* step re-runs the lookup here rather
    /// than trusting an id from the request body.
    pub property_estimate_provider: Arc<dyn PropertyEstimateProvider>,
    /// The process lifecycle handle, for the handful of handlers that start work outliving
    /// the response they return (today: the post-import valuation backfill in
    /// `routes::brokerage`).
    ///
    /// It is here so such a handler can reach `Shutdown::spawn` instead of `tokio::spawn`.
    /// A bare spawn is invisible to the drain: `SIGTERM` closes the pool underneath the
    /// task, the shutdown report counts `abandoned=0`, and `clean=true` is printed over a
    /// valuation write that was cut in half. Tracked, the same task is either waited for or
    /// named in the report — which is the report's entire purpose.
    pub shutdown: sure_appbase::Shutdown,
    /// The most of the MCP surface `SURE_MCP` permits this process to serve.
    ///
    /// Here rather than in `ApiConfig` because a *handler* needs it: the settings route
    /// refuses a stored mode above the ceiling, and reports the ceiling so the app can
    /// explain a control it has to disable. A `sure-core` type, so this struct still names
    /// nothing from `sure-mcp` — the two adapters remain unaware of each other.
    pub mcp_ceiling: sure_core::McpMode,
}
