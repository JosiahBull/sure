// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `485_000_00` == $485,000.00), which reads far better than 3-digit groups for
// financial values; clippy's grouping lint fights it, so allow it crate-wide.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod openapi;
pub mod routes;
pub mod state;
pub mod telemetry;

// The shared error type keeps its historical module path so handler/OpenAPI code compiles
// against `crate::error` unchanged. `sure-api` names neither `sure_dal` nor `sqlx`
// anywhere — the composition root (`sure-server`) is the only place that connects to the
// database and builds `AppState`. Provider adapters are injected as ports too; the only
// direct `sure-providers` use left is the Sharesies export parser in `routes::brokerage`.
pub use sure_core::error; // crate::error::{AppError, AppResult, ErrorBody, ErrorDetail}

use axum::{routing::get, Json, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;

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

/// Initialise tracing from `RUST_LOG`, defaulting to something useful in dev.
/// See the [`telemetry`] module for the request-logging model this sets up.
pub use telemetry::init_tracing;
