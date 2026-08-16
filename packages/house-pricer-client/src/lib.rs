//! # house-pricer-client
//!
//! An unofficial Rust client for [House Pricer](https://www.housepricer.co.nz) — a free,
//! keyless automated valuation model for **Christchurch, New Zealand**, built on Christchurch
//! City Council sale/valuation records and LINZ property data.
//!
//! ## Why this is its own crate
//!
//! It owns exactly one thing: **what House Pricer's wire format is**. The endpoint is
//! undocumented and can change without notice, so the question "did the upstream rename a
//! field?" should have one place to look and one place to fix — [`models`] — and fixing it
//! should not recompile anything that knows what a valuation, an account or a minor unit is.
//! That is why this crate depends on nothing in the Sure workspace: it cannot accidentally grow
//! a domain concept, because it cannot name one.
//!
//! The division of labour with `sure_providers::house_pricer`, which is its only caller:
//!
//! | here | there |
//! |---|---|
//! | the URL shape, the query encoding, the status codes | which HTTP client policy applies |
//! | the JSON contract and its field names | which of the two models to record |
//! | dollars as the wire quotes them (`f64`) | dollars → minor units, and refusing one that won't fit |
//! | "no match" as [`HousePricerError::NotFound`] | what "no match" means to a caller |
//!
//! ## Personal data
//!
//! A `/match` response is **not market data**. It is a dossier on one dwelling — street address,
//! GPS centroid, title boundary polygon, legal description, land and improvement values — for
//! wherever the person running the caller lives. This crate treats it accordingly: it
//! deserialises four fields and ignores the rest, it never logs a response, and a body kept for
//! debugging is redacted in `Debug` and absent from `Display` (see
//! [`error::ResponseBody`]). In Sure, `scripts/pii-scan.mjs` additionally refuses a
//! `house_pricer` recording by path *and* by content, so no fixture here is ever a capture.
//!
//! ## Usage
//!
//! ```no_run
//! use house_pricer_client::{HousePricerClient, HousePricerError, DEFAULT_BASE_URL};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // The `reqwest::Client` is the caller's: its timeouts and TLS policy are the caller's too.
//! let client = HousePricerClient::new(reqwest::Client::new(), DEFAULT_BASE_URL);
//!
//! match client.match_address("1 Invented Street, Christchurch").await {
//!     Ok(property) => println!(
//!         "{}: {:?}",
//!         property.street_address, property.gross_sale_price_predicted_model_a
//!     ),
//!     // The ordinary answer for an address outside Christchurch, or one with a typo.
//!     Err(HousePricerError::NotFound) => println!("no match"),
//!     Err(e) => return Err(e.into()),
//! }
//! # Ok(())
//! # }
//! ```

pub mod client;
pub mod error;
pub mod models;

pub use client::{DEFAULT_BASE_URL, DEFAULT_MAX_RESPONSE_BYTES, HousePricerClient};
pub use error::{HousePricerError, ResponseBody};
pub use models::PropertyMatch;
