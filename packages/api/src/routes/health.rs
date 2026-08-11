use axum::{Router, routing::get};
use serde::Serialize;
use utoipa::ToSchema;

use crate::extract::Json;
use crate::state::AppState;

// OTEL span names for this module's handlers.
const META_HEALTH: &str = "meta.health";

#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    /// Always `"ok"` when the service is up.
    pub status: String,
    pub name: String,
    pub version: String,
}

/// Liveness probe.
#[utoipa::path(
    get,
    path = "/api/health",
    tag = "meta",
    responses((status = 200, description = "Service is healthy", body = Health))
)]
#[tracing::instrument(
    name = META_HEALTH,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
)]
pub async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".to_string(),
        name: env!("CARGO_PKG_NAME").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}
