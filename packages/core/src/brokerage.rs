//! Brokerage accounts: holdings lots, computed positions/snapshot, and dividend detail —
//! pure domain shapes. See `packages/dal/src/brokerage.rs` for persistence (the row shape
//! lives there, mapped into these types) and `packages/app/src/brokerage.rs` for the
//! price-lookup/FX compute that turns lots into a snapshot.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;

/// What kind of ledger entry a `holdings` row is. Stored as `holdings.kind` (plain `TEXT`).
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LotKind {
    Buy,
    Sell,
    /// A corporate action: a split/bonus issue (unpriced, quantity-only) or something
    /// like a DRIP dividend reinvestment (priced, treated like a buy) — see
    /// `sure_app::brokerage::cost_basis_by_ticker`, which is the one place that
    /// distinguishes the two.
    Corporate,
}

impl LotKind {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        match self {
            LotKind::Buy => "buy",
            LotKind::Sell => "sell",
            LotKind::Corporate => "corporate",
        }
    }
}

impl std::str::FromStr for LotKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "buy" => LotKind::Buy,
            "sell" => LotKind::Sell,
            "corporate" => LotKind::Corporate,
            other => return Err(format!("unknown lot kind '{other}'")),
        })
    }
}

#[derive(Debug, Serialize, ToSchema, Clone)]
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
    pub kind: LotKind,
    pub external_id: Option<String>,
    pub provider: Option<String>,
    pub created_at: String,
}

/// Manually record a trade (parity with equity's manual grant entry — most lots come
/// from a bulk import instead).
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveHoldingLot {
    pub ticker: String,
    #[serde(default)]
    pub exchange: String,
    #[serde(default)]
    pub name: Option<String>,
    pub currency_code: String,
    #[schema(value_type = String)]
    pub trade_date: IsoDate,
    pub quantity: f64,
    #[serde(default)]
    pub unit_price: Option<f64>,
    #[serde(default)]
    pub fee_minor: i64,
    #[serde(default = "manual_kind")]
    pub kind: LotKind,
}
fn manual_kind() -> LotKind {
    LotKind::Buy
}

/// A ticker position as of a date: quantity currently held, its latest known price, and
/// the resulting market value in the position's own trading currency.
#[derive(Debug, Serialize, ToSchema, Clone)]
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
    /// Remaining cost basis of the current position, average-cost method, in
    /// `currency_code`. `None` if the position was fully exited (no remaining basis) or no
    /// lot ever carried a price (nothing to base a cost on). An estimate, not authoritative
    /// — `holdings.unit_price` is informational-only, and corporate actions (splits, bonus
    /// issues) with no price are treated as quantity-only adjustments.
    pub cost_basis_minor: Option<i64>,
    /// (market_value − cost_basis) / cost_basis × 100. `None` if either side is unavailable.
    pub return_pct: Option<f64>,
}

/// A wallet cash balance in one currency, as of a date.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct WalletBalance {
    pub currency_code: String,
    pub amount_minor: i64,
}

/// A brokerage account's full computed value as of a date: every position plus every
/// wallet balance, converted into the account's own currency for `total_value_minor`.
#[derive(Debug, Serialize, ToSchema)]
pub struct BrokerageSnapshot {
    pub account_id: i64,
    pub as_of: String,
    pub currency_code: String,
    pub positions: Vec<Position>,
    pub wallets: Vec<WalletBalance>,
    /// Sum of every position and wallet balance that *could* be converted into
    /// `currency_code`. Anything in `unconverted` is missing from it — read the two together
    /// or the figure reads as complete when it isn't.
    pub total_value_minor: i64,
    /// Currency codes held here that have no exchange rate to `currency_code`, so their
    /// value is absent from `total_value_minor` rather than counted at parity. Non-empty
    /// makes the snapshot unpersistable — see `sure_app::brokerage::BrokerageService::revalue`.
    pub unconverted: Vec<String>,
    /// Newest date across the exchange rates used (ISO-8601), `null` if none are on record.
    /// The rate poller only writes on success, so this is the only signal that the feed has
    /// been down and these figures are converted at last year's rates.
    pub rates_as_of: Option<String>,
    pub activity_30d: BrokerageActivity30d,
}

/// A rolling 30-days-to-`as_of` cash-movement summary for a brokerage account.
///
/// `trades` (buy/sell lot count) is an exact count. `contributions_minor`/
/// `withdrawals_minor` are a **heuristic** — the wallet-cash ledger is just the account's
/// ordinary `transactions` rows, and today nothing distinguishes an external top-up/
/// withdrawal from internal trade-settlement cash movement at the data-model level (e.g.
/// the Sharesies importer files deposits, withdrawals, and trade settlement all under one
/// `"Transfers"` category). This matches on the raw transaction `description` text
/// (provider-specific phrasing like "Wallet top up"/"Withdrawal"), so it only recognises
/// contributions/withdrawals it has seen a pattern for — not a durable classification.
#[derive(Debug, Serialize, ToSchema, Clone, Default)]
pub struct BrokerageActivity30d {
    pub contributions_minor: i64,
    pub withdrawals_minor: i64,
    pub trades: i64,
}

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct DividendWithholding {
    pub id: i64,
    pub dividend_id: i64,
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

#[derive(Debug, Serialize, ToSchema, Clone)]
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

#[derive(Debug, Serialize, ToSchema)]
pub struct DividendDetail {
    pub dividend: Dividend,
    pub withholdings: Vec<DividendWithholding>,
}

/// The outcome of a bulk zip import: counts for each of the three things it can write,
/// plus any per-record parse issues that were skipped rather than failing the whole
/// import.
#[derive(Debug, Serialize, ToSchema, Default)]
pub struct BrokerageImportResult {
    pub transactions_imported: i64,
    pub transactions_skipped: i64,
    pub holdings_imported: i64,
    pub holdings_skipped: i64,
    pub dividends_imported: i64,
    pub dividends_skipped: i64,
    pub warnings: Vec<String>,
}
