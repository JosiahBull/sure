use std::time::Duration;

use crate::error::FrankfurterError;
use crate::models::LatestRates;

/// The public API base. Callers are expected to override it for tests; nothing in this crate
/// reaches for it implicitly.
pub const DEFAULT_BASE_URL: &str = "https://api.frankfurter.dev/v1";

/// Default ceiling on a response body this client will buffer.
///
/// The whole ECB reference table is ~2KB, so 1MiB is ~500× the real thing and still bounds what
/// a malfunctioning or hostile upstream can do to the caller's memory. A request timeout bounds
/// how *long* a response may take, never how many bytes it may be — that is what this is for.
/// Callers with their own house number override it with
/// [`FrankfurterClient::with_max_response_bytes`]; `sure-providers` does, so one const in that
/// workspace answers "how much of a response may this process buffer?" for every adapter.
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// A client for the Frankfurter exchange-rate API.
///
/// **Takes a `reqwest::Client` rather than building one.** Whether a plaintext request is
/// refused, whether a redirect is followed, and what the connect and request timeouts are, are
/// properties of the `Client`, fixed when it is built — so they belong to the application that
/// has a policy about them, not to this crate. `sure-providers` hands in the same bounded client
/// every one of its adapters shares, which is also how it points this at a loopback test proxy
/// without this crate needing to know test proxies exist.
pub struct FrankfurterClient {
    http: reqwest::Client,
    base_url: String,
    max_response_bytes: u64,
}

impl FrankfurterClient {
    /// `base_url` is the API root, without a trailing slash (e.g. [`DEFAULT_BASE_URL`]).
    pub fn new(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
        }
    }

    /// Override [`DEFAULT_MAX_RESPONSE_BYTES`].
    #[must_use]
    pub fn with_max_response_bytes(mut self, limit: u64) -> Self {
        self.max_response_bytes = limit;
        self
    }

    /// The latest reference rates against `base`: `GET /latest?base=<base>`.
    ///
    /// The URL is built by concatenation rather than through `Url::query_pairs_mut`, and that is
    /// deliberate in a way it was not for `house-pricer-client`: `base` is a three-letter
    /// currency code with nothing to percent-encode, and the record/replay proxy in front of
    /// this in every test compares the query *verbatim*. Reshaping this string — a trailing
    /// slash, a reordered parameter, an encoded one — silently invalidates every committed
    /// snapshot, and the suite would go on replaying answers to a request nothing makes any
    /// more. `sure-providers`' `tests/frankfurter.rs` asserts the exact origin-form URI for that
    /// reason.
    pub async fn latest(&self, base: &str) -> Result<LatestRates, FrankfurterError> {
        let url = format!("{}/latest?base={base}", self.base_url);
        let response = self.http.get(&url).send().await?;

        // Cloned up front: `chunk()` below needs `&mut response`, so the borrow `url()` hands
        // out cannot be held across the read loop that reports it. It is the URL actually
        // fetched rather than the one built above, which is what an operator needs to see.
        let url = response.url().to_string();
        let status = response.status();

        // Checked before anything else, because a refusal-on-volume must not be flattened into
        // "some 4xx" and retried straight away. Both statuses that mean it are covered: `429` is
        // the unambiguous one; `503` is not — it is also what an ordinary outage looks like — so
        // a `503` only counts when it *names* a `Retry-After`, which is a server saying "come
        // back then" rather than "I am broken".
        let retry_after = retry_after(response.headers(), chrono::Utc::now());
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || (status == reqwest::StatusCode::SERVICE_UNAVAILABLE && retry_after.is_some())
        {
            return Err(FrankfurterError::RateLimited {
                status: status.as_u16(),
                retry_after,
            });
        }

        // Exactly what `error_for_status` fails on, and no wider. A `3xx` deliberately falls
        // through to the body read below: the caller's client refuses to follow redirects, so a
        // redirect arrives as an ordinary response whose body then fails to deserialise. Turning
        // it into a status error here would make a *followed* redirect — the case that leaks a
        // credential header to whatever host the `Location` named — and a refused one look the
        // same from the outside. See `sure-providers`' `tests/http_bounds.rs`.
        if status.is_client_error() || status.is_server_error() {
            return Err(FrankfurterError::Http {
                status: status.as_u16(),
                url,
            });
        }

        let body = self.read_capped(response, &url).await?;
        serde_json::from_slice(&body)
            .map_err(|error| FrankfurterError::Deserialization { error, url })
    }

    /// Read a body, refusing to buffer more than [`Self::max_response_bytes`].
    ///
    /// Two guards, because either alone is bypassable. The declared `Content-Length` is checked
    /// first, so a body the upstream *declares* is oversized costs no allocation at all; but
    /// that header is absent on a chunked response and is only ever a claim, so the body is then
    /// read chunk-by-chunk against a running total and abandoned — connection dropped
    /// mid-stream, rather than drained — the moment it crosses the ceiling.
    ///
    /// `Response::chunk` rather than `bytes_stream()` on purpose: the latter lives behind
    /// reqwest's `stream` feature, which this crate does not enable; the bound is identical
    /// either way.
    async fn read_capped(
        &self,
        mut response: reqwest::Response,
        url: &str,
    ) -> Result<Vec<u8>, FrankfurterError> {
        if let Some(declared) = response.content_length() {
            self.enforce_cap(declared, url)?;
        }

        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            self.enforce_cap(body.len() as u64 + chunk.len() as u64, url)?;
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// The ceiling comparison itself, applied both to a declared `Content-Length` and to the
    /// running total actually read. Split out of [`Self::read_capped`] so the boundary is
    /// unit-testable without a `reqwest::Response`: hand-constructing one needs the `http` crate
    /// (not a dependency here), and the alternative — a live socket — is far too much machinery
    /// to catch an off-by-one in a comparison.
    fn enforce_cap(&self, bytes: u64, url: &str) -> Result<(), FrankfurterError> {
        if bytes > self.max_response_bytes {
            return Err(FrankfurterError::ResponseTooLarge {
                limit: self.max_response_bytes,
                bytes,
                url: url.to_string(),
            });
        }
        Ok(())
    }
}

/// How long a `Retry-After` asks for, in either of the two forms RFC 9110 allows.
///
/// Delay-seconds is what this upstream would actually send; the HTTP-date form is parsed via
/// `chrono`'s RFC 2822 reader, which accepts IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) — the
/// same grammar with a fixed offset — so honouring it costs no dependency beyond the one this
/// crate already has for it. A date already in the past reads as zero rather than as an error:
/// the upstream is saying "now".
///
/// **`yahoo-finance-client` carries a copy of this function, deliberately.** Each client crate
/// depends on nothing — not on the Sure workspace and not on its sibling — because that is what
/// stops either growing a domain concept, and a third crate existing to share twenty lines of
/// header parsing would cost more than the duplication does. Each copy is tested where it lives,
/// so neither can drift unnoticed.
fn retry_after(
    headers: &reqwest::header::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let raw = raw.trim();

    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let at = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    // `to_std` refuses a negative span; a date already past means "now", not "unparseable".
    Some(
        (at.with_timezone(&chrono::Utc) - now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> FrankfurterClient {
        // Aimed at a port nothing is listening on: nothing here opens a socket.
        FrankfurterClient::new(reqwest::Client::new(), "http://127.0.0.1:1/v1")
    }

    fn headers(retry_after: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            retry_after.parse().expect("a header value"),
        );
        headers
    }

    fn at(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .expect("a fixed instant")
            .with_timezone(&chrono::Utc)
    }

    /// The ceiling is a property of the client, and overriding it is how the host application
    /// keeps one answer to "how much may this process buffer?" across its adapters.
    #[test]
    fn the_body_ceiling_is_overridable() {
        assert_eq!(client().max_response_bytes, DEFAULT_MAX_RESPONSE_BYTES);
        assert_eq!(
            client().with_max_response_bytes(4096).max_response_bytes,
            4096
        );
    }

    #[test]
    fn caps_at_the_ceiling_inclusive() {
        // A real payload (~2KB) and a body exactly at the ceiling both pass; one byte past it,
        // whether declared or accumulated, does not.
        let client = client();
        let url = "http://api.frankfurter.dev/v1/latest?base=NZD";
        assert!(client.enforce_cap(2_048, url).is_ok());
        assert!(client.enforce_cap(DEFAULT_MAX_RESPONSE_BYTES, url).is_ok());
        assert!(
            client
                .enforce_cap(DEFAULT_MAX_RESPONSE_BYTES + 1, url)
                .is_err()
        );
    }

    /// The message is the whole diagnostic: a scheduler log carries three adapters' failures,
    /// and "a body was too big" names none of them.
    #[test]
    fn the_cap_error_names_the_host_and_the_size() {
        let err = client()
            .enforce_cap(
                DEFAULT_MAX_RESPONSE_BYTES * 2,
                "http://api.frankfurter.dev/v1/latest?base=NZD",
            )
            .expect_err("twice the ceiling is over it")
            .to_string();
        assert!(err.contains("api.frankfurter.dev"), "{err}");
        assert!(
            err.contains(&(DEFAULT_MAX_RESPONSE_BYTES * 2).to_string()),
            "{err}"
        );
    }

    /// The form every upstream here would actually send.
    #[test]
    fn retry_after_reads_delay_seconds() {
        let now = at("2026-08-16T03:00:00Z");
        assert_eq!(
            retry_after(&headers("120"), now),
            Some(Duration::from_secs(120))
        );
        // Surrounding whitespace is legal in a header value and means nothing.
        assert_eq!(
            retry_after(&headers("  30  "), now),
            Some(Duration::from_secs(30))
        );
        // `0` is a real answer — "try again immediately" — and must not read as absent, which
        // would have the caller substitute its own default for the nothing that was asked for.
        assert_eq!(retry_after(&headers("0"), now), Some(Duration::ZERO));
    }

    /// The other form RFC 9110 allows. Parsed through `chrono`'s RFC 2822 reader, which is why
    /// honouring it needs no new dependency — and why it is worth a test that the grammar really
    /// does overlap.
    #[test]
    fn retry_after_reads_an_http_date() {
        let now = at("2026-08-16T03:00:00Z");
        assert_eq!(
            retry_after(&headers("Sun, 16 Aug 2026 03:02:00 GMT"), now),
            Some(Duration::from_secs(120))
        );
        // Already past: the upstream is saying "now", which is a window of zero rather than an
        // unparseable value — `chrono`'s `to_std` refuses a negative span, and taking that as
        // `None` would substitute a wait nobody asked for.
        assert_eq!(
            retry_after(&headers("Sun, 16 Aug 2026 02:00:00 GMT"), now),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn an_absent_or_unreadable_retry_after_is_none() {
        let now = at("2026-08-16T03:00:00Z");
        assert_eq!(retry_after(&reqwest::header::HeaderMap::new(), now), None);
        // Neither form. The caller falls back to its own window rather than to no wait at all,
        // which is the safe direction: the upstream did refuse.
        assert_eq!(retry_after(&headers("soon"), now), None);
        // A negative delay-seconds is not a `u64` and is not a date; same answer.
        assert_eq!(retry_after(&headers("-5"), now), None);
    }
}
