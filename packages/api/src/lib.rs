// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `485_000_00` == $485,000.00), which reads far better than 3-digit groups for
// financial values; clippy's grouping lint fights it, so allow it crate-wide.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod cache;
pub mod config;
pub mod etag;
pub mod extract;
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

use axum::extract::DefaultBodyLimit;
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
/// | body limit | Sets the *default* ceiling only — see [`with_global_body_limit`] for why a per-route override still wins from further in. |
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

    let app = with_global_body_limit(app, limits)
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

/// Apply [`Limits::max_body_bytes`] (`MAX_BODY_BYTES`) as the request-body ceiling for
/// every route that does not set its own.
///
/// This has to be a whole-router layer, and the direction of the resulting override is the
/// entire point. `DefaultBodyLimit` is not a check — it inserts a request extension that
/// the body extractors read, and **the last insert wins**. A layer added here runs *outside*
/// routing, so the four import routes' own `DefaultBodyLimit` (added inside their
/// `MethodRouter` in `routes::{asb,brokerage,snapshot,student_loan}`) inserts afterwards and
/// keeps its larger cap. Reversed — a global limit applied nearer the handler than the
/// per-route one — a 32 MiB config snapshot and a 50 MiB import zip would both be clamped
/// to 2 MiB, which is what `specs/http.spec.ts`'s over-cap snapshot test and the 51 MB
/// import tests are there to catch.
///
/// Until this existed the knob was inert in both directions: the effective ceiling was
/// axum's own built-in default, which happens to be 2 MB as well, so `MAX_BODY_BYTES=524288`
/// did not tighten anything and 10 MiB did not loosen it. The refusal still comes back as a
/// bare-text 413 from the extractor and is re-clothed in the standard
/// `{ "error": { code, message } }` envelope by [`telemetry::request_context`] on the way
/// out, exactly as before.
fn with_global_body_limit(app: Router, limits: &Limits) -> Router {
    app.layer(DefaultBodyLimit::max(limits.max_body_bytes))
}

/// Serialise the live OpenAPI document. Handy for debugging; the build-time client
/// generation uses the `gen-openapi` binary instead.
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

/// Initialise tracing from `RUST_LOG`, defaulting to something useful in dev.
/// See the [`telemetry`] module for the request-logging model this sets up.
pub use telemetry::init_tracing;

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::{Body, Bytes};
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::post;
    use tower::ServiceExt;

    /// axum's own built-in ceiling, which every assertion below has to be measured against:
    /// a limit that merely *matches* it proves nothing, because that is the value the knob
    /// silently had while it was applied nowhere.
    const AXUM_DEFAULT: usize = 2 * 1000 * 1000;

    /// A stand-in with the same shape as [`routes::router`]: everything nested under `/api`,
    /// one plain route on the global ceiling, one carrying its own larger
    /// `DefaultBodyLimit` inside its `MethodRouter` the way the four import routes do. Wired
    /// through the same [`with_global_body_limit`] `build_app` uses, so a change to where
    /// that layer sits is what these tests actually exercise.
    fn app(limits: &Limits) -> Router {
        async fn echo(body: Bytes) -> String {
            body.len().to_string()
        }
        let api = Router::new().route("/plain", post(echo)).route(
            "/import",
            post(echo).layer(DefaultBodyLimit::max(limits.max_import_body_bytes)),
        );
        with_global_body_limit(Router::new().nest("/api", api), limits)
    }

    async fn post_bytes(limits: &Limits, path: &str, bytes: usize) -> StatusCode {
        let request = HttpRequest::post(path)
            .body(Body::from(vec![b'x'; bytes]))
            .expect("request builds");
        app(limits)
            .oneshot(request)
            .await
            .expect("router is infallible")
            .status()
    }

    #[tokio::test]
    async fn the_global_ceiling_tightens_below_axums_default() {
        let limits = Limits {
            max_body_bytes: 1024,
            ..Limits::default()
        };
        assert_eq!(
            post_bytes(&limits, "/api/plain", 1024).await,
            StatusCode::OK
        );
        // Both sides of the boundary are orders of magnitude under `AXUM_DEFAULT`, so only
        // the configured limit can be producing this refusal.
        assert_eq!(
            post_bytes(&limits, "/api/plain", 1025).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn the_global_ceiling_loosens_above_axums_default() {
        let limits = Limits {
            max_body_bytes: 4 * 1024 * 1024,
            ..Limits::default()
        };
        // Over axum's default and under ours: refused before this layer existed, accepted now.
        assert_eq!(
            post_bytes(&limits, "/api/plain", 3 * 1024 * 1024).await,
            StatusCode::OK
        );
        assert_eq!(
            post_bytes(&limits, "/api/plain", 5 * 1024 * 1024).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn a_per_route_override_still_wins_over_the_global_ceiling() {
        let limits = Limits {
            max_body_bytes: 1024,
            ..Limits::default()
        };
        let body = 3 * 1024 * 1024;
        assert!(body > AXUM_DEFAULT, "must also clear axum's own default");
        assert!(body < limits.max_import_body_bytes);
        // The import route's own limit is inserted from further in and therefore last. If
        // the global layer ever ends up nearer the handler this flips to 413 — and the
        // 32 MiB snapshot and 50 MiB import routes break in production with it.
        assert_eq!(
            post_bytes(&limits, "/api/import", body).await,
            StatusCode::OK
        );
        assert_eq!(
            post_bytes(&limits, "/api/plain", body).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
}
