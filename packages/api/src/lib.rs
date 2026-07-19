// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `485_000_00` == $485,000.00), which reads far better than 3-digit groups for
// financial values; clippy's grouping lint fights it, so allow it crate-wide.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod brokerage;
pub mod config;
pub mod exchange_rates;
pub mod fx;
pub mod openapi;
pub mod provider_poll;
pub mod routes;
pub mod state;
pub mod stock_prices;
pub mod telemetry;
pub mod transfer_link;

// The lower layers now live in their own crates. Re-export them under the historical
// module paths so handler/OpenAPI code keeps compiling against `crate::error`,
// `crate::db`, and `crate::providers` unchanged.
pub use sure_core::error; // crate::error::{AppError, AppResult, ErrorBody, ErrorDetail}
pub use sure_dal as db; // crate::db::{connect, migrate, MIGRATOR, Db}
pub use sure_providers as providers; // crate::providers::{Registry, SyncContext, ProviderKind, ...}

use axum::{routing::get, Json, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;

use crate::config::Config;
use crate::openapi::ApiDoc;
use crate::state::AppState;

pub use state::AppState as State;
pub use sure_core::{AppError, AppResult};

/// Build the Axum application: all API routes, the live OpenAPI document, permissive
/// CORS (the app runs behind a firewall), request tracing, and — in production —
/// static serving of the built SPA with client-side-routing fallback.
///
/// The e2e test harness calls this with a fresh [`AppState`] to drive the real app
/// over HTTP, so there is no separate "test app" that could drift from production.
pub fn build_app(state: AppState, web_dir: Option<&str>) -> Router {
    let mut app: Router<AppState> = routes::router().route("/api/openapi.json", get(openapi_json));

    if let Some(dir) = web_dir {
        let index = format!("{dir}/index.html");
        let serve = ServeDir::new(dir).not_found_service(ServeFile::new(index));
        app = app.fallback_service(serve);
    }

    app.with_state(state)
        .layer(CorsLayer::permissive())
        // One INFO span per request with OTEL fields; handler/DAL spans nest beneath it.
        // See the `telemetry` module for the logging model.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(telemetry::make_span)
                .on_request(telemetry::on_request)
                .on_response(telemetry::on_response)
                .on_failure(telemetry::on_failure),
        )
        // Outermost: establishes the per-request `request_id` scope before the span is
        // built and scrubs internal detail from 5xx responses on the way out. Must wrap
        // the trace layer so `make_span` can read the id.
        .layer(axum::middleware::from_fn(telemetry::request_context))
}

/// Serialise the live OpenAPI document. Handy for debugging; the build-time client
/// generation uses the `gen-openapi` binary instead.
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Connect, migrate, and serve until shutdown. Used by the `sure-api` binary.
pub async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;

    let task_state =
        std::sync::Arc::new(db::scheduled_tasks::SqliteTaskStateStore::new(pool.clone()));
    let mut scheduler =
        sure_scheduler::Scheduler::new(task_state, std::time::Duration::from_secs(60));
    scheduler.register(Box::new(exchange_rates::ExchangeRateTask::new(
        pool.clone(),
        std::sync::Arc::new(providers::FrankfurterProvider::new()),
    )));
    scheduler.register(Box::new(provider_poll::ProviderPollTask::new(pool.clone())));
    scheduler.register(Box::new(stock_prices::StockPriceTask::new(
        pool.clone(),
        std::sync::Arc::new(providers::YahooFinanceProvider::new()),
    )));
    scheduler.register(Box::new(transfer_link::TransferLinkTask::new(pool.clone())));
    scheduler.spawn();

    let state = AppState::new(pool);
    let app = build_app(state, config.web_dir.as_deref());

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "sure-api listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

/// Initialise tracing from `RUST_LOG`, defaulting to something useful in dev.
/// See the [`telemetry`] module for the request-logging model this sets up.
pub use telemetry::init_tracing;
