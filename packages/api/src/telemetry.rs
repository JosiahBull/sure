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
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::classify::ServerErrorsFailureClass;
use tracing::{field::Empty, Span};
use uuid::Uuid;

use crate::error::{ErrorBody, ErrorDetail};
use crate::limits::ErrorAlreadyClothed;

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

/// Stamp every request with a fresh `request_id`, hold it in [`REQUEST_ID`] for the whole
/// request (so the span and error handling agree on it), and normalise error bodies on the
/// way out:
///
/// * **5xx** is scrubbed to a generic message carrying only the `request_id`, so internal
///   detail never reaches the client. The real cause is still recorded server-side — at
///   WARN by the handler's `#[instrument(err)]` — under the same `request.id`, so the
///   generic client message can be traced back to the exact failure in the logs.
/// * **4xx that isn't already JSON** is re-clothed in the same
///   `{ "error": { code, message } }` envelope. Rejections generated by the framework
///   rather than by a handler — an over-limit body (413), an unroutable method (405),
///   a body that isn't valid JSON (400) — otherwise come back as bare text that a client
///   parsing the envelope can't read.
/// * Responses marked [`ErrorAlreadyClothed`] are left alone: the rate limiter, the load
///   shedder, and the deadline build their own envelope and attach headers (`Retry-After`)
///   that must survive.
pub async fn request_context(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4();
    // The envelope is an API contract, so 4xx rewriting is scoped to `/api`. The static
    // side must keep its own bodies: `ServeDir::not_found_service` deliberately serves the
    // SPA shell *with* a 404 status, and replacing that with JSON would stop the app from
    // booting on a deep link.
    let is_api = request.uri().path().starts_with("/api");

    REQUEST_ID
        .scope(request_id, async move {
            let response = next.run(request).await;
            let status = response.status();

            if response.extensions().get::<ErrorAlreadyClothed>().is_some() {
                return response;
            }
            if status.is_server_error() {
                return scrub_internal_error(response, request_id);
            }
            if is_api && status.is_client_error() && !is_json(&response) {
                let detail = ErrorDetail {
                    code: status_code_slug(status).to_string(),
                    message: describe(status),
                };
                return rewrite_body(response, detail);
            }
            response
        })
        .await
}

/// Whether a response already carries a JSON body — i.e. it came from [`AppError`], which
/// produces the envelope itself.
///
/// [`AppError`]: crate::error::AppError
fn is_json(response: &Response) -> bool {
    response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
}

/// A stable machine-readable code for a framework-generated rejection, in the same style
/// as [`AppError::code`](crate::error::AppError::code).
fn status_code_slug(status: StatusCode) -> &'static str {
    match status {
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::UNPROCESSABLE_ENTITY => "validation",
        _ => "bad_request",
    }
}

fn describe(status: StatusCode) -> String {
    let reason = status.canonical_reason().unwrap_or("Request rejected");
    format!("{reason}.")
}

/// Replace an internal-error (5xx) body with a generic one carrying only the
/// `request_id`. Keeps the same `{ "error": { "code", "message" } }` envelope the rest of
/// the API uses, so clients parse it uniformly.
fn scrub_internal_error(response: Response, request_id: Uuid) -> Response {
    rewrite_body(
        response,
        ErrorDetail {
            code: "internal".to_string(),
            message: format!("Internal Error: Something went wrong! request_id={request_id}"),
        },
    )
}

/// Swap a response's body for the error envelope while keeping its status and headers.
///
/// Building a fresh response instead would silently drop everything the layers below
/// added — `Cache-Control`, the CDN directives, `Retry-After` — so an error would be the
/// one response with no cache policy on it.
fn rewrite_body(response: Response, detail: ErrorDetail) -> Response {
    let (mut parts, _discarded) = response.into_parts();
    let json = serde_json::to_vec(&ErrorBody { error: detail })
        .expect("the error envelope is always serialisable");

    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // This layer sits outside compression, so the replacement body is plain bytes. Leaving
    // the original `Content-Encoding` in place would tell the client to inflate JSON that
    // was never deflated. The old length is wrong for the same reason; hyper recomputes it.
    parts.headers.remove(header::CONTENT_ENCODING);
    parts.headers.remove(header::CONTENT_LENGTH);
    // The body no longer matches whatever validator described the original.
    parts.headers.remove(header::ETAG);
    Response::from_parts(parts, axum::body::Body::from(json))
}
