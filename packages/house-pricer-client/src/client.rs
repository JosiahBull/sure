use crate::error::{HousePricerError, ResponseBody};
use crate::models::PropertyMatch;

/// The public API base. Callers are expected to override it for tests; nothing in this crate
/// reaches for it implicitly.
pub const DEFAULT_BASE_URL: &str = "https://www.housepricer.co.nz/api/property/core";

/// Default ceiling on a response body this client will buffer.
///
/// A match response is ~1.5KB, so 1MiB is ~700× the real thing and still bounds what a
/// malfunctioning or hostile upstream can do to the caller's memory. A request timeout bounds
/// how *long* a response may take, never how many bytes it may be — that is what this is for.
/// Callers with their own house number override it with
/// [`HousePricerClient::with_max_response_bytes`]; `sure-providers` does, so one const in that
/// workspace answers "how much of a response may this process buffer?" for every adapter.
pub const DEFAULT_MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// A client for the House Pricer property API.
///
/// **Takes a `reqwest::Client` rather than building one.** Whether a plaintext request is
/// refused, what the connect and request timeouts are, and which TLS backend is used are
/// properties of the `Client`, fixed when it is built — so they belong to the application that
/// has a policy about them, not to this crate. `sure-providers` hands in the same bounded
/// client every one of its adapters shares, which is also how it points this at a loopback test
/// proxy without this crate needing to know test proxies exist.
pub struct HousePricerClient {
    http: reqwest::Client,
    base_url: String,
    max_response_bytes: u64,
}

impl HousePricerClient {
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

    /// Look one address up: `GET /match?q=<address>`.
    ///
    /// [`HousePricerError::NotFound`] is the ordinary answer for an address the upstream does
    /// not cover — House Pricer is Christchurch-only — and for one with a typo. It is an error
    /// variant rather than an `Option` so a caller has to decide what "no match" means where it
    /// stands; returning `Option` here would make it just as easy to ignore as to handle.
    ///
    /// The address is appended with `query_pairs_mut` rather than interpolated: `q` is a street
    /// address, so it always needs percent-encoding, and hand-rolling that is how a `&` or a `#`
    /// in an address silently truncates the search. It also encodes identically every time,
    /// which a record/replay proxy needs — it compares the query verbatim.
    pub async fn match_address(&self, query: &str) -> Result<PropertyMatch, HousePricerError> {
        let query = query.trim();
        if query.is_empty() {
            // Refused here rather than sent. The upstream answers a blank `q` with a 400, and
            // the round trip teaches the caller nothing it does not already know.
            return Err(HousePricerError::BadRequest {
                message: "an address is needed to look up a property estimate".to_string(),
            });
        }

        // Parsing the concatenated base is safe in a way parsing the bare base would not be:
        // `Url` normalises an empty path to `/`, so round-tripping a bare origin through it can
        // produce a doubled slash — but `{base}/match` always has a path already.
        let mut url = reqwest::Url::parse(&format!("{}/match", self.base_url))
            .map_err(|e| HousePricerError::InvalidUrl(e.to_string()))?;
        url.query_pairs_mut().append_pair("q", query);

        let response = self.http.get(url).send().await?;

        let status = response.status();

        // Before the other status checks, because a refusal-on-volume must not be flattened into
        // a generic `Http` that loses the one thing distinguishing it: that retrying shortly
        // makes it worse. Both statuses that mean it are covered. `429` is unambiguous; `503` is
        // not — it is also what an ordinary outage looks like — so a `503` only counts when it
        // *names* a `Retry-After`, which is a server saying "come back then" rather than "I am
        // broken".
        let retry_after = retry_after(response.headers(), chrono::Utc::now());
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || (status == reqwest::StatusCode::SERVICE_UNAVAILABLE && retry_after.is_some())
        {
            return Err(HousePricerError::RateLimited {
                status: status.as_u16(),
                retry_after,
            });
        }

        if status == reqwest::StatusCode::NOT_FOUND {
            // The body says `{"_embedded":{"errors":[{"message":"No matching house found"}]}}`;
            // it is not read, because the status alone carries the same meaning and the body
            // would be one more echo of the address to hold.
            return Err(HousePricerError::NotFound);
        }
        if status == reqwest::StatusCode::BAD_REQUEST {
            return Err(HousePricerError::BadRequest {
                message: format!("HTTP {status}"),
            });
        }
        if !status.is_success() {
            return Err(HousePricerError::Http {
                status: status.as_u16(),
            });
        }

        let body = self.read_capped(response).await?;
        serde_json::from_slice(&body).map_err(|error| HousePricerError::Deserialization {
            error,
            response_body: Some(ResponseBody::from_bytes(&body)),
        })
    }

    /// Read a body, refusing to buffer more than [`Self::max_response_bytes`].
    ///
    /// Checks the declared `Content-Length` first — a well-behaved upstream saves the read
    /// entirely — and then bounds the actual read anyway, because the header is a claim rather
    /// than a promise and a chunked response carries none at all.
    async fn read_capped(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, HousePricerError> {
        let limit = self.max_response_bytes;
        if let Some(declared) = response.content_length()
            && declared > limit
        {
            return Err(HousePricerError::ResponseTooLarge { limit });
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() as u64 + chunk.len() as u64 > limit {
                return Err(HousePricerError::ResponseTooLarge { limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

/// How long a `Retry-After` asks for, in either of the two forms RFC 9110 allows.
///
/// Delay-seconds is what this upstream would send; the HTTP-date form is parsed via `chrono`'s
/// RFC 2822 reader, which accepts IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) — the same
/// grammar with a fixed offset — so honouring it costs no new dependency. A date already in the
/// past reads as zero rather than as an error: the upstream is saying "now".
///
/// Duplicated from `frankfurter-client` and `yahoo-finance-client` rather than shared, for the
/// reason those two already record: a fourth crate existing to hold twenty lines of header
/// parsing would couple three clients that are otherwise independent, which is the property that
/// stops any of them growing a domain concept. Each copy is tested where it lives.
fn retry_after(
    headers: &reqwest::header::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<std::time::Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let raw = raw.trim();

    if let Ok(seconds) = raw.parse::<u64>() {
        return Some(std::time::Duration::from_secs(seconds));
    }

    let at = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    // `to_std` refuses a negative span; a date already past means "now", not "unparseable".
    Some(
        (at.with_timezone(&chrono::Utc) - now)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            value.parse().expect("a header value"),
        );
        headers
    }

    fn at(raw: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(raw)
            .expect("a fixture timestamp")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn reads_both_spellings_of_retry_after() {
        let now = at("2026-08-16T00:00:00Z");
        // Delay-seconds, which is what this upstream would actually send.
        assert_eq!(
            retry_after(&headers("120"), now),
            Some(std::time::Duration::from_secs(120))
        );
        // IMF-fixdate, the other form RFC 9110 allows.
        assert_eq!(
            retry_after(&headers("Sun, 16 Aug 2026 00:02:00 GMT"), now),
            Some(std::time::Duration::from_secs(120))
        );
        // Already past: the upstream is saying "now", not sending something unparseable.
        assert_eq!(
            retry_after(&headers("Sat, 15 Aug 2026 00:00:00 GMT"), now),
            Some(std::time::Duration::ZERO)
        );
        // Absent and unparseable both mean "it did not say", which the caller answers with its
        // own default window rather than treating as an error.
        assert_eq!(retry_after(&reqwest::header::HeaderMap::new(), now), None);
        assert_eq!(retry_after(&headers("soon"), now), None);
    }

    fn client() -> HousePricerClient {
        // Aimed at a port nothing is listening on: every test here asserts on a refusal that
        // happens before a socket is opened.
        HousePricerClient::new(reqwest::Client::new(), "http://127.0.0.1:1/api")
    }

    /// A blank address never reaches the wire. The one caller that can produce one is a
    /// pre-flight from an account with no address typed yet, and it would be a guaranteed 400.
    #[tokio::test]
    async fn a_blank_address_is_refused_without_a_request() {
        for blank in ["", "   ", "\t\n"] {
            let err = client()
                .match_address(blank)
                .await
                .expect_err("a blank address cannot be looked up");
            assert!(
                matches!(err, HousePricerError::BadRequest { .. }),
                "{err:?}"
            );
        }
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
}
