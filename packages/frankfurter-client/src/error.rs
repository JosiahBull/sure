use std::time::Duration;

/// Everything that can go wrong talking to Frankfurter.
///
/// Modelled on what a caller has to *branch* on rather than on HTTP in general. Two variants
/// exist because behaviour turns on them — [`Self::RateLimited`], where coming back immediately
/// makes things worse, and everything else, where it does not — and the rest carry the
/// diagnostic a scheduler log needs to say which feed broke and how.
///
/// Unlike `house-pricer-client`'s equivalent, the messages here are free to quote the URL and
/// the sizes involved: a rate table is public market data, not a dossier on anybody, so there is
/// nothing in a Frankfurter response that must be kept out of a log.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrankfurterError {
    /// The upstream refused on volume, and asked for `retry_after` if it said so at all.
    ///
    /// Its own variant rather than a [`Self::Http`] with a 429 in it, because it is the one
    /// failure whose correct handling is *not* "try again shortly": a retry against a host that
    /// has just asked for quiet is what turns a temporary throttle into a block on the whole
    /// machine. `sure_providers::frankfurter` arms a stand-down window from it and refuses the
    /// next call locally.
    ///
    /// `retry_after` is `None` when the response named no `Retry-After` (or named one in neither
    /// form RFC 9110 allows). That is a real and common answer, not a parse failure — how long
    /// to wait when the upstream did not say is the caller's policy, not this crate's.
    #[error("Frankfurter refused this request on volume with HTTP {status}")]
    RateLimited {
        /// The status that carried the refusal: `429`, or a `503` that named a `Retry-After`.
        status: u16,
        /// How long the upstream asked for, if it asked at all.
        retry_after: Option<Duration>,
    },

    /// Any other status reqwest's `error_for_status` would have failed on — a `4xx` or a `5xx`.
    ///
    /// Deliberately **not** every non-success status: a `3xx` is passed through to the body
    /// reader instead, exactly as `error_for_status` does. The caller builds its client with
    /// `redirect::Policy::none()`, so a redirect arrives here as an ordinary response whose HTML
    /// body then fails to deserialise — which is the behaviour
    /// `sure-providers`' `tests/http_bounds.rs` pins, because a redirect that *was* followed
    /// would leak a credential header rather than announce itself as a status.
    ///
    /// The URL is in the message because `reqwest::Error`'s own status error had it there, and
    /// dropping it would be a real loss: a status with no query attached does not say which
    /// request failed, and `packages/api-tests`' backfill spec recovers the window it asked for
    /// from exactly this line.
    #[error("Frankfurter returned HTTP {status} for {url}")]
    Http {
        /// The status code, which is the part a caller can act on.
        status: u16,
        /// The URL that answered with it, query included.
        url: String,
    },

    /// Network-level failure: DNS, connect, TLS, timeout.
    #[error("network error talking to Frankfurter: {0}")]
    Network(#[from] reqwest::Error),

    /// The response body was larger than the client's configured ceiling.
    ///
    /// A request timeout bounds how *long* a response may take, not how many bytes it may be,
    /// so this is the only thing standing between a caller and an unbounded read. All three
    /// numbers are in the message on purpose: the ceiling says which limit was enforced, the
    /// size says how far over, and the URL says which feed and which query — three adapters
    /// share one scheduler log, and "a response was too big" on its own names none of them.
    #[error("response body from {url} is over the {limit} byte ceiling ({bytes} bytes)")]
    ResponseTooLarge {
        /// The ceiling that was hit — [`crate::DEFAULT_MAX_RESPONSE_BYTES`] unless the caller
        /// overrode it with [`crate::FrankfurterClient::with_max_response_bytes`].
        limit: u64,
        /// What the body declared or had already reached when the read was abandoned.
        bytes: u64,
        /// The URL the oversized body came from.
        url: String,
    },

    /// The body did not deserialise — the case a field rename upstream produces, and the whole
    /// reason this crate is separate from `sure-providers`: it is fixed here, once, without
    /// touching anything that knows what an exchange rate is.
    ///
    /// Also what a redirect and a truncated response arrive as; hence the URL, which is the only
    /// thing in the message that distinguishes them from each other in a log.
    #[error("could not decode the JSON body from {url}: {error}")]
    Deserialization {
        /// The deserialisation error, which names the line and column it failed at.
        error: serde_json::Error,
        /// The URL the undecodable body came from, query included.
        url: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rate limit has to be distinguishable without string-matching a message: it is the one
    /// error whose correct handling is "do not come back yet", and the caller arms a cooldown
    /// from it.
    #[test]
    fn a_rate_limit_is_its_own_variant_and_carries_what_was_asked_for() {
        let err = FrankfurterError::RateLimited {
            status: 429,
            retry_after: Some(Duration::from_secs(120)),
        };
        let FrankfurterError::RateLimited {
            status,
            retry_after,
        } = &err
        else {
            unreachable!("constructed as a RateLimited above")
        };
        assert_eq!(*status, 429);
        assert_eq!(*retry_after, Some(Duration::from_secs(120)));
        assert!(err.to_string().contains("429"), "{err}");
    }

    /// The three numbers `sure-providers`' `http_bounds.rs` reads back out of this message. It
    /// cannot import the ceiling — it is `pub(crate)` over there — so the message is the only
    /// evidence it has that the limit enforced was the configured one.
    #[test]
    fn an_oversized_body_names_the_ceiling_the_size_and_the_url() {
        let err = FrankfurterError::ResponseTooLarge {
            limit: 8 * 1024 * 1024,
            bytes: 8 * 1024 * 1024 + 1,
            url: "http://127.0.0.1:53219/v1/latest?base=NZD".to_string(),
        }
        .to_string();
        assert!(err.contains("8388608"), "{err}");
        assert!(err.contains("8388609"), "{err}");
        assert!(err.contains("127.0.0.1:53219"), "{err}");
    }
}
