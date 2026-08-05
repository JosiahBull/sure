//! Reference [`ExchangeRateProvider`] backed by the Frankfurter API
//! (<https://frankfurter.dev>) — free, keyless, ECB reference rates. No credentials or
//! signup needed, so it's a reasonable zero-config default.

use std::collections::BTreeMap;

use async_trait::async_trait;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;

use sure_app::ports::{ExchangeRateProvider, ExchangeRateQuote};

use crate::http::Endpoint;

/// The real API. `pub` because the composition root owns the decision of where this provider
/// points (it is the only place configuration is read) and needs a default to fall back to.
pub const DEFAULT_BASE_URL: &str = "https://api.frankfurter.dev/v1";

pub struct FrankfurterProvider {
    endpoint: Endpoint,
    client: reqwest::Client,
}

impl FrankfurterProvider {
    /// The only constructor, and deliberately so: there is no argument-free `new()` that
    /// reaches for [`DEFAULT_BASE_URL`] itself. That const is the composition root's fallback
    /// (`Config::from_env` parses it into an [`Endpoint`]), and a second constructor holding
    /// the same URL would be the one a future caller reached for by reflex — pointing an
    /// adapter at the live API from inside a test, past the configuration that was supposed to
    /// decide it. The same reasoning removed `Registry`'s `Default`; see `lib.rs`.
    ///
    /// In practice the endpoint is either that parsed default or the record/replay proxy a
    /// test binds on loopback, which is the only way the fetch path below is exercisable at
    /// all without reaching the live API.
    ///
    /// The client is built from the endpoint rather than shared: whether a plaintext request
    /// is refused is a property of the `Client`, fixed when it is built, not something a
    /// per-request URL can override.
    pub fn with_endpoint(endpoint: Endpoint) -> Self {
        let client = crate::http::client(&endpoint);
        Self { endpoint, client }
    }
}

#[derive(Debug, Deserialize)]
struct LatestResponse {
    date: String,
    rates: BTreeMap<String, f64>,
}

#[async_trait]
impl ExchangeRateProvider for FrankfurterProvider {
    fn kind(&self) -> &'static str {
        "frankfurter"
    }

    fn description(&self) -> &'static str {
        "Daily exchange rates, sourced from European Central Bank reference rates"
    }

    async fn fetch_rates(&self, base: &str) -> anyhow::Result<Vec<ExchangeRateQuote>> {
        let url = format!("{}/latest?base={base}", self.endpoint.url());
        let response = self.client.get(&url).send().await?.error_for_status()?;
        // `json_capped`, not `.json()`: the whole rate table is ~2KB, and the request timeout
        // bounds how long an upstream may talk, not how much it may say. See `http.rs`.
        let body: LatestResponse = crate::http::json_capped(response).await?;
        Ok(parse_quotes(body))
    }
}

/// Turn the upstream's `{code: rate}` map into quotes. `Decimal::from_f64` (rather than
/// `from_f64_retain`) is used deliberately: it gives the shortest decimal that round-trips
/// to the same float (`0.87207`), not the exact binary expansion (`0.87206999999...`).
fn parse_quotes(body: LatestResponse) -> Vec<ExchangeRateQuote> {
    body.rates
        .into_iter()
        .filter_map(|(quote_code, rate)| {
            Decimal::from_f64(rate).map(|rate| ExchangeRateQuote {
                quote_code,
                rate,
                as_of: body.date.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_response() {
        let body: LatestResponse = serde_json::from_str(
            r#"{"amount":1.0,"base":"USD","date":"2026-07-16","rates":{"EUR":0.87207,"NZD":1.7078}}"#,
        )
        .unwrap();
        let mut quotes = parse_quotes(body);
        quotes.sort_by(|a, b| a.quote_code.cmp(&b.quote_code));

        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[0].quote_code, "EUR");
        assert_eq!(quotes[0].rate.to_string(), "0.87207");
        assert_eq!(quotes[0].as_of, "2026-07-16");
        assert_eq!(quotes[1].quote_code, "NZD");
        assert_eq!(quotes[1].rate.to_string(), "1.7078");
    }

    #[test]
    fn decodes_the_body_bytes_the_capped_reader_accumulates() {
        // `crate::http::json_capped` ends in `serde_json::from_slice` over the buffer it
        // built chunk-by-chunk, not `Response::json`'s own decode — so the same payload has
        // to deserialise from raw bytes, split across chunk boundaries and all. A realistic
        // body is ~2KB, three orders of magnitude under the 8MiB ceiling.
        let wire = br#"{"amount":1.0,"base":"USD","date":"2026-07-16","rates":{"NZD":1.7078}}"#;
        let body: LatestResponse = serde_json::from_slice(wire).unwrap();
        let quotes = parse_quotes(body);

        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].quote_code, "NZD");
        assert_eq!(quotes[0].rate.to_string(), "1.7078");
    }
}
