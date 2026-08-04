use serde::Serialize;
use utoipa::ToSchema;

/// The single error type shared across the workspace. Data-access and engine crates
/// return it; the API crate turns it into an HTTP response (behind the `axum` feature).
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0} not found")]
    NotFound(&'static str),

    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    Validation(String),

    #[cfg(feature = "sqlx")]
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// The one "busy, come back" contract. Emitted by the API's in-flight shedder
/// (`sure_api::limits::shed_when_saturated`) *and* by a database that could not serve the
/// request right now (see [`AppError::is_overloaded`]), so a client has a single shape to
/// recognise and back off from instead of one of them arriving as a scrubbed 500.
pub const OVERLOADED_CODE: &str = "overloaded";
/// Message paired with [`OVERLOADED_CODE`]. Says nothing about *which* resource ran out:
/// that is an operator's question, answered by the log line, not the client's.
pub const OVERLOADED_MESSAGE: &str = "The server is busy. Try again shortly.";
/// `Retry-After` seconds on an overload rejection. One second, because the condition it
/// describes — a full in-flight slot table, a drained connection pool, a held write lock —
/// clears in milliseconds when it clears at all.
pub const OVERLOADED_RETRY_AFTER_SECS: u64 = 1;

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    /// Stable machine-readable code, independent of the transport.
    pub fn code(&self) -> &'static str {
        #[cfg(feature = "sqlx")]
        if self.is_overloaded() {
            return OVERLOADED_CODE;
        }
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::BadRequest(_) => "bad_request",
            AppError::Validation(_) => "validation",
            AppError::Conflict(_) => "conflict",
            #[cfg(feature = "sqlx")]
            AppError::Database(sqlx::Error::RowNotFound) => "not_found",
            #[cfg(feature = "sqlx")]
            AppError::Database(_) => "internal",
            AppError::Internal(_) => "internal",
        }
    }

    /// Whether the database refused this request because it is *saturated right now*,
    /// rather than because anything is wrong with the request or the schema.
    ///
    /// Two ways that happens, and both used to arrive as a scrubbed 500:
    ///
    /// * [`sqlx::Error::PoolTimedOut`] — every pooled connection was checked out for longer
    ///   than `sure_dal::POOL_ACQUIRE_TIMEOUT`. The API's in-flight ceiling is larger than
    ///   the pool, deliberately, so a burst *will* queue on acquire.
    /// * `SQLITE_BUSY` / `SQLITE_LOCKED` — another writer held the write lock for longer
    ///   than the connection's `busy_timeout`. Transient by definition: SQLite serialises
    ///   writers, so the only thing wrong is that two arrived at once.
    ///
    /// Both answer with a 503 + `Retry-After` ([`OVERLOADED_CODE`]) instead of a 500, which
    /// is what lets a client back off correctly — and keeps a 500 in the logs meaning "a bug
    /// to look at" rather than "the box was busy".
    #[cfg(feature = "sqlx")]
    pub fn is_overloaded(&self) -> bool {
        let AppError::Database(err) = self else {
            return false;
        };
        if let sqlx::Error::PoolTimedOut = err {
            return true;
        }
        if let sqlx::Error::Database(db) = err {
            return sqlite_is_busy(db.code().as_deref());
        }
        false
    }
}

/// Whether a SQLite result code means "another writer has it".
///
/// `sqlx`'s `DatabaseError::code` renders SQLite's *extended* result code in decimal, whose
/// low byte is the primary code: 5 == `SQLITE_BUSY`, 6 == `SQLITE_LOCKED`. Masking rather
/// than comparing the whole value is what catches the extended spellings — 261
/// (`SQLITE_BUSY_RECOVERY`), 517 (`SQLITE_BUSY_SNAPSHOT`), 773 (`SQLITE_BUSY_TIMEOUT`),
/// 262 (`SQLITE_LOCKED_SHAREDCACHE`) — which mean the same thing and would otherwise be
/// misread as internal errors.
#[cfg(feature = "sqlx")]
fn sqlite_is_busy(code: Option<&str>) -> bool {
    let Some(code) = code.and_then(|c| c.parse::<u32>().ok()) else {
        return false;
    };
    let primary = code & 0xff;
    primary == 5 || primary == 6
}

pub type AppResult<T> = Result<T, AppError>;

/// JSON error envelope: `{ "error": { "code": "...", "message": "..." } }`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code (e.g. `not_found`, `validation`).
    pub code: String,
    /// Human-readable description.
    pub message: String,
}

/// Bound a message that came from outside before it is stored or returned.
///
/// Both callers hold text someone else wrote: a provider's error `Display` (which has
/// historically carried the upstream payload with it) and `serde`'s deserialisation error
/// (which quotes the offending value out of the request body). Neither is length-bounded by
/// anything upstream of it, and both end up somewhere that matters — a durable `TEXT`
/// column served back over HTTP, or a 4xx body, which the error mapping passes to the client
/// verbatim because only 5xx is scrubbed.
///
/// Truncation is by `char`, not by byte: the byte at `max_chars` may be mid-codepoint in
/// UTF-8 input and slicing there panics. The marker is appended so a reader can tell the
/// text is not the whole message.
pub fn truncate_for_wire(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if out.len() < text.len() {
        out.push_str("… (truncated)");
    }
    out
}

/// The response-shaping helpers live in the `axum`-gated module below; re-exported here so
/// callers name them as `sure_core::error::{clothe_error, ..}` (and `sure_api::limits`
/// re-exports them again, where the middleware that uses them lives).
#[cfg(feature = "axum")]
pub use http::{clothe_error, overloaded_response, ErrorAlreadyClothed};

#[cfg(feature = "axum")]
mod http {
    use super::{AppError, OVERLOADED_CODE, OVERLOADED_MESSAGE, OVERLOADED_RETRY_AFTER_SECS};
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
        Json,
    };

    /// Marks a response whose body is already a proper `{ "error": { code, message } }`
    /// envelope, so the API's `request_context` middleware leaves it alone instead of
    /// scrubbing it into the generic internal-error text.
    ///
    /// Lives here rather than in `sure_api::limits` (which re-exports it) because the 503
    /// this module emits for an overloaded database needs the same protection: without the
    /// marker, a 503 is a 5xx like any other and the scrubber would rewrite its
    /// `overloaded` code to `internal`, undoing the whole point of distinguishing them.
    #[derive(Clone, Copy, Debug)]
    pub struct ErrorAlreadyClothed;

    /// Build an error response in the API's standard envelope, marked so nothing downstream
    /// rewrites it.
    ///
    /// Used for rejections that never reach a handler (rate limit, load shed, deadline) and
    /// for the overload 503 below, which would otherwise come back as an empty body, bare
    /// text, or a scrubbed generic message and break clients that expect the envelope
    /// everywhere.
    ///
    /// These short-circuit above the cache layer, so they set their own `no-store` — a
    /// transient rejection is the last thing that should be remembered as this URL's answer.
    pub fn clothe_error(status: StatusCode, code: &str, message: &str) -> Response {
        let body = super::ErrorBody {
            error: super::ErrorDetail {
                code: code.to_string(),
                message: message.to_string(),
            },
        };
        let mut response = (status, Json(body)).into_response();
        response.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        response.extensions_mut().insert(ErrorAlreadyClothed);
        response
    }

    /// The single 503 for "busy, come back": envelope, `overloaded` code, `Retry-After`.
    ///
    /// Both callers go through here on purpose. The in-flight shedder and an exhausted
    /// connection pool are the same event from a client's point of view, and a client that
    /// has to recognise two different shapes for it will get one of them wrong.
    pub fn overloaded_response() -> Response {
        let mut response = clothe_error(
            StatusCode::SERVICE_UNAVAILABLE,
            OVERLOADED_CODE,
            OVERLOADED_MESSAGE,
        );
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            OVERLOADED_RETRY_AFTER_SECS.into(),
        );
        response
    }

    impl AppError {
        fn status(&self) -> StatusCode {
            #[cfg(feature = "sqlx")]
            if self.is_overloaded() {
                return StatusCode::SERVICE_UNAVAILABLE;
            }
            match self {
                AppError::NotFound(_) => StatusCode::NOT_FOUND,
                AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
                AppError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
                AppError::Conflict(_) => StatusCode::CONFLICT,
                #[cfg(feature = "sqlx")]
                AppError::Database(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
                #[cfg(feature = "sqlx")]
                AppError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
                AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            }
        }
    }

    impl IntoResponse for AppError {
        fn into_response(self) -> Response {
            // Load, not a bug: answer with the shedder's own 503 rather than falling into
            // the 5xx scrub below, which would tell the client `internal` and give it no
            // reason to retry. The cause is still logged, once, right here.
            #[cfg(feature = "sqlx")]
            if self.is_overloaded() {
                tracing::warn!(error = %self, "database saturated; answering 503 overloaded");
                return overloaded_response();
            }
            let status = self.status();
            // Never surface internal detail (SQL errors, anyhow chains) to clients. The
            // cause is logged server-side by the handler's `#[instrument(err)]`; the HTTP
            // layer's `request_context` middleware re-clothes this with a `request_id`.
            // Client errors (4xx) keep their descriptive, safe messages.
            let message = if status.is_server_error() {
                "Internal Error: Something went wrong!".to_string()
            } else {
                self.to_string()
            };
            let body = super::ErrorBody {
                error: super::ErrorDetail {
                    code: self.code().to_string(),
                    message,
                },
            };
            (status, Json(body)).into_response()
        }
    }
}

/// Classification tests for the saturation errors. Gated on `sqlx` (and, for the response
/// shape, `axum`) because those are the features that bring the types being classified —
/// `cargo test -p sure-core --all-features` runs them, and clippy's `--all-features` pass
/// type-checks them on every commit.
#[cfg(all(test, feature = "sqlx"))]
mod sqlx_tests {
    use super::*;
    use sqlx::error::{DatabaseError, ErrorKind};
    use std::borrow::Cow;

    /// A stand-in for `sqlx::sqlite::SqliteError`, whose constructor is crate-private: all
    /// this classification reads is `code()`, and the real driver puts SQLite's *extended*
    /// result code there in decimal (see `sqlite_is_busy`).
    #[derive(Debug)]
    struct FakeDbError(&'static str);

    impl std::fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "(code: {}) synthetic", self.0)
        }
    }
    impl std::error::Error for FakeDbError {}

    impl DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            "synthetic"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn db_error(code: &'static str) -> AppError {
        AppError::Database(sqlx::Error::Database(Box::new(FakeDbError(code))))
    }

    /// W-18: waiting out `POOL_ACQUIRE_TIMEOUT` is load, not a defect. Before this it was
    /// `AppError::Database(_)` like any other and became a scrubbed 500.
    #[test]
    fn pool_exhaustion_is_overload_not_internal() {
        let err = AppError::Database(sqlx::Error::PoolTimedOut);
        assert!(err.is_overloaded());
        assert_eq!(err.code(), OVERLOADED_CODE);
    }

    /// W-30: SQLITE_BUSY past `busy_timeout` — and every extended spelling of it, plus
    /// SQLITE_LOCKED — is the same transient "another writer has it".
    #[test]
    fn every_busy_result_code_is_overload() {
        for code in [
            "5",   // SQLITE_BUSY
            "6",   // SQLITE_LOCKED
            "261", // SQLITE_BUSY_RECOVERY
            "517", // SQLITE_BUSY_SNAPSHOT
            "773", // SQLITE_BUSY_TIMEOUT
            "262", // SQLITE_LOCKED_SHAREDCACHE
        ] {
            let err = db_error(code);
            assert!(err.is_overloaded(), "code {code} should be overload");
            assert_eq!(err.code(), OVERLOADED_CODE, "code {code}");
        }
    }

    /// The inverse, which matters more: a real defect must not be laundered into a
    /// retryable 503 just because it came from the database.
    #[test]
    fn other_database_errors_stay_internal() {
        for code in [
            "1",    // SQLITE_ERROR (e.g. a syntax error)
            "11",   // SQLITE_CORRUPT
            "1555", // SQLITE_CONSTRAINT_PRIMARYKEY
            "",     // unparseable
        ] {
            let err = db_error(code);
            assert!(!err.is_overloaded(), "code {code:?} should not be overload");
            assert_eq!(err.code(), "internal", "code {code:?}");
        }
        let not_found = AppError::Database(sqlx::Error::RowNotFound);
        assert!(!not_found.is_overloaded());
        assert_eq!(not_found.code(), "not_found");
        assert!(!AppError::Internal(anyhow::anyhow!("boom")).is_overloaded());
    }

    #[cfg(feature = "axum")]
    mod response {
        use super::*;
        use axum::http::{header, StatusCode};
        use axum::response::IntoResponse;

        /// The whole point of W-18: a client sees the *same* 503 + `Retry-After` +
        /// `overloaded` envelope whether the in-flight shedder refused it or the connection
        /// pool did, and never a 500 for either.
        #[test]
        fn saturation_answers_503_overloaded_with_retry_after() {
            for err in [AppError::Database(sqlx::Error::PoolTimedOut), db_error("5")] {
                let response = err.into_response();
                assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(
                    response.headers().get(header::RETRY_AFTER).unwrap(),
                    OVERLOADED_RETRY_AFTER_SECS.to_string().as_str()
                );
                assert_eq!(
                    response.headers().get(header::CACHE_CONTROL).unwrap(),
                    "no-store"
                );
                // Marked, so the API's 5xx scrubber leaves the `overloaded` code intact
                // instead of rewriting it to `internal` — which would defeat the mapping.
                assert!(response.extensions().get::<ErrorAlreadyClothed>().is_some());
            }
            // Byte-for-byte the shedder's own body.
            assert_eq!(
                body_of(AppError::Database(sqlx::Error::PoolTimedOut).into_response()),
                body_of(overloaded_response()),
            );
        }

        /// A genuine internal error keeps its 500 and its scrubbed body: this mapping must
        /// not turn every database failure into "come back later".
        #[test]
        fn a_real_database_error_is_still_a_scrubbed_500() {
            let response = db_error("1").into_response();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert!(response.headers().get(header::RETRY_AFTER).is_none());
            let body = body_of(response);
            assert!(body.contains("\"code\":\"internal\""), "{body}");
            assert!(!body.contains("synthetic"), "detail leaked: {body}");
        }

        /// Read an error envelope out of a response.
        ///
        /// The envelope is one small in-memory buffer, so collecting it needs no runtime:
        /// a single poll with a no-op waker completes it. That keeps `sure-core` free of a
        /// tokio dev-dependency it has no other use for.
        fn body_of(response: axum::response::Response) -> String {
            use std::future::Future;
            use std::task::{Context, Poll, Waker};

            let mut cx = Context::from_waker(Waker::noop());
            let mut collect = Box::pin(axum::body::to_bytes(response.into_body(), 64 * 1024));
            let bytes = match collect.as_mut().poll(&mut cx) {
                Poll::Ready(Ok(bytes)) => bytes,
                Poll::Ready(Err(e)) => panic!("in-memory body failed to collect: {e}"),
                Poll::Pending => panic!("an in-memory body should collect in one poll"),
            };
            String::from_utf8(bytes.to_vec()).expect("the envelope is UTF-8 JSON")
        }
    }
}
