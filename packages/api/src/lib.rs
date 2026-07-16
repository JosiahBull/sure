pub mod config;
pub mod openapi;
pub mod routes;
pub mod state;

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

pub use sure_core::{AppError, AppResult};
pub use state::AppState as State;

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
        .layer(TraceLayer::new_for_http())
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
    let state = AppState::new(pool);
    let app = build_app(state, config.web_dir.as_deref());

    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    tracing::info!(addr = %config.bind_addr, "sure-api listening");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

/// Initialise tracing from `RUST_LOG`, defaulting to something useful in dev.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sure_api=debug,tower_http=info"));
    let _ = fmt().with_env_filter(filter).try_init();
}
