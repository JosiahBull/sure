use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::error::AppResult;
use crate::state::AppState;

pub use sure_dal::settings::{Settings, UpdateSettings};

// OTEL span names for this module's handlers.
const SETTINGS_GET: &str = "settings.get";
const SETTINGS_UPDATE: &str = "settings.update";

/// Fetch global settings.
#[utoipa::path(get, path = "/api/settings", tag = "settings",
    responses((status = 200, body = Settings)))]
#[tracing::instrument(
    name = SETTINGS_GET,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_settings(State(st): State<AppState>) -> AppResult<Json<Settings>> {
    Ok(Json(sure_dal::settings::get(&st.db).await?))
}

/// Update global settings (currently just the base reporting currency).
#[utoipa::path(put, path = "/api/settings", tag = "settings",
    request_body = UpdateSettings,
    responses((status = 200, body = Settings), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = SETTINGS_UPDATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update_settings(
    State(st): State<AppState>,
    Json(input): Json<UpdateSettings>,
) -> AppResult<Json<Settings>> {
    Ok(Json(sure_dal::settings::update(&st.db, input).await?))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/settings", get(get_settings).put(update_settings))
}
