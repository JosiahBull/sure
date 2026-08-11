use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How much of the MCP (agent) surface is served.
///
/// Ordered deliberately — `Off < Read < Write` — because the environment variable `SURE_MCP`
/// is a *ceiling* and the stored setting is clamped to it with [`Ord::min`]. Adding a variant
/// means deciding where it sits in that order, which is why the derive is spelled out rather
/// than hand-written comparisons living at each call site.
///
/// Lives here rather than in `sure-mcp` because it is persisted (`settings.mcp_mode`) and
/// crosses the wire: CLAUDE.md rule 1 puts a closed set that is read or written as text in
/// `sure-core`, parsed at the edge exactly once.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum McpMode {
    /// Nothing is served. On a default install this is also the ceiling, so `/mcp` is not a
    /// route at all.
    #[default]
    Off,
    /// The read tools, resources and prompts.
    Read,
    /// Everything in [`McpMode::Read`], plus the tools that write.
    Write,
}

impl McpMode {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`.
    pub fn as_str(self) -> &'static str {
        match self {
            McpMode::Off => "off",
            McpMode::Read => "read",
            McpMode::Write => "write",
        }
    }

    /// Whether the tools that change the ledger are served.
    pub fn writes(self) -> bool {
        match self {
            McpMode::Write => true,
            McpMode::Off | McpMode::Read => false,
        }
    }
}

impl std::str::FromStr for McpMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.trim() {
            "off" => McpMode::Off,
            "read" => McpMode::Read,
            "write" => McpMode::Write,
            other => {
                return Err(format!(
                    "unknown MCP mode '{other}' (expected off, read or write)"
                ));
            }
        })
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Settings {
    /// Currency all reports are normalised into.
    pub base_currency_code: String,
    /// How much of the MCP surface the household has asked for. What is actually served is
    /// this clamped to the `SURE_MCP` ceiling — see [`SettingsView::mcp_ceiling`].
    pub mcp_mode: McpMode,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSettings {
    pub base_currency_code: String,
    /// Absent leaves the stored mode alone, so a caller changing only the base currency need
    /// not know this field exists.
    #[serde(default)]
    pub mcp_mode: Option<McpMode>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordering the ceiling relies on. Written as a test because `min` reading the wrong
    /// way round would silently *raise* what a deployment serves.
    #[test]
    fn the_modes_order_from_least_to_most_capable() {
        assert!(McpMode::Off < McpMode::Read);
        assert!(McpMode::Read < McpMode::Write);
        assert_eq!(McpMode::Write.min(McpMode::Read), McpMode::Read);
        assert_eq!(McpMode::Read.min(McpMode::Off), McpMode::Off);
        // A setting above the ceiling is clamped down to it, never the other way.
        assert_eq!(McpMode::Write.min(McpMode::Off), McpMode::Off);
    }

    #[test]
    fn a_mode_round_trips_through_its_wire_form() {
        for mode in [McpMode::Off, McpMode::Read, McpMode::Write] {
            assert_eq!(mode.as_str().parse::<McpMode>(), Ok(mode));
        }
    }

    #[test]
    fn an_unrecognised_mode_is_an_error_rather_than_a_default() {
        let err = "readonly".parse::<McpMode>().unwrap_err();
        assert!(err.contains("readonly"), "{err}");
        assert!(err.contains("off, read or write"), "{err}");
    }

    #[test]
    fn only_write_mode_writes() {
        assert!(McpMode::Write.writes());
        assert!(!McpMode::Read.writes());
        assert!(!McpMode::Off.writes());
    }
}
