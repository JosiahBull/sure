//! Tracing/telemetry setup for the HTTP server.
//!
//! The logging model, top to bottom, is:
//!
//! * **one INFO line per incoming request** — the [`TraceLayer`] builds an
//!   `http.request` span (INFO level) carrying OTEL HTTP semantic-convention fields,
//!   and the fmt subscriber emits a single line when that span *closes* (see
//!   [`init_tracing`]'s `FmtSpan::CLOSE`), with the response status and busy/idle
//!   timing already recorded onto it. It is the *only* INFO-level span, so this stays
//!   exactly one line per request.
//! * **several DEBUG lines per request** — each handler is instrumented with a named,
//!   DEBUG-level span (`#[tracing::instrument(name = ..., level = "debug", skip_all,
//!   fields(...), ret(DEBUG), err(WARN))]`): its `ret` logs the response at DEBUG and
//!   its span close is a timed breadcrumb. DAL repository functions add their own
//!   DEBUG spans beneath. (Handlers are pinned to `level = "debug"` on purpose — a
//!   default INFO span would, under `FmtSpan::CLOSE`, emit a *second* INFO line per
//!   request.)
//! * **WARN on a failed request** — a handler's `err(WARN)` logs the error's cause once,
//!   tagged with the same `request.id`. See [`request_context`] for how that cause is
//!   kept server-side while the client only ever sees a scrubbed message for 5xx.
//! * **a high volume of TRACE lines** — `sqlx` query logging (configured in the DAL's
//!   `connect`) emits one TRACE event per statement executed, plus finer-grained
//!   events like [`on_request`].
//!
//! Turn the volume up or down with `RUST_LOG`, e.g.
//! `RUST_LOG=info` (just the per-request line), or
//! `RUST_LOG=info,sure_api=debug,sure_dal=debug,sqlx::query=trace` (everything).

use std::time::Duration;

use axum::extract::{MatchedPath, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::classify::ServerErrorsFailureClass;
use tracing::{field::Empty, Span};
use uuid::Uuid;

use crate::error::{ErrorBody, ErrorDetail};

tokio::task_local! {
    /// The correlation id for the request currently being handled. Set once per request
    /// by [`request_context`] and read by [`make_span`] (for the `request.id` span field)
    /// and [`request_context`] itself (to echo it in scrubbed 5xx bodies). Because it is
    /// the same value in both places, a client-facing error and its server-side log line
    /// share one id.
    pub static REQUEST_ID: Uuid;
}

/// Default log directives when `RUST_LOG` is unset — tuned for `pnpm dev`.
///
/// * `info` baseline gives the one-line-per-request summary and any operational
///   INFO events (a provider sync finishing, say).
/// * our own crates at `debug` surface the nested handler/DAL spans.
/// * `sqlx=warn` keeps the very chatty per-query TRACE quiet by default (slow-query
///   warnings still come through); raise `sqlx::query=trace` to see every statement.
const DEFAULT_FILTER: &str = "info,\
    sure_api=debug,\
    sure_dal=debug,\
    sure_providers=debug,\
    sure_scheduler=debug,\
    tower_http=warn,\
    sqlx=warn";

/// Initialise the global tracing subscriber from `RUST_LOG`, falling back to
/// [`DEFAULT_FILTER`]. Idempotent: safe to call more than once (later calls no-op).
pub fn init_tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::{fmt, EnvFilter};

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let _ = fmt()
        .with_env_filter(filter)
        .with_target(true)
        // Emit one line when a span closes. For the INFO `http.request` span this is the
        // single per-request summary; for the DEBUG handler/DAL spans it's the timed
        // breadcrumb trail beneath it.
        .with_span_events(FmtSpan::CLOSE)
        .try_init();
}

/// Build the request span for a single incoming HTTP request, populated with the
/// stable [OTEL HTTP server semantic conventions][otel]. `http.response.status_code`
/// is declared empty here and filled in by [`on_response`].
///
/// [otel]: https://opentelemetry.io/docs/specs/semconv/http/http-spans/
pub fn make_span(request: &Request) -> Span {
    let method = request.method();
    let uri = request.uri();
    // The matched route template (e.g. `/api/accounts/{id}`) rather than the concrete
    // path — this is the low-cardinality value OTEL wants for `http.route`. Available
    // here because axum inserts `MatchedPath` before the per-route layer runs.
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_else(|| uri.path());

    // The per-request correlation id set by `request_context` (which wraps this layer).
    // Falls back to nil only if the span is somehow built outside a request scope.
    let request_id = REQUEST_ID
        .try_with(|id| *id)
        .unwrap_or_else(|_| Uuid::nil());

    tracing::info_span!(
        "http.request",
        "http.request.method" = %method,
        "http.route" = %route,
        "url.path" = %uri.path(),
        "request.id" = %request_id,
        // Recorded in `on_response`; declared up front so it renders on the close line.
        "http.response.status_code" = Empty,
    )
}

/// Fine-grained TRACE breadcrumb when a request starts being processed.
pub fn on_request(_request: &Request, span: &Span) {
    tracing::trace!(parent: span, "started processing request");
}

/// Record the response status onto the request span. No event is emitted: the span's
/// `FmtSpan::CLOSE` line is the single INFO summary for the request, and it will render
/// the status recorded here alongside the latency.
pub fn on_response(response: &Response, _latency: Duration, span: &Span) {
    span.record("http.response.status_code", response.status().as_u16());
}

/// Deliberately a no-op. `tower-http` would otherwise log a second ERROR for every 5xx,
/// but each handler already logs its own error via `#[instrument(err)]` (with the cause
/// and the `request.id`), and [`on_response`] records the status onto the span so the
/// request's INFO close line shows e.g. `http.response.status_code=500`.
pub fn on_failure(_failure: ServerErrorsFailureClass, _latency: Duration, _span: &Span) {}

/// Outermost middleware: stamp every request with a fresh `request_id`, hold it in
/// [`REQUEST_ID`] for the whole request (so the span and error handling agree on it),
/// and scrub any 5xx response so internal detail never reaches the client.
///
/// The real cause of a 5xx is still recorded server-side — at WARN by the handler's
/// `#[instrument(err)]` — under the same `request.id`, so the generic client message can
/// be traced back to the exact failure in the logs.
pub async fn request_context(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4();
    REQUEST_ID
        .scope(request_id, async move {
            let response = next.run(request).await;
            if response.status().is_server_error() {
                scrub_internal_error(response.status(), request_id)
            } else {
                response
            }
        })
        .await
}

/// Replace an internal-error (5xx) response with a generic body that carries only the
/// `request_id`. Keeps the same `{ "error": { "code", "message" } }` envelope the rest of
/// the API uses, so clients parse it uniformly.
fn scrub_internal_error(status: StatusCode, request_id: Uuid) -> Response {
    let body = ErrorBody {
        error: ErrorDetail {
            code: "internal".to_string(),
            message: format!("Internal Error: Something went wrong! request_id={request_id}"),
        },
    };
    (status, axum::Json(body)).into_response()
}
