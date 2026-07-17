//! Generic external-provider interface. Implement [`TransactionProvider`] to connect
//! a new source (bank API, broker, CSV, ...). The [`Registry`] exposes the available
//! implementations; the sync route (see `routes::providers`) drives fetch + dedupe +
//! audit. A CSV importer ships as a reference implementation that needs no credentials.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

pub mod csv;
pub mod exchange_rate;
pub mod frankfurter;

pub use exchange_rate::{ExchangeRateProvider, ExchangeRateQuote};
pub use frankfurter::FrankfurterProvider;

/// A normalized transaction pulled from an external source.
#[derive(Debug, Clone)]
pub struct ProviderTransaction {
    /// Stable identifier from the source, used to dedupe on re-sync.
    pub external_id: String,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: Option<String>,
    pub description: String,
    pub merchant: Option<String>,
}

/// Everything a provider needs to perform a sync.
pub struct SyncContext<'a> {
    pub config: &'a Value,
    pub account_currency: &'a str,
    /// Optional inline payload supplied with the sync request (e.g. uploaded CSV).
    pub payload: Option<&'a str>,
}

/// The integration point. One method to fetch + normalize; everything else (dedupe,
/// persistence, audit) is handled generically by the sync route.
#[async_trait]
pub trait TransactionProvider: Send + Sync {
    /// Stable identifier used to select this provider (e.g. `"csv"`).
    fn kind(&self) -> &'static str;
    /// Human-facing description shown in the UI.
    fn description(&self) -> &'static str;
    /// Whether the provider expects an inline payload on sync (vs. fetching remotely).
    fn accepts_payload(&self) -> bool {
        false
    }
    /// Fetch and normalize transactions from the source.
    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>>;
}

/// Metadata about an available provider kind, surfaced via the API.
#[derive(Serialize, ToSchema)]
pub struct ProviderKind {
    pub kind: String,
    pub description: String,
    pub accepts_payload: bool,
}

/// The set of provider implementations the server knows about.
pub struct Registry {
    providers: Vec<Box<dyn TransactionProvider>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            providers: vec![Box::new(csv::CsvProvider)],
        }
    }

    pub fn get(&self, kind: &str) -> Option<&dyn TransactionProvider> {
        self.providers
            .iter()
            .find(|p| p.kind() == kind)
            .map(|b| b.as_ref())
    }

    pub fn kinds(&self) -> Vec<ProviderKind> {
        self.providers
            .iter()
            .map(|p| ProviderKind {
                kind: p.kind().to_string(),
                description: p.description().to_string(),
                accepts_payload: p.accepts_payload(),
            })
            .collect()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
