//! Generic external stock-price source. Implement [`StockPriceProvider`] to connect a
//! new upstream (a paid data vendor, a different free API, ...). [`YahooFinanceProvider`]
//! (see `yahoo_finance.rs`) ships as a credential-free default. Mirrors the shape of
//! [`crate::ExchangeRateProvider`] — a second, unrelated port with exactly one
//! implementation today.

use async_trait::async_trait;
use chrono::NaiveDate;
use rust_decimal::Decimal;

/// A single day's closing price for a ticker.
#[derive(Debug, Clone)]
pub struct StockPriceQuote {
    /// The trading day this close is for (daily resolution is all Sure needs).
    pub as_of: NaiveDate,
    pub close: Decimal,
    /// The currency the price is quoted in (e.g. the exchange's listing currency).
    pub currency_code: String,
}

/// The integration point for pulling historical daily stock prices from an upstream
/// source.
#[async_trait]
pub trait StockPriceProvider: Send + Sync {
    /// Stable identifier for this source (e.g. `"yahoo_finance"`).
    fn kind(&self) -> &'static str;
    /// Human-facing description.
    fn description(&self) -> &'static str;
    /// Fetch daily closes for `ticker` between `from` and `to` (inclusive). `exchange`
    /// is a free-text hint (e.g. `"NZX"`, from an account's `SharesMeta.exchange`) used
    /// to resolve exchange-specific symbol conventions; `None` or an unrecognised value
    /// falls back to the bare ticker.
    async fn fetch_daily_prices(
        &self,
        ticker: &str,
        exchange: Option<&str>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<StockPriceQuote>>;
}
