//! The composition root: the only place concrete adapters (`sure-dal`'s `SqliteStore`,
//! `sure-providers`' clients) are named and wired into `sure-app`'s services and
//! `sure-api`'s [`AppState`](sure_api::State). `sure-api` itself never depends on
//! `sure-dal` or `sqlx` — that split is the point of this crate.

pub mod config;
pub mod health;
pub mod http;
pub mod sandbox;

use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::forecast::ForecastService;
use sure_app::import::ImportService;
use sure_app::ports::{ImportRegistry, ProviderRegistry, StockPriceProvider};
use sure_app::reports::ReportService;
use sure_app::rules::RuleService;
use sure_app::sync::SyncService;
use sure_app::SystemClock;
pub use sure_appbase::Shutdown;
use sure_dal::store::SqliteStore;
use sure_dal::Db;

use crate::config::Config;

/// The MCP endpoint, or an empty router when `SURE_MCP` leaves it off.
///
/// Handed to [`sure_api::build_app`] rather than merged around it, so `/mcp` sits inside the
/// same middleware stack as `/api` — panic catching, request ids, tracing, the rate limiter,
/// the body cap. `sure-api` sees only an opaque `Router` and never names `sure-mcp`; this is
/// the one function that knows both.
///
/// The `Host` allowlist comes from `CORS_ALLOWED_ORIGINS`, deliberately. `rmcp` defaults to
/// accepting loopback authorities only, because a locally-running MCP server is a
/// DNS-rebinding target — a page the user visits can point its own hostname at `127.0.0.1`
/// and POST here from their browser. Serving Sure on a real hostname therefore has to name
/// that hostname twice, and taking the second answer from the first means the two cannot
/// drift into disagreeing about who may reach this process.
fn mcp_router(state: sure_mcp::McpState, config: &Config) -> axum::Router {
    // Mounted on the *ceiling*, not on the stored setting: the household can switch agent
    // access off (and back on) in the app without a restart, and while it is off the mounted
    // endpoint simply serves no tools. A ceiling of `off` — the default — is the only thing
    // that makes `/mcp` not a route at all.
    if config.mcp.ceiling == sure_mcp::McpMode::Off {
        return axum::Router::new();
    }
    let hosts = config
        .api
        .cors_allowed_origins
        .iter()
        .filter_map(|origin| origin.parse::<axum::http::Uri>().ok())
        .filter_map(|uri| uri.authority().map(|a| a.to_string()))
        .collect();
    tracing::info!(
        ceiling = config.mcp.ceiling.as_str(),
        "mcp endpoint mounted at /mcp; the mode served is this capped against the app's setting"
    );
    axum::Router::new().nest_service("/mcp", sure_mcp::http_service(state, config.mcp, hosts))
}

/// Build the state each driving adapter shares: one `SqliteStore` + `SystemClock`, wired
/// into the logic-heavy services and handed out directly (as a repo port trait object) for
/// every thin-CRUD aggregate.
///
/// Both adapters are built here, from the same store and the same service instances — the
/// MCP surface is a sibling of the HTTP one, not a client of it. Each declares its own
/// dependencies (`sure_mcp::McpState` is a strict subset: no import pipeline, no provider
/// registry, no snapshot repo, because no tool may reach them), and this is the one place
/// that knows both lists.
///
/// `shutdown` is passed in rather than made here: a handler that starts work outliving its
/// response must spawn it on *this* process's tracker, so that the drain below waits for it
/// and the shutdown report can name it if it overruns. A second, private handle would track
/// nothing anybody waits for.
fn build_state(
    db: Db,
    registry: Arc<dyn ProviderRegistry>,
    imports: Arc<dyn ImportRegistry>,
    stock_price_provider: Arc<dyn StockPriceProvider>,
    shutdown: Shutdown,
    mcp_ceiling: sure_mcp::McpMode,
) -> (sure_api::State, sure_mcp::McpState) {
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
    // The same `RuleService` the rules routes drive, handed to the two services that land new
    // transactions so a row is classified when it arrives rather than when someone next
    // presses "run all". It is passed as `dyn AutoCategorize` — the one method either of them
    // needs — so neither can reach the rest of the rule surface.
    let sync = Arc::new(SyncService::new(
        store.clone(),
        store.clone(),
        store.clone(),
        registry.clone(),
        clock.clone(),
        rules.clone(),
    ));
    let forecast = Arc::new(ForecastService::new(
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        clock,
    ));
    // Takes `reports` rather than the balances repo: an import reconciles an export's stated
    // closing balance against the figure the account page shows, and that figure is a
    // derivation (newest valuation, else the running transaction sum) rather than a column.
    let import = Arc::new(ImportService::new(
        imports,
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        store.clone(),
        registry.clone(),
        reports.clone(),
        rules.clone(),
    ));

    let mcp = sure_mcp::McpState {
        reports: reports.clone(),
        rules: rules.clone(),
        brokerage: brokerage.clone(),
        accounts: store.clone(),
        transactions: store.clone(),
        categories: store.clone(),
        merchants: store.clone(),
        valuations: store.clone(),
        equity: store.clone(),
        settings: store.clone(),
        currencies: store.clone(),
        stock_price_provider: stock_price_provider.clone(),
        shutdown: shutdown.clone(),
    };

    let api = sure_api::State {
        brokerage,
        import,
        reports,
        forecast,
        rules,
        sync,
        stock_prices: store.clone(),
        accounts: store.clone(),
        transactions: store.clone(),
        categories: store.clone(),
        merchants: store.clone(),
        people: store.clone(),
        currencies: store.clone(),
        settings: store.clone(),
        valuations: store.clone(),
        equity: store.clone(),
        crons: store.clone(),
        snapshot: store.clone(),
        providers: store,
        provider_registry: registry,
        stock_price_provider,
        shutdown,
        mcp_ceiling,
    };

    (api, mcp)
}

/// Connect, migrate, and serve until `shutdown` is cancelled.
///
/// The application half of the lifecycle `sure_appbase::run` drives: everything spawned
/// here goes through the handle, so the shutdown report can account for all of it. See
/// [`main`](../main/index.html) for why the runtime is built by the binary rather than by
/// `sure-appbase`.
pub async fn serve(config: Config, shutdown: Shutdown) -> anyhow::Result<()> {
    let pool = sure_dal::connect(&config.database_url).await?;
    sure_dal::migrate(&pool).await?;

    // The concrete provider adapters, built once here (the composition root is the only
    // crate that names them) and shared between the scheduled tasks and the HTTP handlers.
    // Each is handed the endpoint `Config` parsed for it: `sure-providers` reads no
    // configuration of its own, so where an adapter points is decided exactly here.
    let akahu = sure_providers::AkahuProvider::new(
        config.provider_endpoints.akahu.clone(),
        // The one environment read outside `config` (see the note on `Config`), and the result
        // is *stored* rather than propagated. Akahu is one optional integration of several, so
        // absent tokens are the ordinary state — of every CI run, and of anyone who doesn't
        // bank in NZ — and `?` here would turn that into a server that refuses to start. The
        // error still has to arrive with a variable name attached when someone asks for a
        // sync, which is why the provider keeps the `Result`: `specs/akahu.spec.ts` asserts
        // the 422 from `/api/provider-kinds/akahu/accounts` says `AKAHU_APP_TOKEN`.
        sure_providers::AkahuCredentials::from_env(),
    );
    let registry: Arc<dyn ProviderRegistry> = Arc::new(sure_providers::Registry::new(akahu));
    // The file-import adapters. Nothing to configure — a parser has no endpoint and reads no
    // environment — but built here all the same, so `sure-api` names no parser and the
    // detection order stays one list in one place.
    let imports: Arc<dyn ImportRegistry> = Arc::new(sure_providers::import::ImportRegistry::new());
    let stock_price_provider: Arc<dyn StockPriceProvider> =
        Arc::new(sure_providers::YahooFinanceProvider::with_endpoint(
            config.provider_endpoints.yahoo_finance.clone(),
        ));

    // Opt-out (`BACKGROUND_TASKS=off`) because the scheduler's first check runs
    // immediately, so every never-run task fires during startup: the API e2e suite turns
    // it off so the provider poll — which records a sync row per enabled provider — can't
    // race a test's own fixtures. Containment is no longer one of the reasons, though it
    // was: every adapter that suite spawns is pointed at `sure-testproxy`, so a sweep that
    // does run reaches a stub or a replay-miss 503 rather than the network whatever this
    // flag says — which is what lets `specs/shutdown.spec.ts` turn it back on and drain a
    // poll that is genuinely in flight.
    if config.background_tasks {
        let task_state = Arc::new(sure_dal::scheduled_tasks::SqliteTaskStateStore::new(
            pool.clone(),
        ));
        let mut scheduler =
            sure_scheduler::Scheduler::new(task_state, std::time::Duration::from_secs(60));

        // One store + clock for the scheduled tasks' ports (a separate instance from the one
        // `build_state` builds for the HTTP handlers — both are stateless wrappers around
        // clones of the same pool, so there's nothing to share).
        let store = Arc::new(SqliteStore::new(pool.clone()));
        let clock = Arc::new(SystemClock);
        // The poll's own `RuleService`, for the same reason the store and clock beside it are
        // separate instances: both are stateless wrappers around clones of one pool. This is
        // the path that most needs the automatic pass — a 6-hourly poll lands transactions
        // when nobody is looking at the app at all.
        let sync = Arc::new(SyncService::new(
            store.clone(),
            store.clone(),
            store.clone(),
            registry.clone(),
            clock.clone(),
            Arc::new(RuleService::new(store.clone())),
        ));

        scheduler.register(Box::new(
            sure_app::tasks::exchange_rates::ExchangeRateTask::new(
                store.clone(),
                // The only Frankfurter instance in the process — nothing but this task fetches
                // rates — so unlike Yahoo's it is built here rather than above.
                Arc::new(sure_providers::FrankfurterProvider::with_endpoint(
                    config.provider_endpoints.frankfurter.clone(),
                )),
            ),
        ));
        scheduler.register(Box::new(
            sure_app::tasks::provider_poll::ProviderPollTask::new(
                store.clone(),
                registry.clone(),
                sync,
            ),
        ));
        // After the provider poll, so a balance-only account's freshly-written valuation is
        // already there to be differenced (though only *closed* days are derived, so the
        // ordering is a convenience rather than a correctness requirement).
        scheduler.register(Box::new(
            sure_app::tasks::balance_delta::BalanceDeltaTask::new(
                store.clone(),
                store.clone(),
                store.clone(),
                clock.clone(),
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
        // Tracked, so the drain below waits for a sweep that is mid-flight when the
        // shutdown signal lands — a provider poll part-way through writing a sync row is
        // exactly the thing that must not be cut off.
        let scheduler_task = shutdown.spawn(scheduler.run(shutdown.child_token()));
        // …and watched, because until now this handle was dropped on the floor. A panic
        // inside a job is contained by the scheduler itself, so the only way this resolves
        // abnormally is the loop machinery coming apart — but nothing observed that, and the
        // result was every background job dead for the life of the process while
        // `/api/health` went on answering `ok`. Taking the process down instead is strictly
        // better: a supervisor restarts it with its background work intact.
        let watchdog = shutdown.clone();
        shutdown.spawn(async move {
            if let Err(err) = scheduler_task.await {
                tracing::error!(error = %err, "the scheduler loop ended abnormally; shutting down");
                watchdog.cancel();
            }
        });
    }

    let (state, mcp_state) = build_state(
        pool.clone(),
        registry,
        imports,
        stock_price_provider,
        shutdown.clone(),
        config.mcp.ceiling,
    );
    let app = sure_api::build_app(
        state,
        config.web_dir.as_deref(),
        &config.api,
        mcp_router(mcp_state, &config),
    );

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "sure-api listening");
    // Returns once every connection has drained (or the grace period expired). See `http`
    // for why this isn't `axum::serve`.
    http::serve(listener, app, config.http, &shutdown).await?;

    // The HTTP loop is done, but the scheduler may still be finishing a task that holds a
    // connection, so wait for everything tracked *before* closing the pool. `run` drains
    // again after this future returns; that second call finds an empty tracker and is a
    // no-op. Doing it in the other order would checkpoint the WAL out from under a write
    // still in flight.
    let drain = shutdown.drain(config.lifecycle.drain_grace).await;
    tracing::debug!(
        outcome = drain.as_str(),
        abandoned = drain.abandoned(),
        "background tasks drained"
    );

    // SQLite checkpoints the WAL on the last connection closing; doing it here rather than
    // letting the process exit is what keeps a container restart from leaving one behind.
    tracing::info!("closing the database pool");
    pool.close().await;
    Ok(())
}
