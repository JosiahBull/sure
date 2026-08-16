//! Reference [`StockPriceProvider`] backed by Yahoo Finance's unofficial "chart" JSON
//! endpoint (`query1.finance.yahoo.com/v8/finance/chart/{symbol}`) — free, keyless,
//! covers both NZX and US listings (the same endpoint the popular `yfinance` Python
//! library wraps). It's undocumented and could change without notice, same caveat as
//! depending on Frankfurter for exchange rates.
//!
//! **The wire format is not here.** `yahoo-finance-client` owns the URL shape, the status codes,
//! the JSON contract and the flattening of its two parallel arrays; this file owns what Sure
//! does with the answer. The split is what makes an undocumented endpoint tolerable: a field
//! rename is a one-line change in that crate, which cannot name a holding or a stored price, so
//! nothing in this workspace's domain logic recompiles its idea of anything. What stays here is
//! what is Sure's and not the upstream's:
//!
//! * **[`symbol_for`]** — that an NZX listing is `.NZ` to Yahoo is a mapping from *Sure's*
//!   exchange vocabulary, so the client is handed a symbol and never a ticker;
//! * **the window**, padded by a day at each end and converted to epoch seconds, which is
//!   calendar work about trading days;
//! * **[`parse_quotes`]** — epoch second plus the exchange's `gmtoffset` into the day a close is
//!   filed under, and `f64` into a `Decimal` at a scale worth storing;
//! * the client policy (`Endpoint`, the shared bounded `reqwest::Client`, one body ceiling for
//!   every adapter), the [`Throttle`], and the memo below — all of them about this *process*'s
//!   outbound budget rather than about this endpoint.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use yahoo_finance_client::{Chart, YahooFinanceClient, YahooFinanceError};

use sure_app::ports::{StockPriceProvider, StockPriceQuote};

use crate::http::{Endpoint, Pacing, Throttle};

/// The real endpoint. `pub` because the composition root owns the decision of where this
/// provider points (it is the only place configuration is read) and needs a default to fall
/// back to.
///
/// Re-exported from the client rather than restated, so the two cannot drift: this const and the
/// one the client would use by default are the same string by construction.
pub const DEFAULT_BASE_URL: &str = yahoo_finance_client::DEFAULT_BASE_URL;

/// What this adapter calls the upstream in an error a person will read.
///
/// One const because two messages carry it — the stand-down the throttle arms, and the refusal
/// the *next* call gets — and they land in `provider_syncs.detail` and in a 422 body, where two
/// spellings of the same host would read as two different problems.
const HOST: &str = "Yahoo Finance";

pub struct YahooFinanceProvider {
    client: YahooFinanceClient,
    /// Yahoo publishes no rate limit for this endpoint, but hammering it risks a temporary IP
    /// block: [`Pacing::min_request_interval`] spaces consecutive requests from this instance
    /// apart however many callers share it, and a `429` arms a stand-down window through
    /// [`Throttle::note_refusal`].
    throttle: Throttle,
    /// Symbols Yahoo has answered `404` for, and when that answer stops being reused.
    ///
    /// The loop this closes: a delisted or mistyped ticker 404s → this adapter reports it as
    /// "no prices" (an empty vec, see [`StockPriceProvider::fetch_daily_prices`]) → nothing
    /// lands in the `stock_prices` table → `sure_app::stock_prices::price_at` misses again next
    /// time and asks again. One dead symbol in an account therefore reached Yahoo on *every*
    /// render of the page showing it, forever; the throttle spaced those requests out but did
    /// not remove them.
    ///
    /// Populated **only** from a `404` ([`YahooFinanceError::UnknownSymbol`]), never from a
    /// `200` carrying no closes. A 404 is Yahoo saying the symbol does not exist, which is true
    /// of every date range; an empty 200 says only that this *window* was empty, and memoising
    /// that would suppress a later valid request for a range the symbol does cover (an IPO asked
    /// about before it listed). The client keeps those two apart as two error variants precisely
    /// so this distinction survives the trip.
    ///
    /// A `std::sync::Mutex`, not tokio's: every critical section is one map lookup with no
    /// `.await` in it.
    absent: Mutex<HashMap<String, Instant>>,
}

impl YahooFinanceProvider {
    /// The only constructor, and deliberately so: there is no argument-free `new()` that
    /// reaches for [`DEFAULT_BASE_URL`] itself. That const is the composition root's fallback
    /// (`Config::from_env` parses it into an [`Endpoint`]), and a second constructor holding
    /// the same URL would be the one a future caller reached for by reflex — pointing an
    /// adapter at the live API from inside a test, past the configuration that was supposed to
    /// decide it. The same reasoning removed `Registry`'s `Default`; see `lib.rs`.
    ///
    /// In practice the endpoint is either that parsed default or the record/replay proxy a
    /// test binds on loopback, which is the only way the fetch path below is exercisable at all
    /// without reaching an undocumented endpoint that could change without notice.
    ///
    /// The throttle is per-instance, and [`Pacing`] is injected for the same reason
    /// [`Endpoint`] is: not getting this app's IP blocked is a property of whichever upstream
    /// the instance talks to, and how hard to try is the composition root's decision to make.
    /// A test that pays the interval between two requests to its own proxy is paying it in
    /// exactly the place production does, which is the point of not special-casing it —
    /// `Pacing::unpaced()` is there for the tests that are about something else.
    ///
    /// The `reqwest::Client` is built here and handed to the wire crate, exactly as
    /// `AkahuProvider` does with `akahu-client`: whether a plaintext request is refused, whether
    /// a redirect is followed, and what the timeouts are, are properties of the client that
    /// [`Endpoint`] decides. The body ceiling comes from the same place, so this process has one
    /// answer to "how much of a response may we buffer?" rather than one per upstream.
    pub fn with_endpoint(endpoint: Endpoint, pacing: Pacing) -> Self {
        let client = YahooFinanceClient::new(crate::http::client(&endpoint), endpoint.url())
            .with_max_response_bytes(crate::http::MAX_BODY_BYTES);
        Self {
            client,
            throttle: Throttle::new(pacing),
            absent: Mutex::new(HashMap::new()),
        }
    }

    /// Whether Yahoo's `404` for `symbol` is still worth believing.
    ///
    /// Sweeps the expired entries while it is in there, which is all the bounding this map
    /// needs: its keys are the distinct symbols one household holds, so it is tens of entries
    /// even before anything expires — unlike `sure-api`'s per-IP bucket map, which faces the
    /// open internet and needs a real sweep threshold and ceiling.
    fn is_known_absent(&self, symbol: &str) -> bool {
        let now = Instant::now();
        let mut absent = self.absent.lock().unwrap_or_else(PoisonError::into_inner);
        absent.retain(|_, expires| *expires > now);
        absent.contains_key(symbol)
    }

    fn remember_absent(&self, symbol: &str) {
        let expires = Instant::now() + self.throttle.pacing().discovery_ttl;
        self.absent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(symbol.to_string(), expires);
    }
}

/// Best-effort ticker suffix for exchanges this app actually deals in (an NZ-based
/// household's likely NZX/ASX holdings alongside US ones); anything else, including a
/// bare US ticker, needs no suffix.
///
/// Sure's vocabulary on the left, Yahoo's on the right, which is why this is here and not in
/// `yahoo-finance-client`: the crate that knows the wire is handed the symbol this produces and
/// has no business knowing what an `exchange` column contains.
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

        // Asked and answered, inside the TTL: Yahoo has already said this symbol does not
        // exist, and saying it again costs a request against an endpoint that answers a burst
        // with an IP block. Same empty vec the 404 below returns, so no caller can tell the
        // difference between the memo and the request it replaces.
        if self.is_known_absent(&symbol) {
            tracing::debug!(%symbol, "no price data (remembered from an earlier 404)");
            return Ok(Vec::new());
        }

        // Pad the requested range by a day on each side: Yahoo buckets timestamps by
        // the exchange's local trading day, so a UTC-midnight boundary can otherwise
        // clip the first or last day depending on which side of UTC the exchange sits.
        let period1 = to_unix(from - ChronoDuration::days(1));
        let period2 = to_unix(to + ChronoDuration::days(1));

        self.throttle.acquire(HOST).await?;
        match self.client.chart(&symbol, period1, period2).await {
            Ok(chart) => Ok(parse_quotes(chart)),
            // A delisted stock (e.g. RBD after its 2019 takeover) or an expired instrument
            // (e.g. a lapsed "…RG" rights issue) comes back as 404. That's legitimately "no
            // prices available", not a failure — an account's historical holdings routinely
            // include such symbols — so return empty and let the caller leave those positions
            // unpriced, rather than surfacing a hard error that the backfill/poller then logs as
            // a warning for every delisted ticker. Distinct from `NoChartData`, which is a 200
            // that said nothing and *is* an error: see the `absent` field.
            Err(YahooFinanceError::UnknownSymbol { .. }) => {
                tracing::debug!(%symbol, "no price data (delisted, expired, or unknown symbol)");
                self.remember_absent(&symbol);
                Ok(Vec::new())
            }
            // The one outcome that changes what this process does next rather than only what it
            // reports: a rate limit arms a stand-down window, so the *next* caller is refused
            // before a request goes out instead of adding to the burst that caused this. It is
            // the client that recognised the refusal, because it owns the response the
            // `Retry-After` arrived on; the window it turns into is this crate's policy.
            Err(YahooFinanceError::RateLimited {
                status,
                retry_after,
            }) => Err(self.throttle.note_refusal(HOST, status, retry_after).await),
            // CLAUDE.md rule 2's escape hatch: `YahooFinanceError` is `#[non_exhaustive]`, so a
            // catch-all is the only option — and it is the right answer anyway, because every
            // remaining variant means the same thing to this caller (the prices could not be
            // fetched) and differs only in the message. The two above are the ones that change
            // behaviour, and they are named.
            Err(other) => Err(anyhow::Error::new(other)),
        }
    }
}

/// Turn a chart's candles into dated quotes.
///
/// The half that is genuinely Sure's. The client has already zipped the wire's parallel
/// `timestamp`/`close` arrays and dropped the days that did not trade; what is left is the pair
/// of conversions that need to know what a *stored price* is — which calendar day a bar belongs
/// to, and at what scale its close is worth keeping.
fn parse_quotes(chart: Chart) -> Vec<StockPriceQuote> {
    chart
        .candles
        .into_iter()
        .filter_map(|candle| {
            // Yahoo's own JSON often carries float32-origin noise (e.g. a real close of
            // 315.32 arrives as 315.3200073242188), so round to a sane display/storage
            // precision rather than preserving every spurious digit via `from_f64`'s
            // shortest-round-trip conversion (fine for Frankfurter's already-clean rates,
            // not for this upstream).
            let close = Decimal::from_f64(candle.close)?.round_dp(4);
            // The bar is stamped at the exchange's local market open, in UTC — so an NZX bar for
            // Monday sits at 21:00Z on the Sunday. Adding the offset before taking the calendar
            // date is what files each close under the day it actually traded on; getting it
            // wrong lands a whole week of closes one day early.
            let local = DateTime::from_timestamp(candle.timestamp + chart.gmtoffset, 0)?;
            Some(StockPriceQuote {
                as_of: local.date_naive(),
                close,
                currency_code: chart.currency.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yahoo_finance_client::Candle;

    #[test]
    fn maps_known_exchange_suffixes() {
        assert_eq!(symbol_for("aapl", None), "AAPL");
        assert_eq!(symbol_for("mel", Some("NZX")), "MEL.NZ");
        assert_eq!(symbol_for("bhp", Some("asx")), "BHP.AX");
        assert_eq!(symbol_for("aapl", Some("NASDAQ")), "AAPL");
    }

    /// Built as a value rather than parsed from JSON, which is the visible dividend of the
    /// split: what this test is about is the `gmtoffset` arithmetic and the rounding, and it no
    /// longer restates a wire format — least of all this one, whose two parallel arrays it is
    /// not testing. Whether the JSON *parses*, and whether the zip lines the arrays up, is
    /// `yahoo-finance-client`'s own test, next to the structs that would have to change.
    ///
    /// The timestamps are NZX market opens (10:00 local, +13h in NZDT), so each one is 21:00Z on
    /// the *previous* calendar day — which is exactly what the offset has to undo.
    #[test]
    fn files_each_close_under_the_exchanges_local_trading_day() {
        let quotes = parse_quotes(Chart::new(
            "NZD",
            46_800,
            vec![
                Candle::new(1_772_398_800, 5.6),
                Candle::new(1_772_485_200, 5.55),
            ],
        ));

        assert_eq!(quotes.len(), 2);
        assert_eq!(quotes[0].as_of.to_string(), "2026-03-02");
        assert_eq!(quotes[0].close.to_string(), "5.6");
        assert_eq!(quotes[0].currency_code, "NZD");
        assert_eq!(quotes[1].as_of.to_string(), "2026-03-03");
        assert_eq!(quotes[1].close.to_string(), "5.55");
    }

    /// The rounding, on the noise the real feed carries: a close of 5.63 arrives from Yahoo as
    /// `5.630000114440918`, and storing that verbatim would put a float32 artefact in the
    /// database and on the page.
    #[test]
    fn rounds_away_the_float32_noise_the_feed_carries() {
        let quotes = parse_quotes(Chart::new(
            "USD",
            -14_400,
            vec![Candle::new(1_772_398_800, 5.630000114440918)],
        ));
        assert_eq!(quotes[0].close.normalize().to_string(), "5.63");
    }

    /// A chart with no bars in it is no quotes — not an error, and not a panic. The client
    /// answers this way for a window with no trading days in it; a fortnight over Christmas is
    /// the ordinary case.
    #[test]
    fn a_chart_with_no_candles_is_no_quotes() {
        assert!(parse_quotes(Chart::new("NZD", 46_800, Vec::new())).is_empty());
    }

    /// The one const that could silently drift now that the URL lives in another crate.
    #[test]
    fn the_default_endpoint_is_the_clients_own() {
        assert_eq!(DEFAULT_BASE_URL, yahoo_finance_client::DEFAULT_BASE_URL);
    }
}
