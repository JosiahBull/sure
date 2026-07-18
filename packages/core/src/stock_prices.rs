use serde::Serialize;
use utoipa::ToSchema;

/// A single day's cached closing price for a ticker (see
/// `sure_providers::StockPriceProvider`), keyed by `(ticker, exchange, as_of)`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct StockPrice {
    pub ticker: String,
    /// Free-text exchange hint (e.g. `"NZX"`), or `""` if none was set.
    pub exchange: String,
    /// ISO-8601 date this close is for.
    pub as_of: String,
    /// Decimal text (exact), e.g. `"5.60"`.
    pub close: String,
    pub currency_code: String,
    /// When this row was fetched (ISO-8601 timestamp, UTC).
    pub fetched_at: String,
}
