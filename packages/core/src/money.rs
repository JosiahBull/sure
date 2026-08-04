//! [`Money`] — a signed minor-unit amount whose magnitude has actually been checked.
//!
//! Nothing between the wire and the column used to constrain the *size* of money. A
//! `POST /api/transactions` carrying `amount_minor: 9223372036854775807` was a 201, twice over,
//! and then every report that adds those two rows up ran into `i64` overflow — with the two
//! worst possible outcomes, depending on the build:
//!
//! * **debug** (`overflow-checks` on by default): `attempt to add with overflow` panics inside
//!   the balance walk. `CatchPanicLayer` keeps the process alive, so the symptom is that the
//!   balance sheet, net worth, equity position and forecast all return a scrubbed 500 — and the
//!   rows responsible cannot be found through the UI, because the pages that would list them
//!   are the ones 500ing.
//! * **release** (the root `Cargo.toml` sets no `overflow-checks`, so wrapping is the default):
//!   the sum wraps to a small negative and the balance sheet prints a plausible, wrong number
//!   with no error anywhere. That is strictly the worse of the two.
//!
//! So the magnitude is checked once, at the wire edge, into this type (CLAUDE.md rule 1) — a
//! 422 at the body extractor, before a statement is built, instead of a 201 followed by an
//! unreadable report. [`Serialize`] emits the same JSON integer it accepted, so the wire
//! contract and the generated client are unchanged.
//!
//! This is layer one of two, and it can only ever protect rows written *after* it exists. Rows
//! already on disk are covered separately, by the checked aggregation in
//! `sure_app::reports` (`sum_minor`/`narrow_minor`), which saturates with a WARN instead of
//! panicking or wrapping. Neither layer subsumes the other: the type gives a named 422 at the
//! only edge that still knows which field the user typed, and the aggregation keeps a report
//! answerable when the ceiling never ran.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{AppError, AppResult};

/// The largest magnitude any single money figure may carry, in minor units: **$1 trillion**
/// (`1_000_000_000_000_00` cents), applied symmetrically as ±.
///
/// Chosen for the aggregation headroom it leaves, not for being "big enough" on its own:
///
/// * `i64` holds magnitudes up to ~9.22×10^18; this ceiling is 10^14, so a running total in
///   `i64` can absorb **~92,000** figures each at the absolute ceiling before it overflows.
///   Real household amounts are 10^4–10^9 minor units, which puts the true row count before
///   overflow between 10^9 and 10^14 — beyond any ledger this application will ever hold.
/// * It is four orders of magnitude above any real household figure (the most valuable
///   privately-held asset anyone tracks here is a house), so it cannot reject real data. What
///   it does catch is the shape of mistake that actually happens: a full `i64`, a value pasted
///   into the wrong field, a major-unit figure multiplied by 100 twice.
///
/// The ceiling is deliberately *not* `i64::MAX / expected_row_count`: a bound that tight would
/// have to be re-justified every time the ledger grew. It is a data-entry sanity check with
/// enough headroom that the aggregation layer's saturation is a belt-and-braces guard for
/// legacy rows rather than something a legal ledger can reach.
///
/// Also the ceiling for equity's per-unit strike/fair-value figures — `sure_dal::equity`
/// imports this constant rather than keeping its own, so there is one number in the tree.
pub const MAX_MONEY_MINOR: i64 = 1_000_000_000_000_00;

/// A signed amount in the minor units of some currency (cents, yen), bounded to
/// ±[`MAX_MONEY_MINOR`] on construction.
///
/// Negative is an outflow (or, on a liability valuation, the balance owed) — the sign
/// convention is unchanged; only the magnitude is now checked. The inner `i64` is private and
/// every constructor checks, so there is no path that produces a `Money` a report cannot add
/// up. That is what makes [`Money::abs`] total, where `i64::abs` is not: `i64::MIN.abs()`
/// panics in debug and returns `i64::MIN` in release, which is exactly how a transfer of
/// `from_amount_minor: i64::MIN` used to behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money(i64);

impl Money {
    /// Zero, in any currency. In range by definition, so it needs no check.
    pub const ZERO: Money = Money(0);

    /// The one fallible constructor. The message names the ceiling *and* the offending value,
    /// so a client that hit it can tell a typo from a unit mistake.
    pub fn new(minor: i64) -> AppResult<Self> {
        Self::from_wire(minor).map_err(AppError::validation)
    }

    /// The check itself, with a plain-`String` error so [`Deserialize`] can hand it to
    /// `serde::de::Error::custom` and the HTTP layer can turn it into a field-named 422.
    fn from_wire(minor: i64) -> Result<Self, String> {
        if !(-MAX_MONEY_MINOR..=MAX_MONEY_MINOR).contains(&minor) {
            return Err(format!(
                "amount {minor} is out of range: minor units must be within \
                 +/-{MAX_MONEY_MINOR} (±$1 trillion at two decimal places) — check whether a \
                 major-unit figure was sent, or scaled twice"
            ));
        }
        Ok(Self(minor))
    }

    /// The checked amount, for binding to a column or doing arithmetic. Free: the bound was
    /// applied once, on the way in.
    pub const fn minor(self) -> i64 {
        self.0
    }

    /// Magnitude, as a total function.
    ///
    /// This is the whole point of the bound. `sure_dal::transactions::create_transfer` does
    /// `from_amount_minor.abs()` to normalise a transfer's direction; on a raw `i64` that is a
    /// panic in debug and a *negative* "magnitude" in release for the single input `i64::MIN`,
    /// which the writer then negates again into the outflow leg. Here `|self| <= MAX_MONEY_MINOR`
    /// already holds, so there is no such input — and `saturating_abs` means the unreachable
    /// case is still not a panic, it clamps to a value this type already permits.
    pub const fn abs(self) -> Self {
        Self(self.0.saturating_abs())
    }

    /// Negation, as a total function, for the outflow leg of a transfer: the range is
    /// symmetric, so the negation of any `Money` is a `Money`.
    pub const fn neg(self) -> Self {
        Self(self.0.saturating_neg())
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<i64> for Money {
    type Error = AppError;

    fn try_from(minor: i64) -> Result<Self, Self::Error> {
        Self::new(minor)
    }
}

impl From<Money> for i64 {
    fn from(m: Money) -> Self {
        m.0
    }
}

/// Parses as a plain JSON integer and then applies the bound, so an out-of-range amount is
/// refused by the body extractor (422) before any statement is built. A JSON number too large
/// for `i64` at all (`1e30`, `9223372036854775808`) is already refused one step earlier, by
/// `i64`'s own deserializer.
impl<'de> Deserialize<'de> for Money {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let minor = i64::deserialize(de)?;
        Self::from_wire(minor).map_err(serde::de::Error::custom)
    }
}

/// Emits the same JSON integer it accepted, so putting the type on a response DTO can never
/// change a wire body — and `#[schema(value_type = i64)]` on a field keeps the generated
/// OpenAPI/TS client identical too.
impl Serialize for Money {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_i64(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps the value the way every wire DTO does, so these exercise the real serde path
    /// rather than [`Money::new`] directly.
    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Wrap {
        amount_minor: Money,
    }

    fn de(raw: &str) -> Result<Wrap, serde_json::Error> {
        serde_json::from_str(&format!(r#"{{"amount_minor":{raw}}}"#))
    }

    #[test]
    fn ordinary_amounts_round_trip_unchanged() {
        for raw in ["0", "-4250", "114269", "1234567890"] {
            let json = format!(r#"{{"amount_minor":{raw}}}"#);
            let w: Wrap = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&w).unwrap(), json);
            assert_eq!(w.amount_minor.minor(), raw.parse::<i64>().unwrap());
        }
    }

    /// The exact payload from the report: `i64::MAX` was a 201, twice, and then the balance
    /// walk added the two together.
    #[test]
    fn rejects_i64_max_and_min() {
        for raw in ["9223372036854775807", "-9223372036854775808"] {
            let err = de(raw).unwrap_err().to_string();
            assert!(err.contains("out of range"), "unhelpful message: {err}");
            assert!(
                err.contains(&MAX_MONEY_MINOR.to_string()),
                "message must name the ceiling: {err}"
            );
        }
    }

    #[test]
    fn rejects_one_past_the_ceiling_in_both_directions() {
        assert!(de(&(MAX_MONEY_MINOR + 1).to_string()).is_err());
        assert!(de(&(-MAX_MONEY_MINOR - 1).to_string()).is_err());
    }

    /// The bound is inclusive, so the documented ceiling is itself a legal amount.
    #[test]
    fn accepts_the_ceiling_itself() {
        assert_eq!(
            de(&MAX_MONEY_MINOR.to_string())
                .unwrap()
                .amount_minor
                .minor(),
            MAX_MONEY_MINOR
        );
        assert_eq!(
            de(&(-MAX_MONEY_MINOR).to_string())
                .unwrap()
                .amount_minor
                .minor(),
            -MAX_MONEY_MINOR
        );
    }

    /// A number `i64` itself cannot hold is refused before the bound is reached — the error
    /// still has to be a deserialisation failure (422), not a silent clamp.
    #[test]
    fn rejects_a_number_outside_i64_entirely() {
        assert!(de("9223372036854775808").is_err());
        assert!(de("1e30").is_err());
        assert!(de("null").is_err());
        assert!(de(r#""1000""#).is_err(), "a stringly amount is not money");
        // A fractional amount is a major-unit figure in a minor-unit field.
        assert!(de("42.5").is_err());
    }

    /// Headroom is the reason for the number, so assert it rather than trusting the comment:
    /// tens of thousands of ceiling-magnitude rows must still sum inside an `i64`.
    #[test]
    fn the_ceiling_leaves_room_to_aggregate() {
        assert_eq!(MAX_MONEY_MINOR, 1_000_000_000_000_00);
        const { assert!(i64::MAX / MAX_MONEY_MINOR >= 50_000) };
        // Two of them — the report's failing case — is nowhere near the edge.
        assert!(MAX_MONEY_MINOR.checked_mul(2).is_some());
    }

    /// `abs` is total here where `i64::abs` is not: the value that panics in debug and lies in
    /// release can never be inside a `Money` in the first place.
    #[test]
    fn abs_and_neg_are_total() {
        assert_eq!(Money::new(-4250).unwrap().abs().minor(), 4250);
        assert_eq!(Money::new(4250).unwrap().abs().minor(), 4250);
        assert_eq!(Money::ZERO.abs(), Money::ZERO);
        assert_eq!(
            Money::new(-MAX_MONEY_MINOR).unwrap().abs().minor(),
            MAX_MONEY_MINOR
        );
        assert_eq!(
            Money::new(MAX_MONEY_MINOR).unwrap().neg().minor(),
            -MAX_MONEY_MINOR
        );
        // `i64::MIN` is the input that breaks the raw version, and it cannot be constructed.
        assert!(Money::new(i64::MIN).is_err());
    }

    #[test]
    fn new_and_try_from_are_the_same_check() {
        assert_eq!(Money::try_from(7_50).unwrap().minor(), 7_50);
        assert!(Money::try_from(i64::MAX).is_err());
        assert_eq!(i64::from(Money::new(-1).unwrap()), -1);
        let err = Money::new(i64::MAX).unwrap_err();
        assert_eq!(err.code(), "validation", "must be a 422, not a 500");
    }

    /// Ordering follows the number line, so a `Vec<Money>` sorts the way a report needs.
    #[test]
    fn orders_numerically() {
        let mut v = [Money::new(10).unwrap(), Money::new(-10).unwrap()];
        v.sort();
        assert_eq!(v[0].minor(), -10);
    }
}
