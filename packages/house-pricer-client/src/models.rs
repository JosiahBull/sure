use serde::Deserialize;

/// One property, as `GET /match?q=<address>` describes it.
///
/// **A deliberate subset.** The response carries ~45 fields; this reads four. That is not
/// laziness about coverage — every additional field is one more piece of somebody's house that
/// the process holds in memory and could log, and the rest (boundary polygon, GPS centroid,
/// legal description, rating valuations) has no bearing on what the property is worth. Adding a
/// field here should be a decision about *needing* it, not about completeness.
///
/// `#[serde(rename_all = "camelCase")]` maps the upstream's spelling; the field names below are
/// this crate's, and are the stable surface callers see. When House Pricer renames something,
/// the fix is a `#[serde(rename = "...")]` or an alias on the field it renamed — one line, in
/// this file, with nothing downstream recompiling its idea of what a valuation is.
///
/// Unknown fields are ignored rather than rejected (serde's default): the endpoint is
/// undocumented and gains fields without warning, and a listing that fails because the upstream
/// added something is a worse outcome than one that ignores it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PropertyMatch {
    /// The upstream's stable id for the property.
    ///
    /// What a caller can pin a subscription to, so a fuzzy `q` that starts resolving to the
    /// neighbouring house is caught rather than silently recorded.
    pub unit_of_property_id: String,

    /// The upstream's own normalised address for the match.
    ///
    /// Personal data in the same sense the rest of the response is: it is where somebody lives.
    /// Nothing in this crate logs it.
    pub street_address: String,

    /// Model A's predicted gross sale price, in **dollars** — the wire's own unit and scale.
    ///
    /// Left as the `f64` the JSON carries rather than converted to a decimal or to minor units
    /// here. Two reasons: this crate's job is to say faithfully what the upstream said, and the
    /// conversion needs a currency and a minor-unit scale that are Sure's business, not House
    /// Pricer's. `sure_providers::house_pricer` does the conversion, and refuses a figure that
    /// will not fit.
    ///
    /// `Option` because the upstream omits it for a match it has no model output for.
    pub gross_sale_price_predicted_model_a: Option<f64>,

    /// Model B's prediction, in dollars.
    ///
    /// The same response carries two models and documents neither. Which one to *record* is a
    /// judgement for the caller, not for this crate — both are handed over.
    pub gross_sale_price_predicted_model_b: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire shape, spelled the way the upstream spells it. Invented address and figures
    /// (CLAUDE.md rule 3): nothing recorded from this host may be committed, so every fixture
    /// in this crate is hand-authored.
    const MATCH: &str = r#"{
        "unitOfPropertyId": "UOP-0000001",
        "streetAddress": "1 Invented Street, Christchurch",
        "grossSalePricePredictedModelA": 705500.0,
        "grossSalePricePredictedModelB": 649000.0
    }"#;

    #[test]
    fn parses_a_match() {
        let parsed: PropertyMatch = serde_json::from_str(MATCH).expect("a typical match parses");
        assert_eq!(parsed.unit_of_property_id, "UOP-0000001");
        assert_eq!(parsed.street_address, "1 Invented Street, Christchurch");
        assert_eq!(parsed.gross_sale_price_predicted_model_a, Some(705_500.0));
        assert_eq!(parsed.gross_sale_price_predicted_model_b, Some(649_000.0));
    }

    /// A match the model could not price still parses. The caller decides what that means —
    /// for Sure it is "the upstream knows this address but answered in a shape we cannot use",
    /// which is different from "no such address".
    #[test]
    fn a_match_with_no_prediction_still_parses() {
        let parsed: PropertyMatch = serde_json::from_str(
            r#"{"unitOfPropertyId":"UOP-0000002","streetAddress":"2 Invented Street"}"#,
        )
        .expect("a match with no model output parses");
        assert_eq!(parsed.gross_sale_price_predicted_model_a, None);
        assert_eq!(parsed.gross_sale_price_predicted_model_b, None);
    }

    /// The property that keeps this crate from breaking on somebody else's release: the
    /// endpoint is undocumented and gains fields without notice, and the ~41 fields this type
    /// deliberately omits are exactly that case at rest.
    #[test]
    fn unknown_fields_are_ignored_rather_than_fatal() {
        let parsed: PropertyMatch = serde_json::from_str(
            r#"{
                "unitOfPropertyId": "UOP-0000003",
                "streetAddress": "3 Invented Street",
                "grossSalePricePredictedModelA": 512000.0,
                "titleBoundaryPolygon": [[0,0],[1,1]],
                "someFieldAddedNextTuesday": {"nested": true}
            }"#,
        )
        .expect("an unrecognised field must not fail the match");
        assert_eq!(parsed.unit_of_property_id, "UOP-0000003");
        assert_eq!(parsed.gross_sale_price_predicted_model_a, Some(512_000.0));
    }
}
