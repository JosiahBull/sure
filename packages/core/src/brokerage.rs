//! Brokerage accounts: holdings lots, computed positions/snapshot, and dividend detail —
//! wire/domain shapes. See `packages/dal/src/brokerage.rs` for persistence and
//! `packages/api/src/brokerage.rs` for the price-lookup/FX compute that turns lots into a
//! snapshot.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct HoldingLot {
    pub id: i64,
    pub account_id: i64,
    pub ticker: String,
    pub exchange: String,
    pub name: Option<String>,
    pub currency_code: String,
    pub trade_date: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: String,
    pub external_id: Option<String>,
    pub provider: Option<String>,
    pub created_at: String,
}

/// Manually record a trade (parity with equity's manual grant entry — most lots come
/// from a bulk import instead).
#[derive(Deserialize, ToSchema)]
pub struct SaveHoldingLot {
    pub ticker: String,
    #[serde(default)]
    pub exchange: String,
    #[serde(default)]
    pub name: Option<String>,
    pub currency_code: String,
    pub trade_date: String,
    pub quantity: f64,
    #[serde(default)]
    pub unit_price: Option<f64>,
    #[serde(default)]
    pub fee_minor: i64,
    #[serde(default = "manual_kind")]
    pub kind: String,
}
fn manual_kind() -> String {
    "buy".to_string()
}

/// A ticker position as of a date: quantity currently held, its latest known price, and
/// the resulting market value in the position's own trading currency.
#[derive(Serialize, ToSchema, Clone)]
pub struct Position {
    pub ticker: String,
    pub exchange: String,
    pub name: Option<String>,
    pub currency_code: String,
    pub quantity: f64,
    /// Unit price as of the snapshot date, if a quote was found (a delisted/unrecognised
    /// ticker has a quantity but no price).
    pub price: Option<String>,
    pub price_as_of: Option<String>,
    /// quantity × price, in `currency_code` (the position's own trading currency, not
    /// necessarily the account's).
    pub market_value_minor: Option<i64>,
}

/// A wallet cash balance in one currency, as of a date.
#[derive(Serialize, ToSchema, Clone)]
pub struct WalletBalance {
    pub currency_code: String,
    pub amount_minor: i64,
}

/// A brokerage account's full computed value as of a date: every position plus every
/// wallet balance, converted into the account's own currency for `total_value_minor`.
#[derive(Serialize, ToSchema)]
pub struct BrokerageSnapshot {
    pub account_id: i64,
    pub as_of: String,
    pub currency_code: String,
    pub positions: Vec<Position>,
    pub wallets: Vec<WalletBalance>,
    pub total_value_minor: i64,
}

#[derive(Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct DividendWithholding {
    pub id: i64,
    pub dividend_id: i64,
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

#[derive(Serialize, ToSchema, Clone)]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
pub struct Dividend {
    pub id: i64,
    pub account_id: i64,
    pub ticker: String,
    pub exchange: String,
    pub record_date: Option<String>,
    pub paid_date: String,
    pub shares_held: Option<f64>,
    pub gross_amount_minor: i64,
    pub net_amount_minor: i64,
    pub currency_code: String,
    pub external_id: Option<String>,
    pub provider: Option<String>,
    pub created_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct DividendDetail {
    pub dividend: Dividend,
    pub withholdings: Vec<DividendWithholding>,
}

/// The outcome of a bulk zip import: counts for each of the three things it can write,
/// plus any per-record parse issues that were skipped rather than failing the whole
/// import.
#[derive(Serialize, ToSchema, Default)]
pub struct BrokerageImportResult {
    pub transactions_imported: i64,
    pub transactions_skipped: i64,
    pub holdings_imported: i64,
    pub holdings_skipped: i64,
    pub dividends_imported: i64,
    pub dividends_skipped: i64,
    pub warnings: Vec<String>,
}
