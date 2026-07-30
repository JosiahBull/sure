//! Reference [`StockPriceProvider`] backed by Yahoo Finance's unofficial "chart" JSON
//! endpoint (`query1.finance.yahoo.com/v8/finance/chart/{symbol}`) — free, keyless,
//! covers both NZX and US listings (the same endpoint the popular `yfinance` Python
//! library wraps). It's undocumented and could change without notice, same caveat as
//! depending on Frankfurter for exchange rates.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::Mutex;

use sure_app::ports::{StockPriceProvider, StockPriceQuote};

const BASE_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Yahoo has no published rate limit for this endpoint, but hammering it risks a
/// temporary IP block — this keeps consecutive requests from this provider instance
/// spaced at least this far apart, regardless of how many callers share it.
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(500);

pub struct YahooFinanceProvider {
    base_url: String,
    client: reqwest::Client,
    last_request: Mutex<Option<Instant>>,
}

impl YahooFinanceProvider {
    pub fn new() -> Self {
        Self {
            base_url: BASE_URL.to_string(),
            client: reqwest::Client::new(),
            last_request: Mutex::new(None),
        }
    }

    /// Block until at least [`MIN_REQUEST_INTERVAL`] has elapsed since the last call
    /// returned from this method. Holding the lock across the sleep serializes
    /// concurrent callers (cron + on-demand lookups sharing one `Arc<Self>|`) instead of
    /// letting them all wake up and fire at once.
    async fn throttle(&self) {
        let mut last_request = self.last_request.lock().await;
        if let Some(last) = *last_request {
            let elapsed = last.elapsed();
            if elapsed < MIN_REQUEST_INTERVAL {
                tokio::time::sleep(MIN_REQUEST_INTERVAL - elapsed).await;
            }
        }
        *last_request = Some(Instant::now());
    }
}

impl Default for YahooFinanceProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct ChartResponse {
    chart: Chart,
}

#[derive(Debug, Deserialize)]
struct Chart {
    result: Option<Vec<ChartResult>>,
}

#[derive(Debug, Deserialize)]
struct ChartResult {
    meta: ChartMeta,
    timestamp: Option<Vec<i64>>,
    indicators: ChartIndicators,
}

#[derive(Debug, Deserialize)]
struct ChartMeta {
    currency: String,
    /// Seconds offset from UTC for the exchange this symbol trades on — needed to turn
    /// a UTC timestamp back into the correct local trading-day date.
    gmtoffset: i64,
}

#[derive(Debug, Deserialize)]
struct ChartIndicators {
    quote: Vec<ChartQuote>,
}

#[derive(Debug, Deserialize)]
struct ChartQuote {
    close: Vec<Option<f64>>,
}

/// Best-effort ticker suffix for exchanges this app actually deals in (an NZ-based
/// household's likely NZX/ASX holdings alongside US ones); anything else, including a
/// bare US ticker, needs no suffix.
fn symbol_for(ticker: &str, exchange: Option<&str>) -> String {
    let ticker = ticker.trim().to_uppercase();
    let suffix = match exchange.map(|e| e.trim().to_uppercase()).as_deref() {
        Some("NZX") => Some(".NZ"),
        Some("ASX") => Some(".AX"),
        _ => None,
    };
    match suffix {
        Some(s) => format!("{ticker}{s}"),
        None => ticker,
    }
}

fn to_unix(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
}

#[async_trait]
impl StockPriceProvider for YahooFinanceProvider {
    fn kind(&self) -> &'static str {
        "yahoo_finance"
    }

    fn description(&self) -> &'static str {
        "Daily closing prices for listed shares and funds, from Yahoo Finance"
    }

    async fn fetch_daily_prices(
        &self,
        ticker: &str,
        exchange: Option<&str>,
        from: NaiveDate,
        to: NaiveDate,
    ) -> anyhow::Result<Vec<StockPriceQuote>> {
        let symbol = symbol_for(ticker, exchange);
        // Pad the requested range by a day on each side: Yahoo buckets timestamps by
        // the exchange's local trading day, so a UTC-midnight boundary can otherwise
        // clip the first or last day depending on which side of UTC the exchange sits.
        let period1 = to_unix(from - ChronoDuration::days(1));
        let period2 = to_unix(to + ChronoDuration::days(1));
        let url = format!(
            "{}/{symbol}?period1={period1}&period2={period2}&interval=1d",
            self.base_url
        );

        self.throttle().await;
        let response = self
            .client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await?;

        // A delisted stock (e.g. RBD after its 2019 takeover) or an expired instrument
        // (e.g. a lapsed "…RG" rights issue) comes back as 404 with a "symbol may be
        // delisted" body. That's legitimately "no prices available", not a failure — an
        // account's historical holdings routinely include such symbols — so return empty
        // and let the caller leave those positions unpriced, rather than surfacing a hard
        // error that the backfill/poller then logs as a warning for every delisted ticker.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!(%symbol, "no price data (delisted, expired, or unknown symbol)");
            return Ok(Vec::new());
        }

        let body: ChartResponse = response.error_for_status()?.json().await?;

        let result = body
            .chart
            .result
            .and_then(|mut r| {
                if r.is_empty() {
                    None
                } else {
                    Some(r.remove(0))
                }
            })
            .ok_or_else(|| anyhow::anyhow!("no chart data returned for '{symbol}'"))?;

        Ok(parse_quotes(result))
    }
}

/// Zip the parallel `timestamp`/`close` arrays into quotes, skipping days with no close
/// (non-trading days inside the requested range come back as `null`).
fn parse_quotes(result: ChartResult) -> Vec<StockPriceQuote> {
    let Some(timestamps) = result.timestamp else {
        return Vec::new();
    };
    let Some(quote) = result.indicators.quote.into_iter().next() else {
        return Vec::new();
    };
    let gmtoffset = result.meta.gmtoffset;

    timestamps
        .into_iter()
        .zip(quote.close)
        .filter_map(|(ts, close)| {
            // Yahoo's own JSON often carries float32-origin noise (e.g. a real close of
            // 315.32 arrives as 315.3200073242188), so round to a sane display/storage
            // precision rather than preserving every spurious digit via `from_f64`'s
            // shortest-round-trip conversion (fine for Frankfurter's already-clean rates,
            // not for this upstream).
            let close = Decimal::from_f64(close?)?.round_dp(4);
            let local = DateTime::from_timestamp(ts + gmtoffset, 0)?;
            Some(StockPriceQuote {
                as_of: local.date_naive(),
                close,
                currency_code: result.meta.currency.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_exchange_suffixes() {
        assert_eq!(symbol_for("aapl", None), "AAPL");
        assert_eq!(symbol_for("mel", Some("NZX")), "MEL.NZ");
        assert_eq!(symbol_for("bhp", Some("asx")), "BHP.AX");
        assert_eq!(symbol_for("aapl", Some("NASDAQ")), "AAPL");
    }

    #[test]
    fn parses_a_typical_chart_response() {
        let body: ChartResponse = serde_json::from_str(
            r#"{"chart":{"result":[{
                "meta":{"currency":"NZD","gmtoffset":43200},
                "timestamp":[1767560400,1767646800,1767733200],
                "indicators":{"quote":[{"close":[5.60,5.55,null]}]}
            }]}}"#,
        )
        .unwrap();
        let result = body.chart.result.unwrap().into_iter().next().unwrap();
        let quotes = parse_quotes(result);

        // The null close (a non-trading day inside the range) is dropped.
        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[0].close.to_string(), "5.6");
        assert_eq!(quotes[0].currency_code, "NZD");
        assert_eq!(quotes[1].close.to_string(), "5.55");
    }

    #[test]
    fn missing_chart_result_is_empty() {
        let body: ChartResponse = serde_json::from_str(r#"{"chart":{"result":null}}"#).unwrap();
        assert!(body.chart.result.is_none());
    }
}
