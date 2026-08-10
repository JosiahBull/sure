//! The dependencies the tools need, injected by the composition root.

use std::sync::Arc;

use sure_app::brokerage::BrokerageService;
use sure_app::ports::{
    AccountRepo, CategoryRepo, CurrencyRepo, EquityRepo, MerchantRepo, SettingsRepo,
    StockPriceProvider, TransactionRepo, ValuationRepo,
};
use sure_app::reports::ReportService;
use sure_app::rules::RuleService;

/// Shared state handed to every tool. Cheap to clone — every field is an `Arc`, and the
/// transport builds one handler per request.
///
/// Deliberately **not** `sure_api::AppState`. That struct is the HTTP adapter's declaration
/// of what its handlers need; this is the MCP adapter's, and it is a strict subset — no
/// import pipeline, no provider registry, no snapshot repo, because no tool here may reach
/// them (see the "never exposed" list in `docs/MCP.md`). Sharing one struct would make that
/// a matter of discipline; two structs make it a matter of what is in scope.
///
/// Every field type is defined by `sure-app`, so this crate — like `sure-api` — never names
/// `sure_dal` or `sqlx`. `sure-server` builds it from the same one `SqliteStore`.
#[derive(Clone)]
pub struct McpState {
    pub reports: Arc<ReportService>,
    pub rules: Arc<RuleService>,
    pub brokerage: Arc<BrokerageService>,
    pub accounts: Arc<dyn AccountRepo>,
    pub transactions: Arc<dyn TransactionRepo>,
    pub categories: Arc<dyn CategoryRepo>,
    pub merchants: Arc<dyn MerchantRepo>,
    pub valuations: Arc<dyn ValuationRepo>,
    /// Vesting grants, for a private-shares account's detail view.
    pub equity: Arc<dyn EquityRepo>,
    pub settings: Arc<dyn SettingsRepo>,
    /// Read for one thing only: each currency's `decimal_places`, which is what turns minor
    /// units into the decimal string a caller is shown. Defaulting that to 2 would render
    /// ¥4250 as "¥42.50".
    pub currencies: Arc<dyn CurrencyRepo>,
    /// The price feed, for a brokerage account's live position value.
    pub stock_price_provider: Arc<dyn StockPriceProvider>,
    /// The process lifecycle handle.
    ///
    /// Two uses, both of which a bare `tokio::spawn` would get wrong: the report
    /// aggregations hand their pure-compute half to [`Shutdown::spawn_blocking`] so a long
    /// roll-up cannot stall a tokio worker the way it would inside an `async fn`, and the
    /// transport takes a child cancellation token so an in-flight tool call is part of the
    /// drain rather than something the process walks away from mid-write.
    ///
    /// [`Shutdown::spawn_blocking`]: sure_appbase::Shutdown::spawn_blocking
    pub shutdown: sure_appbase::Shutdown,
}
