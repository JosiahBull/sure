use sure_core::{AppError, AppResult, McpMode};
pub use sure_core::{Settings, UpdateSettings};

use crate::Db;

#[derive(Debug)]
struct SettingsRow {
    base_currency_code: String,
    mcp_mode: String,
    updated_at: String,
}

impl TryFrom<SettingsRow> for Settings {
    type Error = AppError;

    /// Fallible for one column: `mcp_mode` is stored as `TEXT` (the one legal place a domain
    /// enum is a string) and parsed here, on the way in. A `CHECK` constraint keeps the three
    /// values honest, so this only fails on a hand-edited database — and failing is right,
    /// because the alternative is defaulting, and defaulting *up* would serve a surface
    /// nobody asked for.
    fn try_from(r: SettingsRow) -> AppResult<Self> {
        Ok(Settings {
            base_currency_code: r.base_currency_code,
            mcp_mode: r
                .mcp_mode
                .parse::<McpMode>()
                .map_err(AppError::validation)?,
            updated_at: r.updated_at,
        })
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn get(db: &Db) -> AppResult<Settings> {
    sqlx::query_as!(
        SettingsRow,
        "SELECT base_currency_code, mcp_mode, updated_at FROM settings WHERE id = 1"
    )
    .fetch_one(db)
    .await?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn update(db: &Db, input: UpdateSettings) -> AppResult<Settings> {
    let code = input.base_currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &code).await? {
        return Err(AppError::validation(format!("unknown currency '{code}'")));
    }
    // `COALESCE` rather than two statements: an absent `mcp_mode` leaves the stored value
    // alone, so a caller changing only the base currency (the web page did exactly this
    // before agent access existed) cannot silently reset it to `off`.
    let mcp_mode = input.mcp_mode.map(|m| m.as_str());
    sqlx::query_as!(
        SettingsRow,
        "UPDATE settings
         SET base_currency_code = ?1,
             mcp_mode = COALESCE(?2, mcp_mode),
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = 1
         RETURNING base_currency_code, mcp_mode, updated_at",
        code,
        mcp_mode
    )
    .fetch_one(db)
    .await?
    .try_into()
}

/// The stored MCP mode on its own, for the per-request lookup the MCP transport makes.
///
/// A scalar read rather than [`get`]: it runs on every MCP call, and the rest of the row
/// (and its enum parse) is not wanted there.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn mcp_mode(db: &Db) -> AppResult<McpMode> {
    sqlx::query_scalar!("SELECT mcp_mode FROM settings WHERE id = 1")
        .fetch_one(db)
        .await?
        .parse::<McpMode>()
        .map_err(AppError::validation)
}

/// The configured base reporting currency code (used by the report queries).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn base_currency(db: &Db) -> AppResult<String> {
    Ok(
        sqlx::query_scalar!("SELECT base_currency_code FROM settings WHERE id = 1")
            .fetch_one(db)
            .await?,
    )
}
