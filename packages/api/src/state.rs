use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::forecast::ForecastService;
use sure_app::ports::{
    AccountRepo, CategoryRepo, CronRepo, CurrencyRepo, EquityRepo, MerchantRepo, PersonRepo,
    ProviderRegistry, ProviderRepo, SettingsRepo, SnapshotRepo, StockPriceCacheRepo,
    StockPriceProvider, TransactionRepo, ValuationRepo,
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
    pub crons: Arc<dyn CronRepo>,
    pub snapshot: Arc<dyn SnapshotRepo>,
    pub providers: Arc<dyn ProviderRepo>,
    /// The transaction-provider adapters (CSV, Akahu), injected so provider routes never
    /// name a concrete adapter or `Registry::new()`.
    pub provider_registry: Arc<dyn ProviderRegistry>,
    /// The stock-price feed (Yahoo Finance), injected for the on-demand price lookups the
    /// brokerage/stock-price routes drive.
    pub stock_price_provider: Arc<dyn StockPriceProvider>,
}
