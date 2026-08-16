use std::time::Duration;

/// Everything that can go wrong talking to Yahoo's chart endpoint.
///
/// Modelled on what a caller has to *branch* on rather than on HTTP in general. Three variants
/// exist because behaviour turns on them — [`Self::UnknownSymbol`], which is not a failure;
/// [`Self::NoChartData`], which is; and [`Self::RateLimited`], where coming back immediately
/// makes things worse — and the rest carry the diagnostic a scheduler log needs to say which
/// feed broke and how.
///
/// Unlike `house-pricer-client`'s equivalent, the messages here are free to quote the URL and
/// the sizes involved: a chart is public market data, not a dossier on anybody, so there is
/// nothing in a Yahoo response that must be kept out of a log.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum YahooFinanceError {
    /// Yahoo answered `404`: it has no such symbol.
    ///
    /// **Not a failure.** A delisted company (Restaurant Brands NZ, taken over in 2019) or an
    /// expired instrument (a lapsed `…RG` rights issue) 404s forever, and an account's
    /// historical holdings routinely contain several — a caller that treated this as an error
    /// would log a warning per dead ticker per poll and never price the live ones any better.
    /// `sure_providers::yahoo_finance` turns it into "no prices" and remembers it for a while.
    ///
    /// Distinct from [`Self::NoChartData`] on purpose: this is true of *every* date range, which
    /// is what makes it safe to remember. See the crate docs.
    #[error("Yahoo Finance has no data for '{symbol}' (delisted, expired, or unknown)")]
    UnknownSymbol {
        /// The Yahoo symbol that was asked for, e.g. `RBD.NZ`.
        symbol: String,
    },

    /// A `200` whose body carried no chart at all (`"result": null`, or an empty array).
    ///
    /// The upstream knows the symbol but answered in a shape this crate cannot read. Its own
    /// variant, and an error rather than an empty result, because it says nothing about future
    /// windows — memoising it the way [`Self::UnknownSymbol`] is memoised would suppress a later
    /// perfectly valid request, and returning "no prices" would hide an upstream change behind
    /// a portfolio that merely looks unpriced.
    #[error("no chart data returned for '{symbol}'")]
    NoChartData {
        /// The Yahoo symbol that was asked for.
        symbol: String,
    },

    /// The upstream refused on volume, and asked for `retry_after` if it said so at all.
    ///
    /// Its own variant rather than a [`Self::Http`] with a 429 in it, because it is the one
    /// failure whose correct handling is *not* "try again shortly". Yahoo publishes no rate
    /// limit for this endpoint, which is not the same as not having one: what it has is a
    /// temporary IP block, applied to the whole machine rather than to the request that earned
    /// it. `sure_providers::yahoo_finance` arms a stand-down window from this and refuses the
    /// next call locally.
    ///
    /// `retry_after` is `None` when the response named no `Retry-After` — which is the *normal*
    /// case here, since this endpoint sends none. How long to wait when the upstream did not say
    /// is the caller's policy, not this crate's.
    #[error("Yahoo Finance refused this request on volume with HTTP {status}")]
    RateLimited {
        /// The status that carried the refusal: `429`, or a `503` that named a `Retry-After`.
        status: u16,
        /// How long the upstream asked for, if it asked at all.
        retry_after: Option<Duration>,
    },

    /// Any other status reqwest's `error_for_status` would have failed on — a `4xx` or a `5xx`,
    /// the `404` above having already been taken out of it.
    ///
    /// Deliberately **not** every non-success status: a `3xx` is passed through to the body
    /// reader instead, exactly as `error_for_status` does. The caller builds its client with
    /// `redirect::Policy::none()`, so a redirect arrives here as an ordinary response whose body
    /// then fails to deserialise — which is the behaviour `sure-providers`'
    /// `tests/http_bounds.rs` pins, because a redirect that *was* followed would leak a
    /// credential header rather than announce itself as a status.
    ///
    /// The URL is in the message because `reqwest::Error`'s own status error had it there, and
    /// dropping it would be a real loss: this endpoint's query is the window that was asked for,
    /// and `packages/api-tests`' backfill spec recovers those two epochs from exactly this line —
    /// they are canonicalised out of everything the record/replay proxy can report.
    #[error("Yahoo Finance returned HTTP {status} for {url}")]
    Http {
        /// The status code, which is the part a caller can act on.
        status: u16,
        /// The URL that answered with it, query included.
        url: String,
    },

    /// Network-level failure: DNS, connect, TLS, timeout.
    #[error("network error talking to Yahoo Finance: {0}")]
    Network(#[from] reqwest::Error),

    /// The response body was larger than the client's configured ceiling.
    ///
    /// A request timeout bounds how *long* a response may take, not how many bytes it may be,
    /// so this is the only thing standing between a caller and an unbounded read — and this
    /// endpoint is the one with a real appetite, since a backfill asks for a decade of daily
    /// bars in one request. All three numbers are in the message on purpose: the ceiling says
    /// which limit was enforced, the size says how far over, and the URL says which feed and
    /// which symbol.
    #[error("response body from {url} is over the {limit} byte ceiling ({bytes} bytes)")]
    ResponseTooLarge {
        /// The ceiling that was hit — [`crate::DEFAULT_MAX_RESPONSE_BYTES`] unless the caller
        /// overrode it with [`crate::YahooFinanceClient::with_max_response_bytes`].
        limit: u64,
        /// What the body declared or had already reached when the read was abandoned.
        bytes: u64,
        /// The URL the oversized body came from.
        url: String,
    },

    /// The body did not deserialise — the case a field rename upstream produces, and the whole
    /// reason this crate is separate from `sure-providers`: it is fixed here, once, without
    /// touching anything that knows what a holding is.
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

    /// The distinction the whole integration turns on. A caller has to be able to tell "this
    /// symbol does not exist" from "the upstream answered with nothing" *without* matching on a
    /// message, because the first is an ordinary empty result it may remember and the second is
    /// a failure it must not.
    #[test]
    fn a_delisted_symbol_and_an_empty_document_are_different_variants() {
        let unknown = YahooFinanceError::UnknownSymbol {
            symbol: "RBD.NZ".to_string(),
        };
        let empty = YahooFinanceError::NoChartData {
            symbol: "RBD.NZ".to_string(),
        };

        assert!(matches!(unknown, YahooFinanceError::UnknownSymbol { .. }));
        assert!(matches!(empty, YahooFinanceError::NoChartData { .. }));
        // Both name the symbol: a poller walking a portfolio logs these one after another, and
        // "no chart data returned" on its own says nothing about which holding it was.
        assert!(unknown.to_string().contains("RBD.NZ"), "{unknown}");
        assert_eq!(empty.to_string(), "no chart data returned for 'RBD.NZ'");
    }

    /// A rate limit has to be distinguishable without string-matching a message: it is the one
    /// error whose correct handling is "do not come back yet", and the caller arms a cooldown
    /// from it. `None` is the ordinary case here — this endpoint sends no `Retry-After`.
    #[test]
    fn a_rate_limit_is_its_own_variant_and_survives_an_absent_retry_after() {
        let err = YahooFinanceError::RateLimited {
            status: 429,
            retry_after: None,
        };
        let YahooFinanceError::RateLimited {
            status,
            retry_after,
        } = &err
        else {
            unreachable!("constructed as a RateLimited above")
        };
        assert_eq!(*status, 429);
        assert_eq!(*retry_after, None);
        assert!(err.to_string().contains("429"), "{err}");
    }
}
