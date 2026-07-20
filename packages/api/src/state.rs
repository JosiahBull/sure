use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::ports::{
    AccountRepo, CategoryRepo, CronRepo, CurrencyRepo, EquityRepo, MerchantRepo, ProviderRepo,
    SettingsRepo, SnapshotRepo, StockPriceCacheRepo, TransactionRepo, ValuationRepo,
};
use sure_app::reports::ReportService;
use sure_app::rules::RuleService;
use sure_app::sync::SyncService;
use sure_app::SystemClock;
use sure_dal::store::SqliteStore;
use sure_dal::Db;

/// Shared application state handed to every handler. Cheap to clone — every field is an
/// `Arc` (the four logic-heavy services, or a repo port trait object directly for thin
/// CRUD aggregates that have no orchestration logic worth a named service). Built once in
/// `AppState::new` from one shared `SqliteStore` + `SystemClock`; no handler names
/// `sure_dal` or `sqlx` — that's confined to this file and `lib.rs::serve()`.
#[derive(Clone)]
pub struct AppState {
    pub brokerage: Arc<BrokerageService>,
    pub reports: Arc<ReportService>,
    pub rules: Arc<RuleService>,
    pub sync: Arc<SyncService>,
    pub stock_prices: Arc<dyn StockPriceCacheRepo>,
    pub accounts: Arc<dyn AccountRepo>,
    pub transactions: Arc<dyn TransactionRepo>,
    pub categories: Arc<dyn CategoryRepo>,
    pub merchants: Arc<dyn MerchantRepo>,
    pub currencies: Arc<dyn CurrencyRepo>,
    pub settings: Arc<dyn SettingsRepo>,
    pub valuations: Arc<dyn ValuationRepo>,
    pub equity: Arc<dyn EquityRepo>,
    pub crons: Arc<dyn CronRepo>,
    pub snapshot: Arc<dyn SnapshotRepo>,
    pub providers: Arc<dyn ProviderRepo>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        let store = Arc::new(SqliteStore::new(db));
        let clock = Arc::new(SystemClock);

        let brokerage = Arc::new(BrokerageService::new(
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            store.clone(),
            clock.clone(),
        ));
        let reports = Arc::new(ReportService::new(
            store.clone(),
            store.clone(),
            clock.clone(),
        ));
        let rules = Arc::new(RuleService::new(store.clone()));
        let sync = Arc::new(SyncService::new(
            store.clone(),
            store.clone(),
            store.clone(),
            clock,
        ));

        Self {
            brokerage,
            reports,
            rules,
            sync,
            stock_prices: store.clone(),
            accounts: store.clone(),
            transactions: store.clone(),
            categories: store.clone(),
            merchants: store.clone(),
            currencies: store.clone(),
            settings: store.clone(),
            valuations: store.clone(),
            equity: store.clone(),
            crons: store.clone(),
            snapshot: store.clone(),
            providers: store,
        }
    }
}
