//! Reference [`ExchangeRateProvider`] backed by the Frankfurter API
//! (<https://frankfurter.dev>) — free, keyless, ECB reference rates. No credentials or
//! signup needed, so it's a reasonable zero-config default.
//!
//! **The wire format is not here.** `frankfurter-client` owns the URL shape, the query encoding,
//! the status codes and the JSON contract; this file owns what Sure does with the answer. The
//! split is what keeps somebody else's endpoint from reaching into this workspace's domain
//! logic: a field rename is a one-line change in that crate, which cannot name an account or a
//! currency pair, so nothing recompiles its idea of anything. What stays here is what is Sure's
//! and not the upstream's — **`f64` → `Decimal`, and what an exchange-rate quote is** — plus the
//! client policy (`Endpoint`, the shared bounded `reqwest::Client`, one body ceiling for every
//! adapter) and the pacing (`Throttle`), which is about this *process*'s outbound budget rather
//! than about this endpoint.

use async_trait::async_trait;
use frankfurter_client::{FrankfurterClient, FrankfurterError, LatestRates};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

use sure_app::ports::{ExchangeRateProvider, ExchangeRateQuote};

use crate::http::{Endpoint, Pacing, Throttle};

/// The real API. `pub` because the composition root owns the decision of where this provider
/// points (it is the only place configuration is read) and needs a default to fall back to.
///
/// Re-exported from the client rather than restated, so the two cannot drift: this const and the
/// one the client would use by default are the same string by construction.
pub const DEFAULT_BASE_URL: &str = frankfurter_client::DEFAULT_BASE_URL;

/// What this adapter calls the upstream in an error a person will read.
///
/// One const because two messages carry it — the stand-down the throttle arms, and the refusal
/// the *next* call gets — and they land in `provider_syncs.detail` and in a 422 body, where two
/// spellings of the same host would read as two different problems.
const HOST: &str = "Frankfurter";

pub struct FrankfurterProvider {
    client: FrankfurterClient,
    throttle: Throttle,
}

impl FrankfurterProvider {
    /// The only constructor, and deliberately so: there is no argument-free `new()` that
    /// reaches for [`DEFAULT_BASE_URL`] itself. That const is the composition root's fallback
    /// (`Config::from_env` parses it into an [`Endpoint`]), and a second constructor holding the
    /// same URL would be the one a future caller reached for by reflex — pointing an adapter at
    /// the live API from inside a test, past the configuration that was supposed to decide it.
    /// The same reasoning removed `Registry`'s `Default`; see `lib.rs`.
    ///
    /// In practice the endpoint is either that parsed default or the record/replay proxy a test
    /// binds on loopback, which is the only way the fetch path below is exercisable at all
    /// without reaching the live API.
    ///
    /// The `reqwest::Client` is built here and handed to the wire crate, exactly as
    /// `AkahuProvider` does with `akahu-client`: whether a plaintext request is refused, whether
    /// a redirect is followed, and what the timeouts are, are properties of the client that
    /// [`Endpoint`] decides — not something a crate that only knows a JSON shape should have an
    /// opinion about. The body ceiling comes from the same place for the same reason, so this
    /// process has one answer to "how much of a response may we buffer?" rather than one per
    /// upstream.
    pub fn with_endpoint(endpoint: Endpoint, pacing: Pacing) -> Self {
        let client = FrankfurterClient::new(crate::http::client(&endpoint), endpoint.url())
            .with_max_response_bytes(crate::http::MAX_BODY_BYTES);
        Self {
            client,
            throttle: Throttle::new(pacing),
        }
    }
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
        self.throttle.acquire(HOST).await?;
        match self.client.latest(base).await {
            Ok(table) => Ok(parse_quotes(table)),
            // The one outcome that changes what this process does next rather than only what it
            // reports: a rate limit arms a stand-down window, so the *next* caller is refused
            // before a request goes out instead of adding to the burst that caused this.
            Err(FrankfurterError::RateLimited {
                status,
                retry_after,
            }) => Err(self.throttle.note_refusal(HOST, status, retry_after).await),
            // CLAUDE.md rule 2's escape hatch: `FrankfurterError` is `#[non_exhaustive]`, so a
            // catch-all is the only option — and it is the right answer anyway, because every
            // remaining variant means the same thing to this caller (the rates could not be
            // fetched) and differs only in the message. `RateLimited` above is the one that
            // changes behaviour, and it is named.
            Err(other) => Err(anyhow::Error::new(other)),
        }
    }
}

/// Turn the upstream's `{code: rate}` map into quotes. `Decimal::from_f64` (rather than
/// `from_f64_retain`) is used deliberately: it gives the shortest decimal that round-trips
/// to the same float (`0.87207`), not the exact binary expansion (`0.87206999999...`).
fn parse_quotes(table: LatestRates) -> Vec<ExchangeRateQuote> {
    table
        .rates
        .into_iter()
        .filter_map(|(quote_code, rate)| {
            Decimal::from_f64(rate).map(|rate| ExchangeRateQuote {
                quote_code,
                rate,
                as_of: table.date.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built as a value rather than parsed from JSON, which is the visible dividend of the
    /// split: what this test is about is `f64` → `Decimal` and what a quote carries, and it no
    /// longer restates a wire format to get at either. Whether the JSON *parses* — the field
    /// names, the ignored `amount`/`base` — is `frankfurter-client`'s own test, next to the
    /// struct that would have to change.
    #[test]
    fn maps_a_rate_table_to_quotes_without_float_drift() {
        let quotes = parse_quotes(LatestRates::new(
            "2026-07-16",
            [("EUR", 0.87207), ("NZD", 1.7078)],
        ));

        assert_eq!(quotes.len(), 2);
        // A `BTreeMap` iterates by code, so the order is the upstream's alphabet rather than
        // whatever a hash seed decided this run.
        assert_eq!(quotes[0].quote_code, "EUR");
        // The `from_f64` property, and the reason these two values are here: the nearest `f64`
        // to 0.87207 is 0.87206999999999994898…, so a rate that renders as five digits is
        // evidence of the shortest-round-trip conversion rather than of a value that happened to
        // be exactly representable.
        assert_eq!(quotes[0].rate.to_string(), "0.87207");
        // The upstream's own reference date, on every quote — not the day it was fetched.
        assert_eq!(quotes[0].as_of, "2026-07-16");
        assert_eq!(quotes[1].quote_code, "NZD");
        assert_eq!(quotes[1].rate.to_string(), "1.7078");
        assert_eq!(quotes[1].as_of, "2026-07-16");
    }

    /// The one const that could silently drift now that the URL lives in another crate.
    #[test]
    fn the_default_endpoint_is_the_clients_own() {
        assert_eq!(DEFAULT_BASE_URL, frankfurter_client::DEFAULT_BASE_URL);
    }
}
