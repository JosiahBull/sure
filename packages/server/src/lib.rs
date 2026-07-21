//! The composition root: the only place concrete adapters (`sure-dal`'s `SqliteStore`,
//! `sure-providers`' clients) are named and wired into `sure-app`'s services and
//! `sure-api`'s [`AppState`](sure_api::State). `sure-api` itself never depends on
//! `sure-dal` or `sqlx` — that split is the point of this crate.

pub mod config;

use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::forecast::ForecastService;
use sure_app::ports::{ProviderRegistry, StockPriceProvider};
use sure_app::reports::ReportService;
use sure_app::rules::RuleService;
use sure_app::sync::SyncService;
use sure_app::SystemClock;
use sure_dal::store::SqliteStore;
use sure_dal::Db;

use crate::config::Config;

/// Build the `AppState` every handler shares: one `SqliteStore` + `SystemClock`, wired
/// into the four logic-heavy services and handed out directly (as a repo port trait
/// object) for every thin-CRUD aggregate.
fn build_state(
    db: Db,
    registry: Arc<dyn ProviderRegistry>,
    stock_price_provider: Arc<dyn StockPriceProvider>,
) -> sure_api::State {
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
        registry.clone(),
        clock.clone(),
    ));
    let forecast = Arc::new(ForecastService::new(
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        clock,
    ));

    sure_api::State {
        brokerage,
        reports,
        forecast,
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
        provider_registry: registry,
        stock_price_provider,
    }
}

/// Connect, migrate, and serve until shutdown. Used by the `sure-api` binary.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = sure_dal::connect(&config.database_url).await?;
    sure_dal::migrate(&pool).await?;

    let task_state = Arc::new(sure_dal::scheduled_tasks::SqliteTaskStateStore::new(
        pool.clone(),
    ));
    let mut scheduler =
        sure_scheduler::Scheduler::new(task_state, std::time::Duration::from_secs(60));

    // The concrete provider adapters, built once here (the composition root is the only
    // crate that names them) and shared between the scheduled tasks and the HTTP handlers.
    let registry: Arc<dyn ProviderRegistry> = Arc::new(sure_providers::Registry::new());
    let stock_price_provider: Arc<dyn StockPriceProvider> =
        Arc::new(sure_providers::YahooFinanceProvider::new());

    // One store + clock for the scheduled tasks' ports (a separate instance from the one
    // `build_state` builds for the HTTP handlers — both are stateless wrappers around
    // clones of the same pool, so there's nothing to share).
    let store = Arc::new(SqliteStore::new(pool.clone()));
    let clock = Arc::new(SystemClock);
    let sync = Arc::new(SyncService::new(
        store.clone(),
        store.clone(),
        store.clone(),
        registry.clone(),
        clock.clone(),
    ));

    scheduler.register(Box::new(
        sure_app::tasks::exchange_rates::ExchangeRateTask::new(
            store.clone(),
            Arc::new(sure_providers::FrankfurterProvider::new()),
        ),
    ));
    scheduler.register(Box::new(
        sure_app::tasks::provider_poll::ProviderPollTask::new(
            store.clone(),
            registry.clone(),
            sync,
        ),
    ));
    scheduler.register(Box::new(sure_app::stock_prices::StockPriceTask::new(
        store.clone(),
        store.clone(),
        clock,
        stock_price_provider.clone(),
    )));
    scheduler.register(Box::new(
        sure_app::tasks::transfer_link::TransferLinkTask::new(store),
    ));
    scheduler.spawn();

    let state = build_state(pool, registry, stock_price_provider);
    let app = sure_api::build_app(state, config.web_dir.as_deref());

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "sure-api listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
