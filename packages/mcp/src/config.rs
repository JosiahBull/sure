//! What the MCP surface is allowed to do, as plain data.
//!
//! Like `sure_api::config`, this parses no environment: reading the environment is a concern
//! of *running* the server, so `sure-server`'s `config.rs` does it and hands the result here.

/// The mode enum itself lives in `sure-core`, because it is persisted (`settings.mcp_mode`)
/// as well as configured. Re-exported so a caller needs one import.
pub use sure_core::McpMode;

/// The tunables, with the defaults that are the intended settings.
#[derive(Debug, Clone, Copy)]
pub struct McpConfig {
    /// The **most** this process will ever serve, from `SURE_MCP`.
    ///
    /// A ceiling rather than the mode itself: the household picks the working mode in the
    /// app (`settings.mcp_mode`), and what is actually served is that value clamped to this
    /// one. So `SURE_MCP=read` means the settings page can offer off and read but not write,
    /// and leaving `SURE_MCP` unset — the default — means the ceiling is
    /// [`McpMode::Off`] and no toggle in the app can turn anything on.
    ///
    /// That direction matters: enabling agent access needs someone with access to the host,
    /// not just to the app.
    pub ceiling: McpMode,
    /// The ceiling on rows any one tool returns.
    ///
    /// Not a preference — a correctness guard. A ledger read that answers with four thousand
    /// rows costs more context than the question was worth and pushes the model toward doing
    /// arithmetic it is bad at; `summarize_spending` is what that traffic is meant to become.
    pub max_rows: usize,
}

/// 200 rows is roughly a screenful of ledger to a person and a few thousand tokens to a
/// model — enough to answer "show me last week", far short of enough to tempt a manual sum.
pub const DEFAULT_MAX_ROWS: usize = 200;

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            ceiling: McpMode::default(),
            max_rows: DEFAULT_MAX_ROWS,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp, stated as the property the whole design rests on: whatever the app stores,
    /// the served mode never exceeds what the environment allowed.
    #[test]
    fn the_served_mode_never_exceeds_the_ceiling() {
        let cases = [
            (McpMode::Off, McpMode::Write, McpMode::Off),
            (McpMode::Read, McpMode::Write, McpMode::Read),
            (McpMode::Read, McpMode::Read, McpMode::Read),
            (McpMode::Write, McpMode::Read, McpMode::Read),
            (McpMode::Write, McpMode::Write, McpMode::Write),
            (McpMode::Write, McpMode::Off, McpMode::Off),
        ];
        for (ceiling, stored, expected) in cases {
            assert_eq!(
                ceiling.min(stored),
                expected,
                "ceiling {ceiling:?} with stored {stored:?}"
            );
        }
    }

    /// The default install: nothing set, nothing served, and no setting can change that.
    #[test]
    fn the_default_ceiling_forbids_everything() {
        let config = McpConfig::default();
        assert_eq!(config.ceiling, McpMode::Off);
        for stored in [McpMode::Off, McpMode::Read, McpMode::Write] {
            assert_eq!(config.ceiling.min(stored), McpMode::Off);
        }
    }
}
