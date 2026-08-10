//! The tools, grouped by the question they answer rather than by the endpoint they replace.
//!
//! Each module contributes one `ToolRouter` and [`crate::server::SureMcp::new`] composes
//! them; `writes` is the only one whose inclusion is conditional.

pub mod accounts;
pub mod reference;
pub mod reports;
pub mod rules;
pub mod transactions;
pub mod writes;
