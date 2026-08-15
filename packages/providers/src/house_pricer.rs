//! [`PropertyEstimateProvider`] backed by House Pricer (<https://www.housepricer.co.nz>) — a
//! free, keyless automated valuation model for **Christchurch, New Zealand**, built on
//! Christchurch City Council sale/valuation records and LINZ property data. Same standing
//! caveat as Frankfurter and Yahoo: the endpoint is undocumented and could change without
//! notice.
//!
//! `GET /match?q=<address>` answers a fuzzy address search with one property's facts, several
//! of which are model output rather than record — this adapter takes exactly two fields from it
//! (an id and one estimate) and drops the rest.
//!
//! **Nothing recorded from this host may be committed.** The response is not market data: it is
//! a dossier on one dwelling — street address, GPS centroid, title boundary polygon, legal
//! description, land and improvement values. That is personal data about wherever the person
//! running Sure lives, so it is treated like Akahu's traffic rather than like Frankfurter's:
//! `scripts/pii-scan.mjs` refuses a `house_pricer` recording by path *and* by content, and the
//! fixtures below are hand-authored with an invented address (CLAUDE.md rule 3).

use anyhow::Context;
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::Deserialize;

use sure_app::ports::{PropertyEstimate, PropertyEstimateProvider};

use crate::http::Endpoint;

/// The real endpoint. `pub` because the composition root owns the decision of where this
/// provider points (it is the only place configuration is read) and needs a default to fall
/// back to.
pub const DEFAULT_BASE_URL: &str = "https://www.housepricer.co.nz/api/property/core";

/// The only currency this feed quotes. Not configurable and not read from the response, which
/// carries no currency field at all: the source is one New Zealand city's council and LINZ
/// records, and every figure in it is NZD. Named rather than inlined so the assumption is
/// searchable — and the poll checks the subscribed account against it rather than booking a
/// foreign-currency property at parity.
pub const QUOTE_CURRENCY: &str = "NZD";

/// Minor units per major unit for [`QUOTE_CURRENCY`]. NZD is a 2-decimal currency; a hardcoded
/// 100 is correct for exactly as long as this feed is NZD-only, which is as long as it is
/// Christchurch-only.
const MINOR_UNITS_PER_DOLLAR: i64 = 100;

pub struct HousePricerProvider {
    endpoint: Endpoint,
    client: reqwest::Client,
}

impl HousePricerProvider {
    /// The only constructor, and deliberately so: there is no argument-free `new()` that
    /// reaches for [`DEFAULT_BASE_URL`] itself. That const is the composition root's fallback
    /// (`Config::from_env` parses it into an [`Endpoint`]), and a second constructor holding the
    /// same URL would be the one a future caller reached for by reflex — pointing an adapter at
    /// the live API from inside a test, past the configuration that was supposed to decide it.
    /// See `lib.rs`.
    ///
    /// In practice the endpoint is either that parsed default or the record/replay proxy a test
    /// binds on loopback. Unlike the other two feeds, a *recording* against the live host is
    /// not a thing this repository may keep (see the module docs), so the fetch path is
    /// exercised against hand-authored stubs.
    pub fn with_endpoint(endpoint: Endpoint) -> Self {
        let client = crate::http::client(&endpoint);
        Self { endpoint, client }
    }
}

/// The subset of `/match` this adapter reads.
///
/// The response carries ~45 fields. Deserialising four of them is not laziness about coverage:
/// every additional field is one more piece of somebody's house that this process holds in
/// memory and could log, and none of the rest — the boundary polygon, the centroid, the legal
/// description — has any bearing on what the property is worth *to a net-worth figure*.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchResponse {
    /// The upstream's stable id for the property. What pins a subscription, so that a fuzzy
    /// `q` that starts resolving to the neighbouring house is caught instead of recorded.
    unit_of_property_id: String,
    /// The upstream's own normalised address for the match.
    street_address: String,
    /// Model A's predicted gross sale price, in dollars. **The figure Sure records** — chosen
    /// deliberately over Model B, which the same response also carries and which ran ~8% lower
    /// on the match this was built against. The upstream documents neither model, so there is
    /// no principled way to prefer one on the merits; this is a stated choice, and
    /// [`Self::gross_sale_price_predicted_model_b`] rides along in the note so the spread stays
    /// visible on the valuation itself.
    gross_sale_price_predicted_model_a: Option<f64>,
    /// Model B's prediction. Read *only* to record it in the note beside model A's.
    gross_sale_price_predicted_model_b: Option<f64>,
}

#[async_trait]
impl PropertyEstimateProvider for HousePricerProvider {
    fn kind(&self) -> &'static str {
        "house_pricer"
    }

    fn description(&self) -> &'static str {
        "Automated property value estimates, from Christchurch City Council and LINZ records"
    }

    fn coverage(&self) -> &'static str {
        "Christchurch, New Zealand"
    }

    async fn fetch_estimate(&self, query: &str) -> anyhow::Result<Option<PropertyEstimate>> {
        let query = query.trim();
        // Refused here rather than sent: an empty `q` is a 400 from the upstream, and the one
        // caller that can produce one is a pre-flight from an account with no address typed yet.
        anyhow::ensure!(
            !query.is_empty(),
            "an address is needed to look up a property estimate"
        );

        // `query_pairs_mut` rather than interpolating into the URL: `q` is a street address, so
        // it always needs percent-encoding, and hand-rolling that is how a `&` or a `#` in an
        // address silently truncates the search. It encodes identically every time, which the
        // replay index needs — it compares the query verbatim.
        //
        // `RequestBuilder::query` would say this in one line, but it is behind a reqwest feature
        // this workspace deliberately does not enable (`default-features = false`, `json` +
        // `rustls` only — see the root manifest), and widening that for one parameter is a
        // change to every crate's build. `Url` is already in reach: `Endpoint::parse` uses it.
        //
        // Parsing the concatenated base is safe in a way it would not be for the bare endpoint:
        // `Url` normalises an empty path to `/`, which is why `Endpoint` keeps its string
        // verbatim, but `{base}/match` always has a path already.
        let mut url = reqwest::Url::parse(&format!("{}/match", self.endpoint.url()))
            .context("build the property-estimate request URL")?;
        url.query_pairs_mut().append_pair("q", query);

        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .with_context(|| format!("fetch a property estimate from {url}"))?;

        // No match is the ordinary answer, not a failure: House Pricer covers one city, so
        // every address outside it 404s, as does one with a typo. The body says
        // `{"_embedded":{"errors":[{"message":"No matching house found"}]}}`; nothing here reads
        // it, because the status alone carries the same meaning and the body would be one more
        // echo of the address to keep. The caller — a pre-flight the person is watching, or the
        // monthly poll — decides what to do with "nothing".
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            tracing::debug!("no property matched the address given");
            return Ok(None);
        }

        // `json_capped`, not `.json()`: a match response is ~1.5KB, and the request timeout
        // bounds how long this undocumented endpoint may talk, not how much it may say. See
        // `http.rs`.
        let body: MatchResponse = crate::http::json_capped(response.error_for_status()?).await?;
        parse_estimate(body).map(Some)
    }
}

/// Turn a match into the one estimate Sure records, or fail saying which field was unusable.
///
/// Split out of the fetch so the whole mapping — model choice, the note, the minor-unit
/// conversion and every way it can go wrong — is testable without a socket.
fn parse_estimate(body: MatchResponse) -> anyhow::Result<PropertyEstimate> {
    // A match with no model-A figure is a match this adapter cannot use. It is an error rather
    // than `Ok(None)` because the two mean different things to the caller: `None` is "the
    // upstream doesn't know this address", which is final and worth telling the person, whereas
    // this is "the upstream knows it but answered in a shape we don't understand" — worth a
    // warning and a retry next month, and worth not overwriting last month's good figure with.
    let model_a = body
        .gross_sale_price_predicted_model_a
        .context("the match carried no model-A predicted sale price")?;

    let value_minor = dollars_to_minor(model_a)
        .with_context(|| format!("model-A predicted sale price {model_a} is out of range"))?;

    // Both models, in the note, so the recorded figure is self-describing: a valuation of
    // $705,500 with no provenance is indistinguishable from one somebody typed. `{:.0}` because
    // the feed quotes whole dollars, and a note is for reading.
    let model_note = match body.gross_sale_price_predicted_model_b {
        Some(model_b) => format!("model A {model_a:.0}, model B {model_b:.0}"),
        None => format!("model A {model_a:.0}"),
    };

    Ok(PropertyEstimate {
        property_id: body.unit_of_property_id,
        matched_address: body.street_address,
        value_minor,
        currency_code: QUOTE_CURRENCY.to_string(),
        model_note,
    })
}

/// Dollars as the feed quotes them (a JSON number) into minor units.
///
/// Every step is checked, and the reasons differ. `Decimal::from_f64` rejects a non-finite
/// float, which is what a `NaN` or an `Infinity` in the JSON would decode to. `Decimal`'s `Mul`
/// then **panics** on overflow rather than wrapping (`checked_mul` is the non-panicking form),
/// and `Decimal::MAX` decodes from a JSON number perfectly happily before panicking on the
/// scale-up. `to_i64` finally catches what scales inside `Decimal` but still doesn't fit an
/// `i64` of cents. Same shape, and the same reasoning, as `sharesies::decimal_to_minor` — down
/// to sharing its `.round()`, which is banker's rounding rather than half-up.
fn dollars_to_minor(dollars: f64) -> Option<i64> {
    // `from_f64`, not `from_f64_retain`: it gives the shortest decimal that round-trips to the
    // same float, so 650000.0 scales to exactly 65000000 rather than to the binary expansion's
    // trailing noise. Same call, same reason, as `frankfurter::parse_quotes`.
    Decimal::from_f64(dollars)?
        .checked_mul(Decimal::from(MINOR_UNITS_PER_DOLLAR))
        .and_then(|scaled| scaled.round().to_i64())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down real response **with an invented address, id and coordinates**. The
    /// shape is real; nothing identifying in it is (CLAUDE.md rule 3). The address is the
    /// placeholder the web account form already suggests, and the id is a nil-ish UUID that
    /// could not be a real `unitOfPropertyId`.
    const MATCH_BODY: &str = r#"{
        "unitOfPropertyId": "00000000-0000-4000-8000-000000000001",
        "streetAddress": "123 kowhai street, riccarton",
        "grossSalePricePredictedModelA": 650000.0,
        "grossSalePricePredictedModelB": 598000.0,
        "buildingAge": "42",
        "landValue": 300000.0,
        "improvementValue": 350000.0,
        "suburb": "Riccarton",
        "bedroomCount": 4,
        "totalLinedFloorArea": 120.0
    }"#;

    #[test]
    fn takes_model_a_and_notes_both_models() {
        let body: MatchResponse = serde_json::from_str(MATCH_BODY).unwrap();
        let estimate = parse_estimate(body).unwrap();

        // The decision this integration is built around: model A is what gets recorded.
        assert_eq!(estimate.value_minor, 650_000_00);
        assert_eq!(estimate.currency_code, "NZD");
        assert_eq!(estimate.property_id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(estimate.matched_address, "123 kowhai street, riccarton");
        // Model B is recorded in the note, not in the figure — so the ~8% spread between two
        // undocumented models stays visible to whoever reads the valuation later.
        assert_eq!(estimate.model_note, "model A 650000, model B 598000");
    }

    #[test]
    fn decodes_the_body_bytes_the_capped_reader_accumulates() {
        // `crate::http::json_capped` ends in `serde_json::from_slice` over a buffer it built
        // chunk-by-chunk, not `Response::json`'s own decode — so the same payload has to
        // deserialise from raw bytes. ~1.5KB against an 8MiB ceiling.
        let body: MatchResponse = serde_json::from_slice(MATCH_BODY.as_bytes()).unwrap();
        assert_eq!(parse_estimate(body).unwrap().value_minor, 650_000_00);
    }

    #[test]
    fn ignores_the_forty_other_fields_including_the_identifying_ones() {
        // The real response carries a title boundary polygon, a GPS centroid and a legal
        // description. Nothing here deserialises them, and this pins that: the struct is
        // not `deny_unknown_fields`, so unknown keys are dropped rather than erroring, and a
        // future field cannot start being read by accident.
        let body: MatchResponse = serde_json::from_str(
            r#"{
                "unitOfPropertyId": "00000000-0000-4000-8000-000000000002",
                "streetAddress": "1 test place, addington",
                "grossSalePricePredictedModelA": 500000.0,
                "boundaryWkt": "POLYGON((0 0, 0 1, 1 1, 0 0))",
                "centroidWkt": "POINT(0 0)",
                "legalDescription": "Lot 1 DP 000000",
                "xCoord": 0.0,
                "yCoord": 0.0
            }"#,
        )
        .unwrap();
        let estimate = parse_estimate(body).unwrap();
        assert_eq!(estimate.value_minor, 500_000_00);
        // With no model B, the note says so rather than inventing a second figure.
        assert_eq!(estimate.model_note, "model A 500000");
    }

    #[test]
    fn a_match_with_no_model_a_price_is_an_error_not_a_miss() {
        // Distinct from a 404 on purpose: "the upstream doesn't know this address" is final and
        // the person should be told, whereas this is a shape we don't understand — retry next
        // month rather than overwrite a good figure or report "not found".
        let body: MatchResponse = serde_json::from_str(
            r#"{
                "unitOfPropertyId": "00000000-0000-4000-8000-000000000003",
                "streetAddress": "2 test place, addington",
                "grossSalePricePredictedModelB": 598000.0
            }"#,
        )
        .unwrap();
        let err = parse_estimate(body).unwrap_err().to_string();
        assert!(err.contains("no model-A predicted sale price"), "{err}");
    }

    #[test]
    fn converts_dollars_to_cents_without_float_drift() {
        assert_eq!(dollars_to_minor(650_000.0), Some(650_000_00));
        // A cents-bearing estimate: 0.1 and 0.2 are the classic non-representable pair, and
        // `from_f64`'s shortest-round-trip decimal is what keeps this off 649_999_99.
        assert_eq!(dollars_to_minor(650_000.10), Some(650_000_10));
        assert_eq!(dollars_to_minor(0.0), Some(0));
        // Exactly on the half-cent, `Decimal::round` is banker's rounding
        // (`MidpointNearestEven`) — so 100.5 cents goes to 100 and 101.5 goes to 102, rather
        // than both going up. Pinned because it is surprising, not because a whole-dollar feed
        // can reach it: the same `.round()` is what `sharesies::decimal_to_minor` uses on
        // amounts that *do* carry cents, and one rounding rule across the two is worth more
        // than half-up here.
        assert_eq!(dollars_to_minor(1.005), Some(1_00));
        assert_eq!(dollars_to_minor(1.015), Some(1_02));
    }

    #[test]
    fn refuses_a_price_that_cannot_be_cents_in_an_i64() {
        // Neither of these can reach the database as a plausible-looking small number. The
        // first two are what a `NaN`/`Infinity` in the JSON decodes to; the third scales past
        // `i64`, and the fourth used to *panic* inside `Decimal::mul` before `checked_mul`.
        assert_eq!(dollars_to_minor(f64::NAN), None);
        assert_eq!(dollars_to_minor(f64::INFINITY), None);
        assert_eq!(dollars_to_minor(1e30), None);
        assert_eq!(dollars_to_minor(f64::MAX), None);
    }
}
