//! Concrete external-provider adapters. Each implements a port trait defined in
//! `sure_app::ports` — [`TransactionProvider`] (CSV, Akahu), `StockPriceProvider`
//! (Yahoo Finance), `ExchangeRateProvider` (Frankfurter), and `ImportAdapter` over each
//! manual-upload export parser ([`sharesies::parse_export`], [`myir::parse_export`],
//! [`asb::parse_upload`], [`csv::parse_rows`]). Two registries implement the two lookup ports:
//! [`Registry`] enumerates the transaction providers for the sync service, and
//! [`import::ImportRegistry`] decides which adapter an uploaded blob belongs to. The
//! composition root (`sure-server`) builds both and injects them.
//!
//! **Nothing here reads configuration, and no adapter can construct its own endpoint.** Each
//! network-facing adapter takes an [`Endpoint`] — its base URL, already checked to be
//! `https://` or a loopback proxy — and Akahu additionally takes [`AkahuCredentials`] (or the
//! [`MissingToken`] explaining why there are none). Every module's `DEFAULT_BASE_URL` is `pub`
//! and is the *only* production default; `sure-server`'s `Config::from_env` is the one thing
//! that reads it, parses it, and hands the result down. No network-facing type here has an
//! argument-free constructor — there is no `FrankfurterProvider::new()`, no
//! `YahooFinanceProvider::new()`, no `Registry::default()` — because that is the call a future
//! caller makes by reflex, and it would silently aim a test at the live API past the
//! configuration that was supposed to decide where it points. Before this, three base URLs were
//! private consts and `AkahuProvider` read the environment on every request, so the only
//! testable part of any adapter was its parsing.
//!
//! This crate defines no ports of its own: it depends on `sure-app` to see them, so
//! `sure-app` never depends back on it. To add a bank/broker integration, implement the
//! relevant port trait and (for a transaction source) add it to [`Registry::new`].

// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `114_269_63` == $114,269.63); clippy's grouping lint fights that convention.
// Same allow, same reason, as `sure-dal`/`sure-app`/`sure-api`.
#![allow(clippy::inconsistent_digit_grouping)]

use sure_app::ports::{ProviderRegistry, TransactionProvider};
use sure_core::ProviderKind;

pub mod akahu;
pub mod asb;
pub mod csv;
pub mod frankfurter;
mod http;
pub mod import;
pub mod myir;
pub mod sharesies;
pub mod yahoo_finance;
pub mod zipfile;

pub use akahu::{AkahuCredentials, AkahuProvider, MissingToken};
pub use frankfurter::FrankfurterProvider;
// `http` is private — it is this crate's own outbound-client plumbing — but `Endpoint` is half
// of every adapter's constructor, so the composition root has to be able to name it.
pub use http::Endpoint;
pub use yahoo_finance::YahooFinanceProvider;

/// The set of transaction-provider implementations the server knows about. Implements
/// [`sure_app::ports::ProviderRegistry`] so the application core can enumerate and select
/// a provider by kind without naming a concrete adapter.
pub struct Registry {
    providers: Vec<Box<dyn TransactionProvider>>,
}

impl Registry {
    /// `akahu` arrives built, because only the composition root knows where it points and
    /// whether it has credentials. `CsvProvider` is still constructed here: it is a pure
    /// parser of an uploaded body, with no endpoint and nothing to configure.
    ///
    /// This is also why there is no `Default` impl any more. One that called `from_env()` and
    /// [`akahu::DEFAULT_BASE_URL`] itself would put a second reader of the environment in the
    /// crate that just stopped having one — and would be the constructor every future caller
    /// reached for by accident, quietly making a test talk to the real Akahu.
    pub fn new(akahu: AkahuProvider) -> Self {
        Self {
            providers: vec![Box::new(csv::CsvProvider), Box::new(akahu)],
        }
    }
}

impl ProviderRegistry for Registry {
    fn get(&self, kind: &str) -> Option<&dyn TransactionProvider> {
        self.providers
            .iter()
            .find(|p| p.kind() == kind)
            .map(|b| b.as_ref())
    }

    fn kinds(&self) -> Vec<ProviderKind> {
        self.providers
            .iter()
            .map(|p| ProviderKind {
                kind: p.kind().to_string(),
                description: p.description().to_string(),
                accepts_payload: p.accepts_payload(),
                supports_account_discovery: p.supports_account_discovery(),
            })
            .collect()
    }
}
