//! Generic external-provider interface. Implement [`TransactionProvider`] to connect
//! a new source (bank API, broker, CSV, ...). The [`Registry`] exposes the available
//! implementations; the sync route (see `routes::providers`) drives fetch + dedupe +
//! audit. A CSV importer ships as a reference implementation that needs no credentials.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use sure_core::AccountKind;
use utoipa::ToSchema;

pub mod akahu;
pub mod csv;
pub mod exchange_rate;
pub mod frankfurter;
pub mod sharesies;
pub mod stock_price;
pub mod yahoo_finance;

pub use akahu::AkahuProvider;
pub use exchange_rate::{ExchangeRateProvider, ExchangeRateQuote};
pub use frankfurter::FrankfurterProvider;
pub use stock_price::{StockPriceProvider, StockPriceQuote};
pub use yahoo_finance::YahooFinanceProvider;

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
    /// The source's own classification for this transaction (e.g. Akahu's NZFCC
    /// enrichment), if it has one — used to find-or-create a matching Sure category
    /// (and, for a newly-seen merchant, its default category) instead of leaving
    /// imported transactions uncategorized.
    pub category: Option<ProviderCategory>,
}

/// A merchant category as classified by the provider's own taxonomy.
#[derive(Debug, Clone)]
pub struct ProviderCategory {
    /// Specific category name (e.g. "Cafes and restaurants") — becomes a Sure category,
    /// nested under `group` when the source has one.
    pub name: String,
    /// Broader grouping (e.g. "Lifestyle"), if the source has one — becomes that
    /// category's parent.
    pub group: Option<String>,
    /// Flow direction hint (`"income"` | `"expense"` | `"transfer"`) applied when the
    /// category is first created. Most enrichment is spending, so `None` defaults to
    /// expense on the DAL side; a broker's dividend row sets `"income"`, an internal
    /// wallet ↔ bank movement sets `"transfer"` so it's excluded from spend/income reports.
    pub kind: Option<String>,
}

/// An upstream account surfaced by a provider that supports account discovery
/// (see [`TransactionProvider::list_accounts`]) — not yet linked to a local `Account`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderAccount {
    /// Stable identifier from the source; stored as `config.external_account_id` on the
    /// `providers` row once linked, and used to fetch that account's transactions.
    pub external_id: String,
    pub name: String,
    pub currency_code: String,
    /// The financial institution's display name (e.g. "ASB"), if the source reports one.
    pub institution: Option<String>,
    /// Best-effort suggestion for the local account's `kind`; the user confirms/edits it
    /// when linking, so an imperfect guess here isn't a correctness problem.
    pub kind_hint: AccountKind,
    pub balance_minor: i64,
    /// Whether the source can provide transaction history for this account (some upstream
    /// account types are balance-only).
    pub supports_transactions: bool,
}

/// Everything a provider needs to perform a sync. Cheap to copy (just references), so the
/// sync route can pass it to both [`TransactionProvider::fetch`] and
/// [`TransactionProvider::current_balance`].
#[derive(Debug, Clone, Copy)]
pub struct SyncContext<'a> {
    pub config: &'a Value,
    pub account_currency: &'a str,
    /// Optional inline payload supplied with the sync request (e.g. uploaded CSV).
    pub payload: Option<&'a str>,
    /// When this provider last completed a successful sync (RFC3339), if ever. Lets
    /// incremental providers avoid re-fetching full history on every run; providers that
    /// don't support incremental fetch (e.g. CSV) simply ignore it.
    pub last_synced_at: Option<&'a str>,
}

/// A point-in-time balance snapshot from an upstream source, plus whatever other
/// per-account facts happened to come back on the same fetch (a single-account refetch
/// is the natural place to also pick up slower-changing facts like a credit limit or an
/// institution name, rather than a separate round-trip for each).
#[derive(Debug, Clone)]
pub struct ProviderBalance {
    pub minor: i64,
    pub currency_code: String,
    /// Credit limit, in minor units, if the source reports one for this account (e.g. a
    /// credit card or revolving credit facility).
    pub limit_minor: Option<i64>,
    /// The financial institution's display name, if the source reports one and the local
    /// account doesn't already have one set (an existing value is never overwritten).
    pub institution: Option<String>,
    /// The original amount borrowed, in minor units, if the source reports one for this
    /// account (e.g. a mortgage or personal loan) — lets a paid-down percentage be shown.
    pub initial_principal_minor: Option<i64>,
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
    /// Whether this provider can enumerate linkable upstream accounts (see
    /// [`Self::list_accounts`]).
    fn supports_account_discovery(&self) -> bool {
        false
    }
    /// Fetch and normalize transactions from the source.
    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>>;
    /// List upstream accounts available to link, for providers that support discovery.
    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> {
        Err(anyhow::anyhow!(
            "{} does not support discovering accounts",
            self.kind()
        ))
    }
    /// The upstream's live current balance for the account this sync is for, if this
    /// provider can report one. Used to keep the account's value accurate even when the
    /// transaction history alone doesn't reach back to when the account was opened (a
    /// mortgage's full term, say) — see `sync_provider`'s post-fetch valuation upsert.
    /// Defaulting to `None` costs nothing for providers (like CSV) with no such concept.
    async fn current_balance(
        &self,
        _ctx: SyncContext<'_>,
    ) -> anyhow::Result<Option<ProviderBalance>> {
        Ok(None)
    }
}

/// Metadata about an available provider kind, surfaced via the API.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderKind {
    pub kind: String,
    pub description: String,
    pub accepts_payload: bool,
    pub supports_account_discovery: bool,
}

/// The set of provider implementations the server knows about.
pub struct Registry {
    providers: Vec<Box<dyn TransactionProvider>>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            providers: vec![Box::new(csv::CsvProvider), Box::new(akahu::AkahuProvider)],
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
