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
//!
//! Lines are **plain text unless `SURE_COLOR` asks otherwise** — see [`ColorChoice`].

use std::io::IsTerminal;
use std::str::FromStr;
use std::time::Duration;

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::classify::ServerErrorsFailureClass;
use tracing::{Span, field::Empty};
use uuid::Uuid;

use crate::error::{ErrorBody, ErrorDetail};
use crate::limits::{ErrorAlreadyClothed, PreservedErrorCode};

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

/// Selects [`ColorChoice`]. Named for the `--color` flag it spells, not for the subscriber.
const COLOR_ENV: &str = "SURE_COLOR";

/// Whether log lines carry ANSI colour — the tri-state the `--color` flag has, spelled the
/// way `git` and `ls` spell it.
///
/// [`Never`](ColorChoice::Never) is the default, which is a deliberate break with the usual
/// `auto`. This binary's normal home is a container: its stdout is a pipe handed to a log
/// driver, and what you read later is `docker logs`, `journalctl`, or a TrueNAS app's log
/// pane — none of which interpret the escapes, so every line arrives wearing runs of
/// `ESC[2m`. `auto` gets that right only if the check happens where the bytes are *written*,
/// and by then the process that could have chosen differently is long gone. Defaulting to
/// no colour makes the deployed reading — the one nobody can fix from a terminal — the
/// legible one, and leaves the interactive nicety as a one-word opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorChoice {
    /// Plain text, whatever the output is attached to. The default.
    #[default]
    Never,
    /// Colour only when the stream the subscriber writes to is a terminal.
    Auto,
    /// Colour unconditionally — for a pager, or a CI log viewer that renders ANSI out of a
    /// pipe it has no way to prove is a terminal.
    Always,
}

impl ColorChoice {
    /// Whether to emit ANSI, given whether the stream this subscriber writes to is a
    /// terminal.
    ///
    /// Taking that as an argument rather than probing in here does two things: it makes the
    /// rule testable without a pty, and it forces the caller to name *which* stream it
    /// means. Probing the wrong one is the classic way `auto` ends up wrong — under
    /// `sure-api | tee` stderr is still a terminal while stdout, where these lines actually
    /// go, is not.
    pub fn ansi(self, sink_is_terminal: bool) -> bool {
        match self {
            ColorChoice::Never => false,
            ColorChoice::Auto => sink_is_terminal,
            ColorChoice::Always => true,
        }
    }
}

impl FromStr for ColorChoice {
    type Err = String;

    /// The env edge, where the value is still text — the one place a wildcard arm over these
    /// spellings is the point rather than a missed variant.
    ///
    /// The boolean spellings are the ones `sure-server`'s `flag` already accepts, and there
    /// is only one way they can read here: asking for colour is asking for it
    /// unconditionally, exactly as a bare `--color` does. Someone who wants the sink
    /// consulted has a word for that, and it is `auto`.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "never" | "off" | "0" | "false" | "no" => Ok(ColorChoice::Never),
            "auto" => Ok(ColorChoice::Auto),
            "always" | "on" | "1" | "true" | "yes" | "force" => Ok(ColorChoice::Always),
            other => Err(format!("unknown colour choice {other:?}")),
        }
    }
}

/// Read [`COLOR_ENV`], falling back to [`ColorChoice::Never`] when it is unset or blank —
/// set-but-empty reading as unset the way `WEB_DIR` and the provider endpoints treat a blank
/// line in a `.env`.
///
/// The `Err` is a complaint to log rather than a reason to stop, but it deliberately isn't
/// logged *here*: this runs while deciding how to build the subscriber, so a `warn!` at this
/// point has nothing installed to receive it and would vanish. [`init_tracing`] carries it
/// across and emits it once there is somewhere for it to go.
fn color_from_env() -> Result<ColorChoice, String> {
    match std::env::var(COLOR_ENV) {
        Err(_) => Ok(ColorChoice::default()),
        Ok(raw) if raw.trim().is_empty() => Ok(ColorChoice::default()),
        Ok(raw) => raw.parse(),
    }
}

/// Initialise the global tracing subscriber from `RUST_LOG`, falling back to
/// [`DEFAULT_FILTER`], with colour from `SURE_COLOR` (see [`ColorChoice`] — off by default).
/// Idempotent: safe to call more than once (later calls no-op).
pub fn init_tracing() {
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::{EnvFilter, Layer as _};

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    // A bad value falls back rather than failing, as `Config::from_env` does for a tunable —
    // and it falls back to no colour, the direction that can only ever make a log easier to
    // read. The complaint waits for the subscriber being built two statements down.
    let (color, complaint) = match color_from_env() {
        Ok(color) => (color, None),
        Err(err) => (ColorChoice::default(), Some(err)),
    };
    // `stdout`, because that is where `fmt::layer` writes: this has to be the stream the
    // lines go down, not merely a stream this process happens to hold.
    let ansi = color.ansi(std::io::stdout().is_terminal());

    let output = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_ansi(ansi)
        // Emit one line when a span closes. For the INFO `http.request` span this is the
        // single per-request summary; for the DEBUG handler/DAL spans it's the timed
        // breadcrumb trail beneath it.
        .with_span_events(FmtSpan::CLOSE);

    // `RUST_LOG` filters the output layer rather than the registry, so a second layer added
    // here would see every span regardless of what the filter prints. Idempotent: `try_init`
    // leaves an already-installed subscriber alone.
    let _ = tracing_subscriber::registry()
        .with(output.with_filter(filter))
        .try_init();

    if let Some(err) = complaint {
        tracing::warn!(
            env = COLOR_ENV,
            reason = %err,
            "unrecognised colour choice; using the default (never)"
        );
    }
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
pub(crate) fn status_code_slug(status: StatusCode) -> &'static str {
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
///
/// The *message* is what has to go — it may carry a SQL error, an `anyhow` chain, or an
/// upstream's own text. The `code` does not: it comes from `AppError::code`, a closed match
/// over our own enum returning `&'static str`, so it names a category and can never carry
/// detail. A response that told us its code via [`PreservedErrorCode`] keeps it; anything else
/// — a framework 5xx, a panic caught downstream, a hand-built body — has no code worth trusting
/// and gets `internal`.
///
/// Until `AppError::Upstream` existed this distinction was invisible, because every 5xx really
/// was `internal`. It stopped being free the moment one of them meant "a third party is down,
/// try again later": that arrived as `internal`, which is the opposite of what a client should
/// do about it.
fn scrub_internal_error(response: Response, request_id: Uuid) -> Response {
    let code = response
        .extensions()
        .get::<PreservedErrorCode>()
        .map_or("internal", |preserved| preserved.0);
    rewrite_body(
        response,
        ErrorDetail {
            code: code.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole rule in one table: output is plain unless colour was asked for, and `auto`
    /// is the only spelling that lets the sink have a say. The `(Never, false)` cell is the
    /// one that matters in production — it is what a container's piped stdout gets, and it
    /// must not depend on `is_terminal` having been asked.
    #[test]
    fn colour_is_emitted_only_when_it_was_asked_for() {
        for (choice, on_a_terminal, on_a_pipe) in [
            (ColorChoice::Never, false, false),
            (ColorChoice::Auto, true, false),
            (ColorChoice::Always, true, true),
        ] {
            assert_eq!(choice.ansi(true), on_a_terminal, "{choice:?} on a terminal");
            assert_eq!(choice.ansi(false), on_a_pipe, "{choice:?} on a pipe");
        }
    }

    /// An unset `SURE_COLOR` is the overwhelmingly common case — a fresh checkout, and every
    /// container — so what `Default` resolves to *is* the shipped behaviour.
    #[test]
    fn the_default_is_no_colour_at_all() {
        assert_eq!(ColorChoice::default(), ColorChoice::Never);
    }

    /// Read alongside `flag` in `packages/server/src/config.rs`: the boolean spellings mean
    /// here what they mean there, so `SURE_COLOR=on` can't quietly land on `auto` and give a
    /// container plain text when it was told to colour.
    #[test]
    fn the_spellings_people_type() {
        for raw in ["never", "NEVER", " off ", "0", "false", "no"] {
            assert_eq!(
                raw.parse::<ColorChoice>(),
                Ok(ColorChoice::Never),
                "{raw:?}"
            );
        }
        for raw in ["auto", "Auto", " auto\n"] {
            assert_eq!(raw.parse::<ColorChoice>(), Ok(ColorChoice::Auto), "{raw:?}");
        }
        for raw in ["always", "ALWAYS", "on", "1", "true", "yes", "force"] {
            assert_eq!(
                raw.parse::<ColorChoice>(),
                Ok(ColorChoice::Always),
                "{raw:?}"
            );
        }
    }

    /// A typo is reported, not guessed at — and the message quotes what was typed, because
    /// the operator's next move is to look at the line they wrote.
    #[test]
    fn a_typo_is_rejected_and_names_itself() {
        let err = " Aut "
            .parse::<ColorChoice>()
            .expect_err("not a colour choice");
        assert!(err.contains("\"aut\""), "{err}");
    }
}
