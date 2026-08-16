//! # frankfurter-client
//!
//! An unofficial Rust client for [Frankfurter](https://frankfurter.dev) — a free, keyless
//! exchange-rate API serving European Central Bank reference rates. No credentials and no
//! signup, which is what makes it a reasonable zero-config default for an app that has to
//! convert currencies before anybody has configured anything.
//!
//! ## Why this is its own crate
//!
//! It owns exactly one thing: **what Frankfurter's wire format is**. The rate table is a public
//! document, but the endpoint is still somebody else's and can change without notice, so the
//! question "did the upstream rename a field?" should have one place to look and one place to
//! fix — [`models`] — and fixing it should not recompile anything that knows what an account or
//! a minor unit is. That is why this crate depends on nothing in the Sure workspace: it cannot
//! accidentally grow a domain concept, because it cannot name one.
//!
//! The division of labour with `sure_providers::frankfurter`, which is its only caller:
//!
//! | here | there |
//! |---|---|
//! | the URL shape and the query encoding | which HTTP client policy applies |
//! | the JSON contract and its field names | which base currency to ask for |
//! | rates as the wire quotes them (`f64`) | `f64` → `Decimal`, and what a quote *is* |
//! | "refused on volume", with its `Retry-After` parsed | how long to stand down, and refusing the next call |
//!
//! ## Rate limiting
//!
//! A refusal-on-volume is [`FrankfurterError::RateLimited`] rather than a generic status,
//! because the two mean opposite things to a caller: retrying an ordinary `4xx` shortly is
//! harmless, and retrying a rate limit shortly is what turns a throttle into a block. This
//! crate goes as far as HTTP goes — it recognises the statuses that mean it and parses the
//! `Retry-After` that comes with them — and stops there. *How long to actually stand down*, and
//! whether to refuse the next call locally, is policy about the whole process rather than about
//! this endpoint, so it belongs to the caller (`sure_providers::http::Throttle`).
//!
//! ## Usage
//!
//! ```no_run
//! use frankfurter_client::{DEFAULT_BASE_URL, FrankfurterClient, FrankfurterError};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // The `reqwest::Client` is the caller's: its timeouts and TLS policy are the caller's too.
//! let client = FrankfurterClient::new(reqwest::Client::new(), DEFAULT_BASE_URL);
//!
//! match client.latest("NZD").await {
//!     Ok(table) => println!("{} rates as of {}", table.rates.len(), table.date),
//!     // The one outcome a caller must not treat as an ordinary failure: coming straight back
//!     // is what escalates it.
//!     Err(FrankfurterError::RateLimited { retry_after, .. }) => {
//!         println!("refused on volume; asked for {retry_after:?}");
//!     }
//!     Err(e) => return Err(e.into()),
//! }
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod error;
pub mod models;

pub use client::{DEFAULT_BASE_URL, DEFAULT_MAX_RESPONSE_BYTES, FrankfurterClient};
pub use error::FrankfurterError;
pub use models::LatestRates;
