//! # yahoo-finance-client
//!
//! An unofficial Rust client for Yahoo Finance's **undocumented** "chart" JSON endpoint
//! (`query1.finance.yahoo.com/v8/finance/chart/{symbol}`) — free, keyless, and covering both US
//! and non-US listings. It is the same endpoint the popular `yfinance` Python library wraps, and
//! it can change without notice.
//!
//! ## Why this is its own crate
//!
//! It owns exactly one thing: **what Yahoo's chart document is**. That document is undocumented,
//! peculiar (see [`models`] on the parallel arrays), and free to be renamed by somebody else's
//! deploy — so the question "did the upstream change shape?" should have one place to look and
//! one place to fix, and fixing it should not recompile anything that knows what a holding or a
//! stored price is. That is why this crate depends on nothing in the Sure workspace: it cannot
//! accidentally grow a domain concept, because it cannot name one.
//!
//! The division of labour with `sure_providers::yahoo_finance`, which is its only caller:
//!
//! | here | there |
//! |---|---|
//! | the URL shape, the `interval=1d` it always asks for | which HTTP client policy applies |
//! | the JSON contract, and flattening the parallel arrays | epoch second + `gmtoffset` → a trading day |
//! | closes as the wire quotes them (`f64`) | `f64` → `Decimal`, rounded to a storable scale |
//! | a Yahoo **symbol** (`MEL.NZ`) | ticker + exchange → that symbol |
//! | `404` as [`YahooFinanceError::UnknownSymbol`] | what "this symbol has no prices" means |
//!
//! The symbol split is the one worth stating twice. `MEL.NZ` is Yahoo's spelling; that an NZX
//! listing takes a `.NZ` suffix is a mapping from *Sure's* exchange vocabulary onto it, so it
//! stays in the adapter. This crate is handed a symbol and asks for it.
//!
//! ## The two "no data" answers, which are not the same answer
//!
//! [`YahooFinanceError::UnknownSymbol`] is a `404`: Yahoo does not know this symbol at all,
//! which is the ordinary fate of a delisted company or a lapsed rights issue, and is true of
//! every date range. [`YahooFinanceError::NoChartData`] is a `200` that carried no chart — the
//! upstream answered in a shape this crate cannot read. Keeping them apart is what lets a caller
//! report the first as "no prices" (a normal portfolio contains several such holdings) and the
//! second as a failure worth looking at, and what lets it *remember* the first without
//! memoising the second.
//!
//! ## Usage
//!
//! ```no_run
//! use yahoo_finance_client::{DEFAULT_BASE_URL, YahooFinanceClient, YahooFinanceError};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // The `reqwest::Client` is the caller's: its timeouts and TLS policy are the caller's too.
//! let client = YahooFinanceClient::new(reqwest::Client::new(), DEFAULT_BASE_URL);
//!
//! // Epoch seconds, because a trading-day window is the caller's arithmetic, not this crate's.
//! match client.chart("MEL.NZ", 1_772_323_200, 1_772_841_600).await {
//!     Ok(chart) => println!("{} bars in {}", chart.candles.len(), chart.currency),
//!     // Not a failure: an account's historical holdings routinely include symbols that have
//!     // stopped resolving.
//!     Err(YahooFinanceError::UnknownSymbol { symbol }) => println!("{symbol} has no prices"),
//!     Err(e) => return Err(e.into()),
//! }
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod error;
pub mod models;

pub use client::{DEFAULT_BASE_URL, DEFAULT_MAX_RESPONSE_BYTES, YahooFinanceClient};
pub use error::YahooFinanceError;
pub use models::{Candle, Chart};
