use std::collections::BTreeMap;

use serde::Deserialize;

/// One rate table, as `GET /latest?base=<code>` describes it.
///
/// **A deliberate subset.** The response also carries `amount` (always `1.0` for the query this
/// crate makes) and `base` (always the code that was asked for), and neither tells a caller
/// anything it did not already know. Adding a field here should be a decision about *needing*
/// it, not about completeness.
///
/// Unknown fields are ignored rather than rejected (serde's default), which is what keeps a
/// currency conversion working on the day the upstream adds something.
///
/// A [`BTreeMap`] rather than a `HashMap`: the iteration order is then the currency code's, so a
/// caller that renders or persists the table gets a stable order for free instead of one that
/// changes per process.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct LatestRates {
    /// The ECB reference date these rates are *for* — not the day they were fetched.
    ///
    /// An ISO date as the wire spells it, left as a string: a date type here would mean picking
    /// a calendar library on this crate's behalf, and the one caller wants the string anyway
    /// (`ExchangeRateQuote::as_of`, which a report renders as "rates as of").
    pub date: String,

    /// Quote currency → units of it per one unit of the base, as the wire quotes them.
    ///
    /// Left as the `f64` the JSON carries rather than converted to a decimal here, for the same
    /// reason `house-pricer-client` leaves dollars as `f64`: this crate's job is to say
    /// faithfully what the upstream said. `sure_providers::frankfurter` does the conversion with
    /// `Decimal::from_f64` — the shortest decimal that round-trips, so `0.87207` stays five
    /// digits — and drops anything that will not convert.
    pub rates: BTreeMap<String, f64>,
}

impl LatestRates {
    /// Build a table directly, for a caller testing what it does with one.
    ///
    /// The alternative is a JSON literal in a test that is not about JSON, which is exactly what
    /// this crate exists to stop leaking outwards: `sure_providers::frankfurter`'s mapping tests
    /// are about `f64` → `Decimal` and about what a quote is, and they should not have to
    /// restate a wire format to get at either. Needed because the struct is `#[non_exhaustive]`,
    /// which is right for a wire type that will gain fields but blocks a struct literal from
    /// another crate.
    pub fn new<K: Into<String>>(
        date: impl Into<String>,
        rates: impl IntoIterator<Item = (K, f64)>,
    ) -> Self {
        Self {
            date: date.into(),
            rates: rates
                .into_iter()
                .map(|(code, rate)| (code.into(), rate))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape, spelled the way the upstream spells it.
    ///
    /// Rates are public market data, so CLAUDE.md rule 3 does not reach them; these are
    /// plausible ECB figures against a USD base.
    #[test]
    fn parses_a_typical_response() {
        let parsed: LatestRates = serde_json::from_str(
            r#"{"amount":1.0,"base":"USD","date":"2026-07-16","rates":{"EUR":0.87207,"NZD":1.7078}}"#,
        )
        .expect("a typical rate table parses");

        assert_eq!(parsed.date, "2026-07-16");
        assert_eq!(parsed.rates.len(), 2);
        assert_eq!(parsed.rates.get("EUR"), Some(&0.87207));
        assert_eq!(parsed.rates.get("NZD"), Some(&1.7078));
    }

    /// The same payload, from raw bytes rather than a `&str`.
    ///
    /// [`crate::FrankfurterClient`] ends in `serde_json::from_slice` over a buffer it built
    /// chunk-by-chunk off the socket, not `Response::json`'s own decode — so the same document
    /// has to deserialise from bytes, split across chunk boundaries and all. A realistic body is
    /// ~2KB, three orders of magnitude under the ceiling.
    #[test]
    fn decodes_the_body_bytes_the_capped_reader_accumulates() {
        let wire = br#"{"amount":1.0,"base":"USD","date":"2026-07-16","rates":{"NZD":1.7078}}"#;
        let parsed: LatestRates = serde_json::from_slice(wire).expect("the wire bytes decode");

        assert_eq!(parsed.date, "2026-07-16");
        assert_eq!(parsed.rates.get("NZD"), Some(&1.7078));
    }

    /// The property that keeps this crate from breaking on somebody else's release: `amount` and
    /// `base` are already fields it declines to read, so an added one is that case at rest.
    #[test]
    fn unknown_fields_are_ignored_rather_than_fatal() {
        let parsed: LatestRates = serde_json::from_str(
            r#"{"amount":1.0,"base":"USD","date":"2026-07-16","provider":"ecb",
                "rates":{"NZD":1.7078}}"#,
        )
        .expect("an unrecognised field must not fail the table");
        assert_eq!(parsed.rates.get("NZD"), Some(&1.7078));
    }

    /// The constructor the caller's mapping tests use instead of a JSON literal, and the order
    /// they can then rely on: a `BTreeMap` iterates by code regardless of what the wire listed
    /// first.
    #[test]
    fn a_table_can_be_built_without_a_wire_document() {
        let table = LatestRates::new("2026-07-16", [("NZD", 1.7078), ("EUR", 0.87207)]);
        assert_eq!(table.date, "2026-07-16");
        assert_eq!(
            table.rates.keys().collect::<Vec<_>>(),
            ["EUR", "NZD"],
            "a BTreeMap iterates by code, not by insertion",
        );
    }
}
