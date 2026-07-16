//! Config snapshot export & import — an opaque JSON blob the UI downloads and re-uploads.
//! The data model + all SQL live in `sure_dal::snapshot`; these handlers only marshal
//! the blob to/from the DAL's typed `Snapshot`.

use axum::extract::State;
use axum::{Json, Router};
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Export the entire configuration and data as a JSON snapshot.
#[utoipa::path(get, path = "/api/config/export", tag = "config",
    responses((status = 200, description = "A full snapshot blob", body = serde_json::Value)))]
pub async fn export(State(st): State<AppState>) -> AppResult<Json<Value>> {
    let snap = sure_dal::snapshot::export(&st.db).await?;
    Ok(Json(
        serde_json::to_value(snap).map_err(|e| AppError::Internal(e.into()))?,
    ))
}

/// Replace the entire database with the given snapshot. Destructive.
#[utoipa::path(post, path = "/api/config/import", tag = "config", request_body = serde_json::Value,
    responses((status = 200, description = "Import summary", body = serde_json::Value),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn import(State(st): State<AppState>, Json(body): Json<Value>) -> AppResult<Json<Value>> {
    let snap = serde_json::from_value(body)
        .map_err(|e| AppError::validation(format!("invalid snapshot: {e}")))?;
    Ok(Json(sure_dal::snapshot::import(&st.db, snap).await?))
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route("/config/export", get(export))
        .route("/config/import", post(import))
}
