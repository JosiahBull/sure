//! The transport: MCP over Streamable HTTP, as a plain `tower::Service`.
//!
//! This module knows nothing about axum. It hands back a service, and `sure-server` — the
//! only crate that decides where anything listens — nests it into the router `sure-api`
//! already assembles. That is what keeps the MCP endpoint inside the existing middleware
//! stack (panic catching, request ids, tracing, the rate limiter, the body cap) instead of
//! standing up a second listener with none of it.

use std::sync::Arc;

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::config::McpConfig;
use crate::server::SureMcp;
use crate::state::McpState;

/// Build the MCP endpoint.
///
/// # Stateless, by choice
///
/// `legacy_session_mode: false` + `json_response: true` means every call is one POST in and
/// one JSON body out, with no long-lived SSE stream behind it. That is not a performance
/// preference — it is what keeps this endpoint compatible with the machinery already
/// wrapped around it. A held-open stream would sit against `sure_api::cache::timeout`'s
/// 30-second request deadline, and would still be open when the shutdown drain came for it.
///
/// # The `Host` allowlist is load-bearing
///
/// `rmcp` defaults to accepting loopback authorities only, because a local MCP server is a
/// DNS-rebinding target: a page the user visits can resolve its own hostname to `127.0.0.1`
/// and POST to this endpoint from their browser. That default is kept. Serving Sure on a
/// real hostname therefore needs that hostname listed — which `sure-server` derives from
/// `CORS_ALLOWED_ORIGINS`, so the two answers to "who may reach this" cannot drift apart.
pub fn http_service(
    state: McpState,
    config: McpConfig,
    allowed_hosts: Vec<String>,
) -> StreamableHttpService<SureMcp, LocalSessionManager> {
    let mut server_config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        // An in-flight tool call is part of the drain, not something the process walks away
        // from mid-write. A child token so cancelling MCP does not cancel everything else.
        .with_cancellation_token(state.shutdown.child_token());
    if !allowed_hosts.is_empty() {
        // Extended, not replaced: loopback stays reachable however the server is deployed,
        // because that is where a developer's inspector and `claude mcp add` point.
        server_config.allowed_hosts.extend(allowed_hosts);
    }

    StreamableHttpService::new(
        // Run per request: the handler is a thin wrapper over `Arc`s, so this is a few
        // pointer clones, and it means one call can never see another's state.
        move || Ok(SureMcp::new(state.clone(), config)),
        Arc::new(LocalSessionManager::default()),
        server_config,
    )
}
