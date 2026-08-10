use axum::extract::State;
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::{McpMode, Settings, UpdateSettings};

// OTEL span names for this module's handlers.
const SETTINGS_GET: &str = "settings.get";
const SETTINGS_UPDATE: &str = "settings.update";

/// Settings as the app needs to render them.
///
/// A DTO twin rather than `Settings` itself, for one reason: two of these four fields are not
/// stored anywhere. `mcp_ceiling` is what the `SURE_MCP` environment variable permits and
/// `mcp_effective` is what is therefore actually served — both facts about how this process
/// was *started*, which no row can hold. Without them a settings page cannot explain why a
/// control it is showing does nothing, and would have to re-implement the clamp to guess.
#[derive(Debug, Serialize, ToSchema)]
pub struct SettingsView {
    /// Currency all reports are normalised into.
    pub base_currency_code: String,
    /// The MCP mode the household has asked for.
    pub mcp_mode: McpMode,
    /// The most this process will serve, from `SURE_MCP`. `off` — the default — means the
    /// endpoint is not mounted at all and no choice here can change that.
    pub mcp_ceiling: McpMode,
    /// What is actually served: `mcp_mode` clamped to `mcp_ceiling`.
    pub mcp_effective: McpMode,
    pub updated_at: String,
}

impl SettingsView {
    fn of(settings: Settings, ceiling: McpMode) -> Self {
        Self {
            mcp_effective: ceiling.min(settings.mcp_mode),
            mcp_ceiling: ceiling,
            base_currency_code: settings.base_currency_code,
            mcp_mode: settings.mcp_mode,
            updated_at: settings.updated_at,
        }
    }
}

/// Fetch global settings.
#[utoipa::path(get, path = "/api/settings", tag = "settings",
    responses((status = 200, body = SettingsView)))]
#[tracing::instrument(
    name = SETTINGS_GET,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_settings(State(st): State<AppState>) -> AppResult<Json<SettingsView>> {
    Ok(Json(SettingsView::of(
        st.settings.get().await?,
        st.mcp_ceiling,
    )))
}

/// Update global settings: the base reporting currency, and how much of the MCP (agent)
/// surface to serve.
#[utoipa::path(put, path = "/api/settings", tag = "settings",
    request_body = UpdateSettings,
    responses((status = 200, body = SettingsView), (status = 422, body = crate::error::ErrorBody)))]
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
) -> AppResult<Json<SettingsView>> {
    // Refused rather than clamped. Storing `write` while serving `read` would make the
    // settings page state something untrue about the running server, and "your request was
    // silently reduced" is a worse thing to discover later than a 422 now.
    if let Some(requested) = input.mcp_mode {
        if requested > st.mcp_ceiling {
            return Err(AppError::validation(format!(
                "SURE_MCP permits at most '{}' on this server, so agent access cannot be set \
                 to '{}'. Change the SURE_MCP environment variable and restart to raise it.",
                st.mcp_ceiling.as_str(),
                requested.as_str()
            )));
        }
    }
    Ok(Json(SettingsView::of(
        st.settings.update(input).await?,
        st.mcp_ceiling,
    )))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/settings", get(get_settings).put(update_settings))
}
