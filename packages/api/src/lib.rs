// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `485_000_00` == $485,000.00), which reads far better than 3-digit groups for
// financial values; clippy's grouping lint fights it, so allow it crate-wide.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod cache;
pub mod config;
pub mod etag;
pub mod limits;
pub mod openapi;
pub mod routes;
pub mod security;
pub mod state;
pub mod telemetry;

// The shared error type keeps its historical module path so handler/OpenAPI code compiles
// against `crate::error` unchanged. `sure-api` names neither `sure_dal` nor `sqlx`
// anywhere — the composition root (`sure-server`) is the only place that connects to the
// database and builds `AppState`. Provider adapters are injected as ports too; the only
// direct `sure-providers` use left is the Sharesies export parser in `routes::brokerage`.
pub use sure_core::error; // crate::error::{AppError, AppResult, ErrorBody, ErrorDetail}

use std::sync::Arc;

use axum::middleware::{from_fn, from_fn_with_state};
use axum::{routing::get, Json, Router};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;

use crate::cache::Deadlines;
use crate::config::ApiConfig;
use crate::limits::{InFlight, RateLimiter};
use crate::openapi::ApiDoc;
use crate::state::AppState;

pub use config::{ApiConfig as Config, Limits};
pub use state::AppState as State;
pub use sure_core::{AppError, AppResult};

/// Build the Axum application: all API routes, the live OpenAPI document, the HTTP
/// middleware stack described below, and — in production — static serving of the built
/// SPA with client-side-routing fallback.
///
/// The e2e test harness calls this with a fresh [`AppState`] to drive the real app
/// over HTTP, so there is no separate "test app" that could drift from production.
///
/// # Layer order
///
/// Listed outermost first; each one wraps everything below it. `Router::layer` applies a
/// layer *inside* routing, so all of these can read the [`MatchedPath`] axum resolved.
///
/// | Layer | Why it sits where it does |
/// | --- | --- |
/// | `CatchPanic` | Must be outermost to catch a panic raised anywhere below it. |
/// | `request_context` | Establishes the `request_id` scope before the span is built, and normalises error bodies on the way out. |
/// | `Trace` | Below the context so it can read the id; above the limiters so a rejection is still logged. |
/// | CORS | Above the limiters so a `429`/`503` carries the headers a browser needs to read it, and so a preflight is answered without touching them. |
/// | security headers | Same reasoning: they belong on every response, not just handler output. |
/// | rate limit → in-flight shed → deadline | Cheapest rejection first: refuse the over-eager client, then the overloaded server, then bound whatever is left. |
/// | compression | Outside the ETag layer, so tags are computed over identity bytes and an empty `304` costs no compression work. |
/// | ETag | Needs the `Cache-Control` the layer below sets, in order to copy it onto a `304`. |
/// | cache headers | Innermost, so it sees the handler's real status before deciding. |
///
/// [`MatchedPath`]: axum::extract::MatchedPath
pub fn build_app(state: AppState, web_dir: Option<&str>, config: &ApiConfig) -> Router {
    let mut app: Router<AppState> =
        routes::router(&config.limits).route("/api/openapi.json", get(openapi_json));

    if let Some(dir) = web_dir {
        let index = format!("{dir}/index.html");
        let serve = ServeDir::new(dir).not_found_service(ServeFile::new(index));
        app = app.fallback_service(serve);
    }

    let limits = &config.limits;
    let rate_limiter = Arc::new(RateLimiter::new(
        limits.rate_limit_rps,
        limits.rate_limit_burst,
        limits.rate_limit_exempt_loopback,
        config.trust_proxy_headers,
    ));

    let mut app = app
        .with_state(state)
        .layer(from_fn_with_state(
            config.cdn_cache_headers,
            cache::cache_control,
        ))
        .layer(from_fn_with_state(limits.max_etag_body_bytes, etag::etag));

    if config.compression {
        // The default predicate already skips tiny bodies, images, gRPC, and SSE. Brotli
        // is pinned well below its default quality: the top levels cost far more CPU than
        // they save bytes on JSON, and this runs on whatever small box is at home.
        app = app.layer(
            CompressionLayer::new()
                .br(true)
                .gzip(true)
                .zstd(true)
                .quality(tower_http::CompressionLevel::Precise(4)),
        );
    }

    let app = app
        .layer(from_fn_with_state(
            Deadlines {
                normal: limits.request_timeout,
                long: limits.long_request_timeout,
            },
            cache::timeout,
        ))
        .layer(from_fn_with_state(
            InFlight::new(limits.max_in_flight),
            limits::shed_when_saturated,
        ))
        .layer(from_fn_with_state(rate_limiter, limits::rate_limit));

    let app = security::with_security_headers(app);
    let app = match security::cors_layer(config) {
        Some(cors) => app.layer(cors),
        None => app,
    };

    app
        // One INFO span per request with OTEL fields; handler/DAL spans nest beneath it.
        // See the `telemetry` module for the logging model.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(telemetry::make_span)
                .on_request(telemetry::on_request)
                .on_response(telemetry::on_response)
                .on_failure(telemetry::on_failure),
        )
        // Establishes the per-request `request_id` scope before the span is built and
        // normalises error bodies on the way out. Must wrap the trace layer so
        // `make_span` can read the id.
        .layer(from_fn(telemetry::request_context))
        // Outermost, so a panic anywhere below becomes a 500 instead of a dropped
        // connection. `request_context` then scrubs it like any other internal error.
        .layer(CatchPanicLayer::new())
}

/// Serialise the live OpenAPI document. Handy for debugging; the build-time client
/// generation uses the `gen-openapi` binary instead.
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Initialise tracing from `RUST_LOG`, defaulting to something useful in dev.
/// See the [`telemetry`] module for the request-logging model this sets up.
pub use telemetry::init_tracing;
