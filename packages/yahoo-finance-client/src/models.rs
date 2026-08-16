use serde::Deserialize;

/// One symbol's daily bars, flattened out of the wire's parallel arrays.
///
/// **This is not the shape Yahoo sends.** The response carries a `timestamp` array and, beside
/// it under `indicators.quote[0]`, a `close` array of the same length, positionally aligned —
/// so a bar is the *n*th element of each, and a non-trading day inside the requested range is a
/// `null` in the second one. That is wire weirdness rather than anything a caller should have to
/// know, and holding the two apart is how an off-by-one silently files every close under the
/// wrong day, so [`Chart`] is the zipped form and the arrays never leave this crate.
///
/// What is deliberately *not* done here is anything calendar-shaped. [`Self::gmtoffset`] and a
/// bar's epoch second are handed over as the wire's own integers, because turning them into a
/// trading day needs to know what a trading day is for — see the crate docs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Chart {
    /// The currency the closes are quoted in, e.g. `NZD`. Read from `meta.currency` rather than
    /// assumed from the symbol's suffix: a listing's currency is the exchange's business.
    pub currency: String,

    /// Seconds offset from UTC for the exchange this symbol trades on, as `meta.gmtoffset`.
    ///
    /// Load-bearing, and the reason it is on the chart rather than dropped: Yahoo stamps a daily
    /// bar at the exchange's *local* market open, so an NZX bar for Monday sits at 21:00Z on the
    /// Sunday. Adding this before taking the calendar date is what files a close under the day
    /// it was actually traded on. Spelled as the wire spells it, so a reader can find it in a
    /// raw response.
    pub gmtoffset: i64,

    /// One entry per day that actually traded, in the order the wire listed them.
    ///
    /// Days the upstream had no close for are already gone — see [`Candle`].
    pub candles: Vec<Candle>,
}

/// One daily bar: when it closed, and at what.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Candle {
    /// Epoch seconds at the exchange's local market open for that trading day, in UTC.
    ///
    /// Not a date: it needs [`Chart::gmtoffset`] added before a calendar day can be read off it,
    /// and that is the caller's step.
    pub timestamp: i64,

    /// The closing price, in the wire's own unit and scale.
    ///
    /// Left as the `f64` the JSON carries rather than converted to a decimal here: this crate's
    /// job is to say faithfully what the upstream said. Note that Yahoo's own JSON carries
    /// float32-origin noise — a real close of `5.63` arrives as `5.630000114440918` — which the
    /// caller rounds away at a scale it has decided on, because "how precisely do we store a
    /// price?" is not a question about the wire.
    pub close: f64,
}

impl Chart {
    /// Build a chart directly, for a caller testing what it does with one.
    ///
    /// The alternative is a JSON literal in a test that is not about JSON, which is exactly what
    /// this crate exists to stop leaking outwards: `sure_providers::yahoo_finance`'s mapping
    /// tests are about the `gmtoffset` arithmetic and the rounding, and they should not have to
    /// restate a wire format — least of all this one, whose two parallel arrays are precisely
    /// what they are not testing. Needed because the struct is `#[non_exhaustive]`, which is
    /// right for a wire-shaped type that will gain fields but blocks a struct literal from
    /// another crate.
    pub fn new(currency: impl Into<String>, gmtoffset: i64, candles: Vec<Candle>) -> Self {
        Self {
            currency: currency.into(),
            gmtoffset,
            candles,
        }
    }
}

impl Candle {
    /// Build one bar. See [`Chart::new`] for why this exists.
    pub fn new(timestamp: i64, close: f64) -> Self {
        Self { timestamp, close }
    }
}

/// The document as it arrives: `{"chart":{"result":[…],"error":null}}`.
#[derive(Debug, Deserialize)]
pub(crate) struct ChartResponse {
    chart: ChartEnvelope,
}

/// The `chart` object. Named for its role rather than for its key, so the flattened [`Chart`] —
/// the type callers actually see — gets the good name.
#[derive(Debug, Deserialize)]
struct ChartEnvelope {
    /// `null` for a symbol Yahoo answered `200` for but has nothing to say about; an array of
    /// exactly one result otherwise. Never observed with two, and the second would be ignored.
    result: Option<Vec<ChartResult>>,
}

#[derive(Debug, Deserialize)]
struct ChartResult {
    meta: ChartMeta,
    /// Absent — not empty — when the requested window contains no trading days at all.
    timestamp: Option<Vec<i64>>,
    indicators: ChartIndicators,
}

#[derive(Debug, Deserialize)]
struct ChartMeta {
    currency: String,
    gmtoffset: i64,
}

#[derive(Debug, Deserialize)]
struct ChartIndicators {
    /// One entry per quote series. Only the first is read; the endpoint has never sent two.
    quote: Vec<ChartQuote>,
}

#[derive(Debug, Deserialize)]
struct ChartQuote {
    /// Positionally aligned with `timestamp`, `null` on a day that did not trade.
    close: Vec<Option<f64>>,
}

impl ChartResponse {
    /// Zip the parallel arrays into a [`Chart`], or `None` if the document carried no result.
    ///
    /// `None` and an empty `candles` are different answers and the caller treats them
    /// differently: no *result* means the upstream answered in a shape we cannot read, while a
    /// result with no bars means this particular window was empty — true of a fortnight over
    /// Christmas, and not a reason to stop asking about the symbol.
    ///
    /// A `null` close is dropped rather than carried as an `Option`, because there is nothing a
    /// caller can do with "there was a day, but no price on it" that it cannot do with the day
    /// being absent — and every caller would otherwise write this same filter.
    pub(crate) fn into_chart(self) -> Option<Chart> {
        let mut results = self.chart.result?;
        if results.is_empty() {
            return None;
        }
        let ChartResult {
            meta,
            timestamp,
            indicators,
        } = results.remove(0);

        Some(Chart::new(
            meta.currency,
            meta.gmtoffset,
            zip_candles(timestamp, indicators),
        ))
    }
}

/// The zip itself: one candle per position the two arrays agree on and the close is not `null`.
///
/// Either array being absent is "this window was empty", which is the caller's *no prices* and
/// not an error — the symbol itself is fine, and a fortnight over Christmas legitimately looks
/// like this.
fn zip_candles(timestamps: Option<Vec<i64>>, indicators: ChartIndicators) -> Vec<Candle> {
    let Some(timestamps) = timestamps else {
        return Vec::new();
    };
    let Some(quote) = indicators.quote.into_iter().next() else {
        return Vec::new();
    };
    timestamps
        .into_iter()
        .zip(quote.close)
        .filter_map(|(timestamp, close)| Some(Candle::new(timestamp, close?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response in the shape the endpoint actually sends, trimmed to what is read plus enough
    /// of what is not to prove the tolerance.
    ///
    /// Prices and tickers are public market data, so CLAUDE.md rule 3 does not reach them.
    const CHART: &str = r#"{"chart":{"result":[{
        "meta":{"currency":"NZD","symbol":"MEL.NZ","exchangeName":"NZE","gmtoffset":46800},
        "timestamp":[1772398800,1772485200,1772571600],
        "indicators":{"quote":[{"close":[5.6,5.630000114440918,null],"volume":[1834021,2201884,1596330]}]}
    }],"error":null}}"#;

    fn parse(json: &str) -> Option<Chart> {
        serde_json::from_str::<ChartResponse>(json)
            .expect("the fixture is a valid chart document")
            .into_chart()
    }

    /// The flattening, which is the whole reason this type is not the wire's.
    #[test]
    fn zips_the_parallel_arrays_into_candles() {
        let chart = parse(CHART).expect("a result is present");

        assert_eq!(chart.currency, "NZD");
        assert_eq!(chart.gmtoffset, 46800);
        // Three timestamps, one null close: the non-trading day is dropped, and — the part an
        // off-by-one would break — the two survivors keep the timestamps they were aligned with.
        assert_eq!(
            chart.candles,
            [
                Candle::new(1_772_398_800, 5.6),
                Candle::new(1_772_485_200, 5.630000114440918),
            ]
        );
    }

    /// The float32-origin noise is passed through untouched. Rounding it away is a decision
    /// about how precisely a price is stored, which is the caller's to make — this only has to
    /// not hide the problem.
    #[test]
    fn a_close_arrives_exactly_as_the_wire_quoted_it() {
        let chart = parse(CHART).expect("a result is present");
        assert_eq!(chart.candles[1].close, 5.630000114440918);
    }

    /// `result: null` is the 200-with-nothing case, and it has to be distinguishable from a
    /// window that simply had no trading days in it.
    #[test]
    fn a_missing_result_is_no_chart_at_all() {
        assert!(parse(r#"{"chart":{"result":null,"error":null}}"#).is_none());
        // An empty array says the same thing; the endpoint has been seen to send both.
        assert!(parse(r#"{"chart":{"result":[],"error":null}}"#).is_none());
    }

    /// …and the other side of that distinction: a result with no `timestamp` is a chart with no
    /// bars, not a missing chart. A caller that conflated the two would stop asking about a
    /// symbol because it happened to request a fortnight over Christmas.
    #[test]
    fn a_window_with_no_trading_days_is_an_empty_chart_not_a_missing_one() {
        let chart = parse(
            r#"{"chart":{"result":[{"meta":{"currency":"USD","gmtoffset":-14400},
               "indicators":{"quote":[{"close":[]}]}}],"error":null}}"#,
        )
        .expect("the result itself is present");
        assert_eq!(chart.currency, "USD");
        assert!(chart.candles.is_empty());
    }

    /// The property that keeps this crate from breaking on somebody else's deploy: the real
    /// document carries roughly forty `meta` fields, a `currentTradingPeriod` object and
    /// pre/post-market flags, none of which are read.
    #[test]
    fn unknown_fields_are_ignored_rather_than_fatal() {
        let chart = parse(
            r#"{"chart":{"result":[{
                "meta":{"currency":"USD","gmtoffset":-14400,"instrumentType":"ETF",
                        "currentTradingPeriod":{"regular":{"start":0}},
                        "someFieldAddedNextTuesday":{"nested":true}},
                "timestamp":[1772398800],
                "indicators":{"quote":[{"close":[512.25],"open":[511.0]}]},
                "events":{"dividends":{}}
            }],"error":null}}"#,
        )
        .expect("an unrecognised field must not fail the chart");
        assert_eq!(chart.candles, [Candle::new(1_772_398_800, 512.25)]);
    }

    /// The constructor the caller's mapping tests use instead of a JSON literal.
    #[test]
    fn a_chart_can_be_built_without_a_wire_document() {
        let chart = Chart::new("NZD", 46800, vec![Candle::new(1_772_398_800, 5.6)]);
        assert_eq!(chart.gmtoffset, 46800);
        assert_eq!(chart.candles[0].close, 5.6);
    }
}
