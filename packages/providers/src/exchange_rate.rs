//! Generic external exchange-rate source. Implement [`ExchangeRateProvider`] to connect
//! a new upstream (a paid data vendor, a different free API, ...). [`FrankfurterProvider`]
//! (see `frankfurter.rs`) ships as a credential-free default.

use async_trait::async_trait;
use rust_decimal::Decimal;

/// A single quoted rate: 1 unit of the requested base currency equals `rate` units of
/// `quote_code`.
#[derive(Debug, Clone)]
pub struct ExchangeRateQuote {
    pub quote_code: String,
    pub rate: Decimal,
    /// ISO-8601 date the rate was quoted as of (the upstream's reference date, not the
    /// time it was fetched).
    pub as_of: String,
}

/// The integration point for pulling live currency exchange rates from an upstream
/// source. One method: fetch every rate quoted against a base currency.
#[async_trait]
pub trait ExchangeRateProvider: Send + Sync {
    /// Stable identifier for this source (e.g. `"frankfurter"`).
    fn kind(&self) -> &'static str;
    /// Human-facing description.
    fn description(&self) -> &'static str;
    /// Fetch every available rate quoted against `base` (an ISO 4217 code).
    async fn fetch_rates(&self, base: &str) -> anyhow::Result<Vec<ExchangeRateQuote>>;
}
