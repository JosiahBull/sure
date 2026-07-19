use serde::Serialize;
use utoipa::ToSchema;

/// The single error type shared across the workspace. Data-access and engine crates
/// return it; the API crate turns it into an HTTP response (behind the `axum` feature).
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0} not found")]
    NotFound(&'static str),

    #[error("{0}")]
    BadRequest(String),

    #[error("{0}")]
    Conflict(String),

    #[error("{0}")]
    Validation(String),

    #[cfg(feature = "sqlx")]
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    /// Stable machine-readable code, independent of the transport.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::NotFound(_) => "not_found",
            AppError::BadRequest(_) => "bad_request",
            AppError::Validation(_) => "validation",
            AppError::Conflict(_) => "conflict",
            #[cfg(feature = "sqlx")]
            AppError::Database(sqlx::Error::RowNotFound) => "not_found",
            #[cfg(feature = "sqlx")]
            AppError::Database(_) => "internal",
            AppError::Internal(_) => "internal",
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

/// JSON error envelope: `{ "error": { "code": "...", "message": "..." } }`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Stable machine-readable code (e.g. `not_found`, `validation`).
    pub code: String,
    /// Human-readable description.
    pub message: String,
}

#[cfg(feature = "axum")]
mod http {
    use super::AppError;
    use axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
        Json,
    };

    impl AppError {
        fn status(&self) -> StatusCode {
            match self {
                AppError::NotFound(_) => StatusCode::NOT_FOUND,
                AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
                AppError::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
                AppError::Conflict(_) => StatusCode::CONFLICT,
                #[cfg(feature = "sqlx")]
                AppError::Database(sqlx::Error::RowNotFound) => StatusCode::NOT_FOUND,
                #[cfg(feature = "sqlx")]
                AppError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
                AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            }
        }
    }

    impl IntoResponse for AppError {
        fn into_response(self) -> Response {
            let status = self.status();
            // Never surface internal detail (SQL errors, anyhow chains) to clients. The
            // cause is logged server-side by the handler's `#[instrument(err)]`; the HTTP
            // layer's `request_context` middleware re-clothes this with a `request_id`.
            // Client errors (4xx) keep their descriptive, safe messages.
            let message = if status.is_server_error() {
                "Internal Error: Something went wrong!".to_string()
            } else {
                self.to_string()
            };
            let body = super::ErrorBody {
                error: super::ErrorDetail {
                    code: self.code().to_string(),
                    message,
                },
            };
            (status, Json(body)).into_response()
        }
    }
}
