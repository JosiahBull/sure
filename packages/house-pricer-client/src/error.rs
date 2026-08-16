/// The raw body of a response that could not be deserialised, kept for debugging.
///
/// **This is a dossier on somebody's house.** A `/match` response carries the street address,
/// the GPS centroid, the title boundary polygon, the legal description, and the land and
/// improvement values — for whatever dwelling the person running Sure typed in, which is
/// usually the one they live in. That is closer to Akahu's bank traffic than to Frankfurter's
/// rate table, and it is why this type exists at all rather than the body being interpolated
/// into an error message.
///
/// So nothing in this crate ever prints it:
///
/// - [`HousePricerError::Deserialization`]'s `Display` shows only the `serde_json` error, which
///   already names the line and column the parse gave up at — the diagnostic part;
/// - this type's `Debug` reports the length and nothing else, so a caller that logs the error
///   with `{:?}` — or an error wrapper that does it for them — cannot spill the body by
///   accident. `sure_app::sync::sync_detail` bounds provider error text for the same reason
///   from the other direction, and `scripts/pii-scan.mjs` refuses a `house_pricer` recording
///   outright.
///
/// Read it with [`ResponseBody::as_str`] when you actually want it, and treat what comes back
/// as personal data: it is not safe to log, persist, or return to an API client.
#[derive(Clone)]
pub struct ResponseBody(String);

impl ResponseBody {
    /// Wrap raw response bytes, replacing any invalid UTF-8 with the replacement character.
    ///
    /// JSON is UTF-8 by definition, so the lossy conversion only matters for a body that is
    /// truncated or wasn't JSON in the first place — exactly the cases this type exists for.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self(String::from_utf8_lossy(bytes).into_owned())
    }

    /// The body as a string. **Sensitive** — see the type-level note.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The length of the body in bytes. Safe to log.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the body was empty. Safe to log.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ResponseBody {
    /// Redacted deliberately — the length is diagnostic, the contents are not ours to print.
    /// See the type-level note.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ResponseBody(<redacted, {} bytes>)", self.0.len())
    }
}

/// Everything that can go wrong talking to House Pricer.
///
/// Modelled on the shapes the endpoint actually produces rather than on HTTP in general: it is
/// undocumented, and the only statuses observed are `200`, `400` for a blank `q`, and `404` for
/// an address it does not cover. Anything else lands in [`Self::Http`] with its status, which is
/// enough for a caller to decide whether to retry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HousePricerError {
    /// No property matched. **Not a failure** — House Pricer covers one city, so every address
    /// outside Christchurch answers this way, as does one with a typo.
    ///
    /// Its own variant rather than an `Option` return so the caller decides what "no match"
    /// means in its context; `sure_providers::house_pricer` turns it into `Ok(None)`.
    #[error("no property matched the address given")]
    NotFound,

    /// The upstream refused on volume, and asked for `retry_after` if it said so at all.
    ///
    /// Its own variant rather than a [`Self::Http`] with a 429 in it, because it is the one
    /// outcome that changes what the *caller* does next rather than only what it reports:
    /// coming back immediately makes it worse. `sure-providers` turns this into a stand-down
    /// window so the next caller is refused before a request goes out.
    ///
    /// Parsing `Retry-After` is this crate's job — it owns the response the header arrived on —
    /// while deciding what window to turn it into is the caller's policy. Same division as
    /// `frankfurter-client` and `yahoo-finance-client`.
    #[error("House Pricer refused this request with HTTP {status} (rate limited)")]
    RateLimited {
        /// The status that carried the refusal: `429`, or a `503` that named a `Retry-After`.
        status: u16,
        /// How long the upstream asked for, if it asked at all.
        retry_after: Option<std::time::Duration>,
    },

    /// The request was rejected as malformed — in practice, a blank or unusable `q`.
    #[error("House Pricer rejected the request: {message}")]
    BadRequest {
        /// The upstream's message. Not the body — see [`ResponseBody`].
        message: String,
    },

    /// Any other non-success status.
    #[error("House Pricer returned HTTP {status}")]
    Http {
        /// The status code, which is the part a caller can act on.
        status: u16,
    },

    /// Network-level failure: DNS, connect, TLS, timeout.
    #[error("network error talking to House Pricer: {0}")]
    Network(#[from] reqwest::Error),

    /// The response body was larger than the client's configured ceiling.
    ///
    /// A request timeout bounds how *long* a response may take, not how many bytes it may be,
    /// so this is the only thing standing between a caller and an unbounded read. Raise the
    /// ceiling with [`crate::HousePricerClient::with_max_response_bytes`] if a legitimate
    /// response is hitting it.
    #[error("House Pricer response exceeded {limit} bytes")]
    ResponseTooLarge {
        /// The ceiling that was hit.
        limit: u64,
    },

    /// The body did not deserialise — the case a field rename upstream produces, and the whole
    /// reason this crate is separate from `sure-providers`: it is fixed here, once, without
    /// touching anything that knows what a valuation is.
    ///
    /// The `Display` is only ever the `serde_json` error, never the body itself. See
    /// [`ResponseBody`].
    #[error("could not deserialise the House Pricer response: {error}")]
    Deserialization {
        /// The deserialisation error, which names the line and column it failed at.
        error: serde_json::Error,
        /// The body that failed to parse, if it was read before the failure.
        ///
        /// **Sensitive**, and deliberately absent from this error's `Display` and redacted in
        /// its `Debug` — see [`ResponseBody`].
        response_body: Option<ResponseBody>,
    },

    /// The configured base URL could not be joined with the request path.
    ///
    /// Carries the rendered message rather than `url::ParseError`, so this crate needs no
    /// direct `url` dependency of its own: reqwest re-exports the `Url` type but not its error,
    /// and taking the whole crate to name one variant would be a dependency for a string.
    #[error("invalid House Pricer base URL: {0}")]
    InvalidUrl(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole type exists for: a body carrying somebody's address must not
    /// reach a log through the error that quotes it.
    #[test]
    fn a_response_body_is_redacted_in_debug_and_absent_from_display() {
        let body = r#"{"streetAddress":"1 Invented Street","gpsCentroid":[0,0]}"#;
        let error = HousePricerError::Deserialization {
            error: serde_json::from_str::<i32>("{}").expect_err("not an integer"),
            response_body: Some(ResponseBody::from_bytes(body.as_bytes())),
        };

        let debug = format!("{error:?}");
        assert!(!debug.contains("Invented Street"), "{debug}");
        assert!(debug.contains("<redacted"), "{debug}");
        assert!(debug.contains(&body.len().to_string()), "{debug}");

        let display = error.to_string();
        assert!(!display.contains("Invented Street"), "{display}");

        // Still reachable on purpose, for a caller that has decided it wants it.
        let HousePricerError::Deserialization { response_body, .. } = &error else {
            unreachable!("constructed as a Deserialization above")
        };
        assert_eq!(response_body.as_ref().map(ResponseBody::as_str), Some(body));
    }

    /// A "no match" has to be distinguishable without string-matching a message: it is the
    /// ordinary answer for any address outside Christchurch, and the caller maps it to `None`.
    #[test]
    fn a_missing_property_is_its_own_variant() {
        assert!(matches!(
            HousePricerError::NotFound,
            HousePricerError::NotFound
        ));
        assert_eq!(
            HousePricerError::NotFound.to_string(),
            "no property matched the address given"
        );
    }
}
