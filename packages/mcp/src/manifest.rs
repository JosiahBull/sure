//! The tool contract, as a file you can read a diff of.
//!
//! `packages/mcp/tool-manifest.json` is to this crate what `packages/client/openapi.json` is
//! to `sure-api`: the surface, written down and committed, so that changing it is a visible
//! act rather than a side effect. A renamed argument, a description that stopped mentioning
//! the field list, a write tool that drifted into the read tier — each shows up as a diff a
//! reviewer sees, instead of as a behaviour change nobody noticed until an agent did the
//! wrong thing with it.
//!
//! Regenerate with `cargo test -p sure-mcp -- --ignored update_the_tool_manifest`.

use serde::Serialize;

use crate::config::McpMode;
use crate::server::SureMcp;

/// One tool, reduced to the parts a client actually sees.
#[derive(Debug, Serialize, PartialEq)]
pub struct ToolSummary {
    pub name: String,
    /// The tier it appears in: `"read"` for a tool present in read mode, `"write"` for one
    /// that only appears once writes are enabled.
    pub tier: &'static str,
    pub description: String,
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    /// Argument names, sorted. The full JSON Schema is not carried: it is large, and its
    /// churn (schemars version, key order) would drown the changes worth reviewing.
    pub arguments: Vec<String>,
    pub required: Vec<String>,
}

/// The whole surface, both tiers, in a stable order.
pub fn manifest() -> Vec<ToolSummary> {
    let read_names: std::collections::HashSet<String> = tools_for(McpMode::Read)
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();

    let mut out: Vec<ToolSummary> = tools_for(McpMode::Write)
        .into_iter()
        .map(|t| {
            let schema = &t.input_schema;
            let properties = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|o| {
                    let mut keys: Vec<String> = o.keys().cloned().collect();
                    keys.sort();
                    keys
                })
                .unwrap_or_default();
            let required = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| {
                    let mut keys: Vec<String> = a
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    keys.sort();
                    keys
                })
                .unwrap_or_default();
            let annotations = t.annotations.as_ref();
            ToolSummary {
                tier: if read_names.contains(t.name.as_ref()) {
                    "read"
                } else {
                    "write"
                },
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                read_only: annotations.and_then(|a| a.read_only_hint),
                destructive: annotations.and_then(|a| a.destructive_hint),
                idempotent: annotations.and_then(|a| a.idempotent_hint),
                arguments: properties,
                required,
            }
        })
        .collect();
    // Sorted by name, not by registration order: the order routers are added is an
    // implementation detail, and letting it reorder the file would make every reshuffle
    // look like a contract change.
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The tools served at `mode` — the same composition `call_tool` dispatches through, so this
/// cannot describe a surface the server does not actually have.
fn tools_for(mode: McpMode) -> Vec<rmcp::model::Tool> {
    SureMcp::tool_names_for(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = include_str!("../tool-manifest.json");

    fn rendered() -> String {
        let mut json = serde_json::to_string_pretty(&manifest()).expect("manifest serialises");
        json.push('\n');
        json
    }

    /// The gate. Any change to a tool's name, description, arguments, annotations or tier
    /// has to be committed alongside the code that caused it.
    #[test]
    fn the_committed_manifest_matches_the_tools_actually_registered() {
        assert_eq!(
            rendered(),
            MANIFEST,
            "the tool surface changed. Review the difference, then regenerate with:\n  \
             cargo test -p sure-mcp -- --ignored update_the_tool_manifest"
        );
    }

    /// Not a snapshot — an invariant. A write tool that appears in read mode is the one
    /// failure this whole design exists to prevent, and it would otherwise be a diff a
    /// reviewer had to notice rather than a test that fails.
    #[test]
    fn read_mode_registers_no_tool_that_can_write() {
        for tool in tools_for(McpMode::Read) {
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(false);
            assert!(
                read_only,
                "`{}` is registered in read mode but is not annotated read-only",
                tool.name
            );
        }
    }

    /// The complement: every write tool is reachable once writes are on, and each one says
    /// it writes. A tool annotated read-only in the write router would be lying to a client
    /// that gates on the hint.
    #[test]
    fn write_mode_adds_tools_and_every_one_of_them_admits_it_writes() {
        let read: std::collections::HashSet<String> = tools_for(McpMode::Read)
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        let write = tools_for(McpMode::Write);
        assert!(
            write.len() > read.len(),
            "write mode registered no extra tools"
        );
        for tool in write {
            if read.contains(tool.name.as_ref()) {
                continue;
            }
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|a| a.read_only_hint)
                .unwrap_or(true);
            assert!(
                !read_only,
                "`{}` only appears in write mode but claims to be read-only",
                tool.name
            );
        }
    }

    /// Off is off — not "read-only", *nothing*. This is now reachable at runtime rather
    /// than only at boot: the endpoint stays mounted while `SURE_MCP` permits it, and the
    /// household switching agent access off in the app has to empty the surface completely.
    #[test]
    fn off_mode_serves_no_tools_at_all() {
        assert!(
            tools_for(McpMode::Off).is_empty(),
            "switching agent access off must leave no tools, not just no writing ones"
        );
    }

    /// Every tool needs a description: it is the entire basis on which a model chooses
    /// between them, and an undescribed tool is one that gets called by accident.
    #[test]
    fn every_tool_describes_itself() {
        for tool in tools_for(McpMode::Write) {
            let description = tool.description.as_deref().unwrap_or_default();
            assert!(
                description.len() > 40,
                "`{}` has no useful description ({description:?})",
                tool.name
            );
        }
    }

    /// Writes the manifest. Ignored, so a normal run only ever *checks*.
    #[test]
    #[ignore = "regenerates the committed manifest; run deliberately"]
    fn update_the_tool_manifest() {
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tool-manifest.json"),
            rendered(),
        )
        .expect("manifest is writable");
    }
}
