//! Reference [`ExchangeRateProvider`] backed by the Frankfurter API
//! (<https://frankfurter.dev>) — free, keyless, ECB reference rates. No credentials or
//! signup needed, so it's a reasonable zero-config default.

use std::collections::BTreeMap;

use async_trait::async_trait;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;

use sure_app::ports::{ExchangeRateProvider, ExchangeRateQuote};

const DEFAULT_BASE_URL: &str = "https://api.frankfurter.dev/v1";

pub struct FrankfurterProvider {
    base_url: String,
    client: reqwest::Client,
}

impl FrankfurterProvider {
    pub fn new() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

impl Default for FrankfurterProvider {
    fn default() -> Self {
        Self::new()
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
        let url = format!("{}/latest?base={base}", self.base_url);
        let body: LatestResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
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
}
