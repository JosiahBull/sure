//! `AppError` on the way out to a client.
//!
//! The one mapping — the MCP counterpart of `sure-core`'s `IntoResponse` for HTTP, and it
//! differs from that one in what it is willing to say.

use rmcp::model::ErrorCode;
use rmcp::ErrorData;
use sure_core::AppError;

/// A `Result` a tool body can `?` through.
pub type ToolResult<T> = Result<T, ErrorData>;

/// Deliberately incurious — see [`to_mcp`].
const INTERNAL_MESSAGE: &str = "the server hit an internal error; its log has the detail";

/// What a caller is told when something inside fails.
///
/// The caller-facing codes keep their message: "account not found", "name is required" are
/// the model's only route to fixing its own call, and they say nothing it could not learn by
/// asking again.
///
/// The rest do not. Unlike the HTTP boundary there is no request id to trade for the detail
/// (`sure-api`'s `telemetry::scrub_internal_error` prints one, so a developer can find the
/// log line from what the client was shown); here the cause is logged and a fixed sentence
/// goes back. A model relaying an error verbatim into a chat transcript is exactly the
/// audience that should not receive a database path, a table name, or whatever a third
/// party put in its error body.
///
/// # Why this dispatches on [`AppError::code`] rather than on the variant
///
/// `AppError::Database` exists only when `sure-core`'s `sqlx` feature is on, which for this
/// crate depends on what else is in the build — so a `match` naming every variant would
/// compile in the workspace and fail on `cargo check -p sure-mcp` alone. (`sure-app`'s
/// `stock_prices::is_unusable_quote` sidesteps the same problem with a `matches!`.)
/// `AppError::code` is `sure-core`'s own answer to this: a stable code chosen by an
/// exhaustive match *inside* the crate where the `cfg` works, documented as
/// "independent of the transport". MCP is a transport.
pub fn to_mcp(err: AppError) -> ErrorData {
    let code = err.code();
    match code {
        // The call was wrong, and saying how is the only way it gets fixed. MCP has no
        // richer vocabulary than "your arguments were the problem", so the message carries
        // which way they were.
        "not_found" | "bad_request" | "validation" => {
            ErrorData::new(ErrorCode::INVALID_PARAMS, err.to_string(), None)
        }
        // Not the arguments but the state they met — a sync already running, a person who
        // still owns accounts. Retrying may genuinely work, which is worth saying.
        "conflict" => ErrorData::new(ErrorCode::INVALID_REQUEST, err.to_string(), None),
        // Saturation, not a fault. Already phrased for a client by `sure-core`, and says
        // nothing about *which* resource ran out — that is an operator's question.
        "overloaded" => ErrorData::new(
            ErrorCode::INTERNAL_ERROR,
            sure_core::error::OVERLOADED_MESSAGE,
            None,
        ),
        // A price feed or bank misbehaved. Worth distinguishing from an internal fault
        // because this one is worth retrying — but `AppError::Upstream` carries up to 500
        // characters of whatever the third party said, which is not ours to forward.
        "upstream" => {
            tracing::warn!(error = %err, "mcp tool failed against an upstream");
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                "an upstream service failed; the server log has the detail",
                None,
            )
        }
        // `"internal"`, and anything a future variant introduces. A wildcard over a
        // `&'static str` returned by another crate — the one arm that cannot be enumerated
        // here — and it fails safe: an unrecognised code is treated as an internal fault,
        // so a new variant leaks nothing while waiting to be given its own arm.
        _ => {
            tracing::error!(error = %err, code, "mcp tool failed");
            ErrorData::new(ErrorCode::INTERNAL_ERROR, INTERNAL_MESSAGE, None)
        }
    }
}

/// The caller's own arguments were wrong in a way no service got to judge — a date that
/// will not parse, an amount that is not a decimal, a `group_by` that is not one of the four.
pub fn invalid_params(message: impl Into<String>) -> ErrorData {
    ErrorData::new(ErrorCode::INVALID_PARAMS, message.into(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_facing_error_keeps_the_message_that_tells_them_how_to_fix_it() {
        for (err, expected) in [
            (AppError::NotFound("account"), "account not found"),
            (
                AppError::BadRequest("from is not a date".into()),
                "from is not a date",
            ),
            (
                AppError::Validation("name is required".into()),
                "name is required",
            ),
        ] {
            let data = to_mcp(err);
            assert_eq!(data.code, ErrorCode::INVALID_PARAMS);
            assert_eq!(data.message, expected);
        }
    }

    #[test]
    fn a_conflict_is_the_callers_request_not_their_arguments() {
        let data = to_mcp(AppError::Conflict("a sync is already running".into()));
        assert_eq!(data.code, ErrorCode::INVALID_REQUEST);
        assert_eq!(data.message, "a sync is already running");
    }

    /// The property that matters: nothing an internal fault carried reaches the caller.
    /// A model will paste whatever it is handed straight into a chat transcript.
    #[test]
    fn an_internal_fault_says_nothing_about_itself() {
        let data = to_mcp(AppError::Internal(anyhow::anyhow!(
            "no such table: transactions in /Users/someone/data/sure.db"
        )));
        assert_eq!(data.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(data.message, INTERNAL_MESSAGE);
        assert!(!data.message.contains("sure.db"));
        assert!(data.data.is_none());
    }

    #[test]
    fn an_upstream_failure_is_reported_without_quoting_the_upstream() {
        let data = to_mcp(AppError::Upstream(
            "Yahoo said: {\"error\":\"token 9f3a-secret expired\"}".into(),
        ));
        assert_eq!(data.code, ErrorCode::INTERNAL_ERROR);
        assert!(data.message.contains("upstream"), "{}", data.message);
        assert!(!data.message.contains("secret"), "{}", data.message);
    }

    /// Pins the dispatch to `AppError::code`'s vocabulary. If `sure-core` ever renames a
    /// code, this fails here rather than silently routing a 4xx-shaped error into the
    /// scrubbed internal arm — where the caller would lose the message that fixes the call.
    #[test]
    fn every_code_this_maps_is_still_a_code_sure_core_produces() {
        for (err, expected) in [
            (AppError::NotFound("account"), "not_found"),
            (AppError::BadRequest(String::new()), "bad_request"),
            (AppError::Validation(String::new()), "validation"),
            (AppError::Conflict(String::new()), "conflict"),
            (AppError::Upstream(String::new()), "upstream"),
            (AppError::Internal(anyhow::anyhow!("x")), "internal"),
        ] {
            assert_eq!(err.code(), expected);
        }
        assert_eq!(sure_core::error::OVERLOADED_CODE, "overloaded");
    }
}
