//! Config snapshot export & import — an opaque JSON blob the UI downloads and re-uploads.
//! The data model + all SQL live in `sure_dal::snapshot`, behind the `SnapshotRepo` port;
//! these handlers only marshal the blob through it.

use axum::extract::{DefaultBodyLimit, State};
use axum::{Json, Router};
use serde_json::Value;

use crate::config::Limits;
use crate::error::AppResult;
use crate::state::AppState;

// OTEL span names for this module's handlers.
const SNAPSHOT_EXPORT: &str = "snapshot.export";
const SNAPSHOT_IMPORT: &str = "snapshot.import";

/// Export the entire configuration and data as a JSON snapshot.
#[utoipa::path(get, path = "/api/config/export", tag = "config",
    responses((status = 200, description = "A full snapshot blob", body = serde_json::Value)))]
#[tracing::instrument(
    name = SNAPSHOT_EXPORT,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn export(State(st): State<AppState>) -> AppResult<Json<Value>> {
    Ok(Json(st.snapshot.export().await?))
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
