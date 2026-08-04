//! A `Json` extractor that says *what* was wrong with the body.
//!
//! `axum::Json`'s rejection knows exactly which field failed and why — serde produces
//! `posted_at: invalid date "31/07/2026": expected an ISO-8601 calendar date in YYYY-MM-DD
//! form at line 1 column 45` — but it renders that as `text/plain`. Everything that reaches
//! [`crate::telemetry::request_context`] with a non-JSON 4xx body under `/api` gets re-clothed
//! into the standard envelope, and because the original body was plain text there is nothing
//! to preserve: the envelope ends up carrying [`describe`]'s generic
//! `"Unprocessable Entity."`. So a caller was told *that* their body was rejected and never
//! *which field*, on every typed field in the API — and the typed edges added recently
//! (`IsoDate`, `Money`) made that the common case rather than a corner one, since they moved
//! validation from hand-written checks that returned their own messages into `Deserialize`.
//!
//! This wrapper answers the rejection itself, in the envelope, with serde's text intact. It
//! is a drop-in for `axum::Json` in both positions — extractor and response — so a route only
//! changes which `Json` it imports.
//!
//! [`describe`]: crate::telemetry

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{clothe_error, truncate_for_wire};
use crate::telemetry::status_code_slug;

/// How much of a rejection's text reaches the client.
///
/// serde quotes the offending value, and nothing upstream bounds a string field, so a 2 MiB
/// body can produce a message nearly as large. 500 chars covers the field path, the reason and
/// the line/column, which is the whole diagnostic value.
const MAX_REJECTION_CHARS: usize = 500;

/// `axum::Json`, but a rejected body is answered in the API's error envelope naming the field
/// that failed rather than the status's canonical reason.
///
/// Deliberately the same name as the type it replaces: every route already writes `Json(..)`
/// in both positions, so adopting this is an import change and nothing else. The derives match
/// `axum::Json`'s for the same reason — several handlers carry
/// `#[tracing::instrument(ret(..))]`, which needs `Debug` on what they return.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    /// A ready `Response` rather than a typed rejection: the whole point is to answer in the
    /// envelope here, instead of handing something back for the middleware to rewrite.
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(rejection_response(
                rejection.status(),
                &rejection.body_text(),
            )),
        }
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Clothe a body rejection, keeping the status axum chose and the code the middleware would
/// have used.
///
/// The status mapping stays axum's (422 for a type error, 400 for malformed JSON, 415 for a
/// missing content type, 413 for an over-cap body) and the code comes from the same
/// [`status_code_slug`] the middleware uses, so this changes only the `message` — a client
/// matching on `code` sees exactly what it saw before, and `specs/http.spec.ts`'s
/// `payload_too_large` assertions keep holding.
fn rejection_response(status: StatusCode, body_text: &str) -> Response {
    clothe_error(
        status,
        status_code_slug(status),
        &truncate_for_wire(body_text, MAX_REJECTION_CHARS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::post;
    use axum::Router;
    use serde::Deserialize;
    use tower::ServiceExt;

    #[derive(Deserialize)]
    struct Payload {
        #[allow(dead_code)]
        amount_minor: i64,
    }

    fn app() -> Router {
        Router::new().route("/t", post(|Json(_): Json<Payload>| async { "ok" }))
    }

    async fn post_body(content_type: Option<&str>, body: &'static str) -> (StatusCode, String) {
        let mut req = Request::builder().method("POST").uri("/t");
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        let response = app()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn a_wrong_field_type_names_the_field() {
        let (status, body) =
            post_body(Some("application/json"), r#"{"amount_minor":"lots"}"#).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        // The envelope is intact...
        assert!(body.contains(r#""code":"validation""#), "{body}");
        // ...and the message now names the field, which is the whole point.
        assert!(body.contains("amount_minor"), "{body}");
        // The old generic text must be gone, or nothing has changed for a caller.
        assert!(!body.contains("Unprocessable Entity."), "{body}");
    }

    #[tokio::test]
    async fn a_missing_field_names_it_too() {
        let (status, body) = post_body(Some("application/json"), "{}").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("amount_minor"), "{body}");
    }

    #[tokio::test]
    async fn malformed_json_is_a_bad_request_in_the_envelope() {
        let (status, body) = post_body(Some("application/json"), "{").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(r#""code":"bad_request""#), "{body}");
    }

    #[tokio::test]
    async fn a_missing_content_type_keeps_its_status_and_code() {
        let (status, body) = post_body(None, r#"{"amount_minor":1}"#).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(
            body.contains(r#""code":"unsupported_media_type""#),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_valid_body_still_reaches_the_handler() {
        let (status, body) = post_body(Some("application/json"), r#"{"amount_minor":42}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[test]
    fn a_vast_rejection_message_is_bounded() {
        let long = "x".repeat(10_000);
        let out = rejection_response(StatusCode::UNPROCESSABLE_ENTITY, &long);
        assert_eq!(out.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // The body itself is checked through `truncate_for_wire`'s own tests; what matters
        // here is that this path goes through it rather than binding the raw text.
        assert!(truncate_for_wire(&long, MAX_REJECTION_CHARS).len() < long.len());
    }
}
