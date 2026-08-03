//! Concrete external-provider adapters. Each implements a port trait defined in
//! `sure_app::ports` — [`TransactionProvider`] (CSV, Akahu), `StockPriceProvider`
//! (Yahoo Finance), `ExchangeRateProvider` (Frankfurter) — plus the manual-upload export
//! parsers, which implement no port because there is one implementation of each ever:
//! [`sharesies::parse_export`], [`myir::parse_export`], [`asb::parse_export`].
//! The [`Registry`] implements
//! [`sure_app::ports::ProviderRegistry`], enumerating the transaction providers for the
//! sync service; the composition root (`sure-server`) builds it and injects it.
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
pub mod myir;
pub mod sharesies;
pub mod yahoo_finance;
pub mod zipfile;

pub use akahu::AkahuProvider;
pub use frankfurter::FrankfurterProvider;
pub use yahoo_finance::YahooFinanceProvider;

/// The set of transaction-provider implementations the server knows about. Implements
/// [`sure_app::ports::ProviderRegistry`] so the application core can enumerate and select
/// a provider by kind without naming a concrete adapter.
pub struct Registry {
    providers: Vec<Box<dyn TransactionProvider>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            providers: vec![Box::new(csv::CsvProvider), Box::new(akahu::AkahuProvider)],
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

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
