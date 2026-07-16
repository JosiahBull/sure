//! Shared domain types and the workspace error type. No web framework, no database
//! connection management — just the vocabulary every other crate speaks.

pub mod error;
pub mod types;

pub use error::{AppError, AppResult, ErrorBody, ErrorDetail};
pub use types::{class_of, AccountClass, AccountKind};
