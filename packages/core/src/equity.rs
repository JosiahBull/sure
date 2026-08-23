//! Equity grants, exercises, and computed vesting status: wire/domain shapes.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct EquityGrant {
    pub id: i64,
    pub account_id: i64,
    pub company: String,
    pub grant_date: String,
    pub quantity: i64,
    pub strike_minor: i64,
    pub currency_code: String,
    pub vest_months: i64,
    pub cliff_months: i64,
    pub unit_value_minor: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveGrant {
    pub company: String,
    #[schema(value_type = String)]
    pub grant_date: IsoDate,
    pub quantity: i64,
    #[serde(default)]
    pub strike_minor: i64,
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default = "vest48")]
    pub vest_months: i64,
    #[serde(default = "cliff12")]
    pub cliff_months: i64,
    /// Convenience: what one unit is worth, recorded as a [`EquityMark`] at `grant_date` on the
    /// way in rather than kept on the grant.
    ///
    /// The mark ledger is the only thing valuation reads — a price is a level that changes at
    /// funding rounds, and a scalar per grant could not say when it started applying or let two
    /// grants disagree. This field stays because supplying a grant and its price together is the
    /// common case, and because `config/export` snapshots carry it; the column it writes to is
    /// no longer read.
    #[serde(default)]
    pub unit_value_minor: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
}
fn vest48() -> i64 {
    48
}
fn cliff12() -> i64 {
    12
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EquityExercise {
    pub id: i64,
    pub grant_id: i64,
    pub exercise_date: String,
    pub quantity: i64,
    pub price_minor: i64,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveExercise {
    #[schema(value_type = String)]
    pub exercise_date: IsoDate,
    pub quantity: i64,
    #[serde(default)]
    pub price_minor: i64,
    #[serde(default)]
    pub note: Option<String>,
}

/// What a history rebuild wrote.
///
/// A count and a range rather than the rows themselves: the caller that wants the series already
/// has `GET /accounts/{id}/valuations` for it, and returning `Valuation` values from here would
/// mean inventing an `id` and a `created_at` for rows that were upserted rather than inserted.
#[derive(Debug, Serialize, ToSchema)]
pub struct RebuildResult {
    /// How many dates were written — one per date the position's value could change.
    pub written: i64,
    /// Oldest and newest date in the rebuilt series, absent if there was nothing to write
    /// (no grants, or none dated on or before today).
    pub from: Option<String>,
    pub to: Option<String>,
}

/// What one unit of an unlisted holding was worth, from `as_of` until the next mark.
///
/// The private-company counterpart to a `stock_prices` row: there is no feed to poll, so the
/// figure is entered by hand after each funding round or internal revaluation. Together with the
/// grant schedule — which fixes *how many* units are held on any date — this is what makes an
/// account's whole valuation history computable rather than hand-entered.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct EquityMark {
    pub id: i64,
    pub account_id: i64,
    pub as_of: String,
    pub unit_value_minor: i64,
    pub currency_code: String,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveMark {
    #[schema(value_type = String)]
    pub as_of: IsoDate,
    pub unit_value_minor: i64,
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// One dated change in what an account holds: a vesting tranche or an exercise.
///
/// The quantity side of the ledger, and the counterpart to a brokerage account's `holdings`
/// lots. Vesting rows are *computed* from each grant's schedule rather than stored — a tranche
/// is not an event anybody records, it is what the deed already says will happen on the 1st of
/// each month — so this type is built on read and has no table behind it.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct EquityEvent {
    pub date: String,
    pub grant_id: i64,
    /// Which grant, as the user labelled it — so several grants from one employer are
    /// distinguishable without opening each.
    pub grant_label: Option<String>,
    pub kind: EquityEventKind,
    /// Units vesting, or units exercised. Always positive.
    pub quantity: i64,
    /// Units of this grant vested in total, after this event.
    pub vested_running: i64,
    /// Units of this grant exercised in total, after this event.
    pub exercised_running: i64,
    /// The mark in force on `date`, if the ledger has one by then.
    pub unit_value_minor: Option<i64>,
    pub note: Option<String>,
}

/// What kind of change an [`EquityEvent`] records.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum EquityEventKind {
    /// A monthly tranche becoming vested. Computed from the schedule, not stored.
    Vest,
    /// The cliff releasing its accumulated months in one step. Its own kind rather than a
    /// `Vest` that happens to be large, because it is the one date on a grant where nothing
    /// vests before and a year's worth vests at once.
    Cliff,
    /// An `equity_exercises` row: options becoming shares.
    Exercise,
}

impl EquityEventKind {
    /// The wire representation (snake_case) — matches `#[serde(rename_all = "snake_case")]`.
    /// Built directly rather than parsed (there is no table to read one back from), so there
    /// is no `FromStr` pair.
    pub fn as_str(self) -> &'static str {
        match self {
            EquityEventKind::Vest => "vest",
            EquityEventKind::Cliff => "cliff",
            EquityEventKind::Exercise => "exercise",
        }
    }
}

/// Vesting/exercise status of a grant as of a date.
#[derive(Debug, Serialize, ToSchema)]
pub struct VestingStatus {
    pub grant_id: i64,
    pub company: String,
    pub as_of: String,
    pub quantity: i64,
    pub vested: i64,
    pub unvested: i64,
    pub exercised: i64,
    /// Vested but not yet exercised (i.e. currently exercisable).
    pub vested_unexercised: i64,
    /// Shares actually held, having been exercised. Equal to [`Self::exercised`], because
    /// there is no disposal ledger: `equity_exercises` records options turning into shares
    /// and nothing records a share leaving again. That is the right model for the unlisted
    /// grants this table exists for — a private company's shares generally cannot be sold
    /// until an exit — but a holding that *can* be disposed of needs its own rows before
    /// this field can diverge from `exercised`, and every reader here would have to be
    /// revisited. Kept as its own field rather than having callers reach for `exercised`
    /// so that day arrives as one change, not a search for every site that assumed it.
    pub owned: i64,
    pub strike_minor: i64,
    pub unit_value_minor: Option<i64>,
    pub currency_code: String,
    /// Intrinsic value of vested-unexercised units: qty × max(0, unit_value − strike).
    ///
    /// Deliberately *not* the grant's whole worth — see [`Self::owned_value_minor`], which
    /// carries the other half.
    pub intrinsic_value_minor: i64,
    /// Market value of the shares already owned: [`Self::owned`] × unit_value, at full
    /// price rather than intrinsic.
    ///
    /// Full price because the strike on these units is not a cost still to be met — it was
    /// paid in cash on the exercise date and has already left a bank account this app
    /// tracks. Netting it off here would count the same spend twice.
    pub owned_value_minor: i64,
    /// What this grant is worth in total: [`Self::intrinsic_value_minor`] +
    /// [`Self::owned_value_minor`]. Unvested units are excluded — they are contingent on
    /// staying employed and are cancelled outright on leaving.
    pub total_value_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AccountEquity {
    pub account_id: i64,
    pub as_of: String,
    pub currency_code: String,
    pub grants: Vec<VestingStatus>,
    /// Summed [`VestingStatus::intrinsic_value_minor`]: the vested-but-unexercised options.
    pub total_intrinsic_minor: i64,
    /// Summed [`VestingStatus::owned_value_minor`]: the shares already exercised and held.
    pub total_owned_minor: i64,
    /// The account's value: `total_intrinsic_minor + total_owned_minor`. This — not
    /// `total_intrinsic_minor` — is what a revaluation persists, because an exercise moves
    /// units from one of those two figures to the other and must not read as the position
    /// shrinking by everything that was exercised.
    pub total_value_minor: i64,
    /// The mark used for every figure above: the latest [`EquityMark`] dated on or before
    /// `as_of`. `None` means the ledger has no mark by that date, in which case the position
    /// values at zero — the quantities are still exact, and nothing here is a guess at a price
    /// nobody has supplied.
    pub unit_value_minor: Option<i64>,
    /// The date that mark was set, so a stale price is visible as one rather than being read as
    /// current.
    pub unit_value_as_of: Option<String>,
}
