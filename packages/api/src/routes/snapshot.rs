//! Config snapshot export & import — an opaque JSON blob the UI downloads and re-uploads.
//! The data model + all SQL live in `sure_dal::snapshot`, behind the `SnapshotRepo` port;
//! these handlers only marshal the blob through it.

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::config::Limits;
use crate::error::AppResult;
use crate::extract::Json;
use crate::limits::overloaded_response;
use crate::state::AppState;

// OTEL span names for this module's handlers.
const SNAPSHOT_EXPORT: &str = "snapshot.export";
const SNAPSHOT_IMPORT: &str = "snapshot.import";

/// One export at a time, process-wide.
///
/// The export is the single largest allocation the API can be asked to make — the whole
/// database, serialised — and it takes no parameters, so the global in-flight ceiling
/// (`crate::limits::InFlight`, dozens of slots) is not a bound on it at all: a handful of
/// concurrent `GET /api/config/export`s multiply that blob by the number of them. One slot makes
/// the peak the size of *one* snapshot no matter who asks.
///
/// It sheds rather than queues, exactly like [`crate::limits::shed_when_saturated`]: a fast 503
/// with `Retry-After` lets the client come back, whereas queueing turns a burst into a pile of
/// requests that each still cost a full copy when their turn comes.
static EXPORT_SLOT: Semaphore = Semaphore::const_new(1);

/// Export the entire configuration and data as a JSON snapshot.
// Kept out of the doc comment on purpose: utoipa publishes that as this endpoint's public
// OpenAPI description, and an internal module path tells an API consumer nothing.
//
// The body is written by `sure_dal::snapshot::export_bytes` and handed to the response as those
// exact bytes rather than going through `Json<Value>`: parsing them back into a `Value` here
// only to re-serialise them would restore the extra full copy of the database that was the
// point of the change. The wire shape is unchanged — one JSON object, `application/json`. It is
// still one buffered body, not a streamed one; `export_bytes` says what a true streaming export
// would cost.
#[utoipa::path(get, path = "/api/config/export", tag = "config",
    responses((status = 200, description = "A full snapshot blob", body = serde_json::Value),
              // Body declared, because `overloaded_response` answers in the standard
              // `{ error: { code, message } }` envelope like every other error here — a client
              // that expects nothing on a 503 would fail to read the `overloaded` code.
              (status = 503, description = "An export is already in progress",
               body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = SNAPSHOT_EXPORT,
    level = "debug",
    skip_all,
    // No `ret`: this handler's return value *is* the database. Logging it at DEBUG would both
    // copy it again and write the household's finances into the log.
    err(level = tracing::Level::WARN),
)]
pub async fn export(State(st): State<AppState>) -> AppResult<Response> {
    let Ok(_slot) = EXPORT_SLOT.try_acquire() else {
        tracing::warn!("shedding snapshot export: one is already in progress");
        return Ok(overloaded_response());
    };
    let body = st.snapshot.export().await?;
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        // `Bytes::from` takes ownership of the `Vec`, so handing the body over is a move and
        // not the fourth copy of the database.
        axum::body::Bytes::from(body),
    )
        .into_response())
}

/// Replace the entire database with the given snapshot. Destructive.
#[utoipa::path(post, path = "/api/config/import", tag = "config", request_body = serde_json::Value,
    responses((status = 200, description = "Import summary", body = serde_json::Value),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = SNAPSHOT_IMPORT,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn import(State(st): State<AppState>, Json(body): Json<Value>) -> AppResult<Json<Value>> {
    Ok(Json(st.snapshot.import(body).await?))
}

pub fn router(limits: &Limits) -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new().route("/config/export", get(export)).route(
        "/config/import",
        // The matching export is a full database dump, so the global 2 MB body cap
        // would make a snapshot round trip fail on any established ledger.
        post(import).layer(DefaultBodyLimit::max(limits.max_snapshot_body_bytes)),
    )
}
