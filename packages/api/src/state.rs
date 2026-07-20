use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::ports::StockPriceCacheRepo;
use sure_app::reports::ReportService;
use sure_app::rules::RuleService;
use sure_app::sync::SyncService;
use sure_app::SystemClock;
use sure_dal::store::SqliteStore;
use sure_dal::Db;

/// Shared application state handed to every handler. Cheap to clone (every field is an
/// `Arc` — the pool internally, the services explicitly). `db` is the DAL's `Db` type,
/// still used directly by the thin CRUD routes that Phase 2 doesn't port; the four
/// logic-heavy services depend on ports instead, backed by the same pool through one
/// shared `SqliteStore`.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub brokerage: Arc<BrokerageService>,
    pub reports: Arc<ReportService>,
    pub rules: Arc<RuleService>,
    pub sync: Arc<SyncService>,
    pub stock_prices: Arc<dyn StockPriceCacheRepo>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        let store = Arc::new(SqliteStore::new(db.clone()));
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
            db,
            brokerage,
            reports,
            rules,
            sync,
            stock_prices: store,
        }
    }
}
