//! The MCP boundary: Sure's ledger, reachable by an agent.
//!
//! A second *driving adapter*, sibling to `sure-api`. It depends on `sure-app` and nothing
//! below it — no SQL, no `sure_dal`, no listener of its own. `sure-server` builds the state
//! and mounts the transport, exactly as it does for the HTTP routes.
//!
//! # Why this is not the REST API with a different hat on
//!
//! `sure-api` exposes 118 operations, and the generated OpenAPI document is right there.
//! Turning that into 118 tools is the reliable way to make a server a model uses badly: the
//! tool list alone costs more context than most answers, and `PUT /api/accounts/{id}` is not
//! a task anyone has. The tools here are shaped like questions instead — [`tools::reports`]'s
//! `summarize_spending` exists precisely so nothing ever pulls four thousand rows to add them
//! up — and there are eighteen of them at the widest.
//!
//! # What crosses this boundary
//!
//! Everything a tool returns goes to whichever model the connecting client runs. Transaction
//! descriptions carry account numbers, IRD numbers, payee names and card last-fours. That is
//! a real change in posture for an app whose README opens with "no logins, no cloud", and it
//! is why the whole surface is off unless `SURE_MCP` says otherwise, and why writes need a
//! second, separate opt-in. See `docs/MCP.md`.
//!
//! # The two rules the wire format exists to enforce
//!
//! Money leaves as a decimal string beside its currency, never as the minor units it is
//! stored in: a model handed `-4250` reports "$4,250". And a row-returning tool caps at
//! [`config::McpConfig::max_rows`], asking for one row more than it will show so that
//! "there is more than this" is something it *knows* rather than something it implies.
//! Both live in [`convert`].

// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `114_269_63` == $114,269.63); clippy's grouping lint fights that convention. The
// tests here are full of the literals precisely because rendering them is what this crate
// has to get right.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod config;
pub mod convert;
pub mod error;
pub mod http;
pub mod manifest;
pub mod prompts;
pub mod resources;
pub mod server;
pub mod state;
pub mod tools;

pub use config::{McpConfig, McpMode};
pub use http::http_service;
pub use server::SureMcp;
pub use state::McpState;
