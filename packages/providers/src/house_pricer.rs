//! [`PropertyEstimateProvider`] backed by House Pricer (<https://www.housepricer.co.nz>) — a
//! free, keyless automated valuation model for **Christchurch, New Zealand**, built on
//! Christchurch City Council sale/valuation records and LINZ property data. Same standing
//! caveat as Frankfurter and Yahoo: the endpoint is undocumented and could change without
//! notice.
//!
//! **The wire format is not here.** `house-pricer-client` owns the URL shape, the query
//! encoding, the status codes and the JSON contract; this file owns what Sure does with the
//! answer. The split is what makes an undocumented endpoint tolerable: a field rename is a
//! one-line change in that crate, which cannot name an account or a valuation, so nothing in
//! this workspace's domain logic recompiles its idea of anything. What stays here is the pair
//! of judgements that are Sure's and not the upstream's — **which of the two models to record**,
//! and **how dollars become minor units** — plus the client policy (`Endpoint`, the shared
//! bounded `reqwest::Client`, one body ceiling for every adapter).
//!
//! **Nothing recorded from this host may be committed.** The response is not market data: it is
//! a dossier on one dwelling — street address, GPS centroid, title boundary polygon, legal
//! description, land and improvement values. That is personal data about wherever the person
//! running Sure lives, so it is treated like Akahu's traffic rather than like Frankfurter's:
//! `scripts/pii-scan.mjs` refuses a `house_pricer` recording by path *and* by content, and the
//! fixtures below are hand-authored with an invented address (CLAUDE.md rule 3).

use async_trait::async_trait;
use house_pricer_client::{HousePricerClient, HousePricerError, PropertyMatch};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

use sure_app::ports::{PropertyEstimate, PropertyEstimateProvider};

use crate::http::Endpoint;

/// The real endpoint. `pub` because the composition root owns the decision of where this
/// provider points (it is the only place configuration is read) and needs a default to fall
/// back to.
///
/// Re-exported from the client rather than restated, so the two cannot drift: this const and
/// the one the client would use by default are the same string by construction.
pub const DEFAULT_BASE_URL: &str = house_pricer_client::DEFAULT_BASE_URL;

/// The only currency this feed quotes. Not configurable and not read from the response, which
/// carries no currency field at all: the source is one New Zealand city's council and LINZ
/// records, and every figure in it is NZD. Named rather than inlined so the assumption is
/// searchable — and the poll checks the subscribed account against it rather than booking a
/// foreign-currency property at parity.
///
/// Sure's, not the client's: "what currency is this feed in?" is a question about how to record
/// the number, which is exactly the sort of thing the wire crate is kept ignorant of.
pub const QUOTE_CURRENCY: &str = "NZD";

/// Minor units per major unit for [`QUOTE_CURRENCY`]. NZD is a 2-decimal currency; a hardcoded
/// 100 is correct for exactly as long as this feed is NZD-only, which is as long as it is
/// Christchurch-only.
const MINOR_UNITS_PER_DOLLAR: i64 = 100;

pub struct HousePricerProvider {
    client: HousePricerClient,
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
    ///
    /// The `reqwest::Client` is built here and handed to the wire crate, exactly as
    /// `AkahuProvider` does with `akahu-client`: whether a plaintext request is refused, and
    /// what the timeouts are, are properties of the client that `Endpoint` decides — not
    /// something a crate that only knows a JSON shape should have an opinion about. The body
    /// ceiling comes from the same place for the same reason, so this process has one answer to
    /// "how much of a response may we buffer?" rather than one per upstream.
    pub fn with_endpoint(endpoint: Endpoint) -> Self {
        let client = HousePricerClient::new(crate::http::client(&endpoint), endpoint.url())
            .with_max_response_bytes(crate::http::MAX_BODY_BYTES);
        Self { client }
    }
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
        match self.client.match_address(query).await {
            Ok(property) => parse_estimate(property).map(Some),
            // No match is the ordinary answer, not a failure: House Pricer covers one city, so
            // every address outside it answers this way, as does one with a typo. The caller —
            // a pre-flight the person is watching, or the monthly poll — decides what to do
            // with "nothing".
            Err(HousePricerError::NotFound) => {
                tracing::debug!("no property matched the address given");
                Ok(None)
            }
            // CLAUDE.md rule 2's escape hatch: `HousePricerError` is `#[non_exhaustive]`, so a
            // catch-all is the only option — and it is the right answer anyway, because every
            // remaining variant means the same thing to this caller (the estimate could not be
            // fetched) and differs only in the message. `NotFound` above is the one that
            // changes behaviour, and it is named.
            Err(other) => Err(anyhow::Error::new(other)),
        }
    }
}

/// Turn a match into the one estimate Sure records, or fail saying which field was unusable.
///
/// This is the half that is genuinely Sure's: the upstream hands over two undocumented model
/// outputs and this decides which one becomes a valuation, in what units, with what note. Split
/// from the fetch so the whole mapping is testable without a socket — and now, with the wire
/// format gone to its own crate, without a JSON literal either.
fn parse_estimate(property: PropertyMatch) -> anyhow::Result<PropertyEstimate> {
    use anyhow::Context as _;

    // A match with no model-A figure is a match this adapter cannot use. It is an error rather
    // than `Ok(None)` because the two mean different things to the caller: `None` is "the
    // upstream doesn't know this address", which is final and worth telling the person, whereas
    // this is "the upstream knows it but answered in a shape we don't understand" — worth a
    // warning and a retry next month, and worth not overwriting last month's good figure with.
    let model_a = property
        .gross_sale_price_predicted_model_a
        .context("the match carried no model-A predicted sale price")?;

    let value_minor = dollars_to_minor(model_a)
        .with_context(|| format!("model-A predicted sale price {model_a} is out of range"))?;

    // Both models, in the note, so the recorded figure is self-describing: a valuation of
    // $705,500 with no provenance is indistinguishable from one somebody typed. `{:.0}` because
    // the feed quotes whole dollars, and a note is for reading.
    let model_note = match property.gross_sale_price_predicted_model_b {
        Some(model_b) => format!("model A {model_a:.0}, model B {model_b:.0}"),
        None => format!("model A {model_a:.0}"),
    };

    Ok(PropertyEstimate {
        property_id: property.unit_of_property_id,
        matched_address: property.street_address,
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

    /// A match with an **invented address, id and figures** (CLAUDE.md rule 3). The address is
    /// the placeholder the web account form already suggests, and the id is a nil-ish UUID that
    /// could not be a real `unitOfPropertyId`.
    ///
    /// Built as a value rather than parsed from JSON, which is the visible dividend of the
    /// split: what these tests are about is the model choice and the minor-unit conversion, and
    /// they no longer restate a wire format to get at them. Whether the JSON *parses* — the
    /// camelCase spelling, the ignored forty-odd other fields — is `house-pricer-client`'s own
    /// test, next to the struct that would have to change.
    fn matched(model_a: Option<f64>, model_b: Option<f64>) -> PropertyMatch {
        // `..` on a `#[non_exhaustive]` struct from another crate is not allowed, so this is
        // written out; it is also the only place in this crate that constructs one.
        serde_json::from_value(serde_json::json!({
            "unitOfPropertyId": "00000000-0000-4000-8000-000000000001",
            "streetAddress": "123 kowhai street, riccarton",
            "grossSalePricePredictedModelA": model_a,
            "grossSalePricePredictedModelB": model_b,
        }))
        .expect("the fixture matches the client's own shape")
    }

    #[test]
    fn takes_model_a_and_notes_both_models() {
        let estimate = parse_estimate(matched(Some(650_000.0), Some(598_000.0))).unwrap();

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
    fn with_no_model_b_the_note_says_so_rather_than_inventing_one() {
        let estimate = parse_estimate(matched(Some(500_000.0), None)).unwrap();
        assert_eq!(estimate.value_minor, 500_000_00);
        assert_eq!(estimate.model_note, "model A 500000");
    }

    #[test]
    fn a_match_with_no_model_a_price_is_an_error_not_a_miss() {
        // Distinct from a 404 on purpose: "the upstream doesn't know this address" is final and
        // the person should be told, whereas this is a shape we don't understand — retry next
        // month rather than overwrite a good figure or report "not found".
        let err = parse_estimate(matched(None, Some(598_000.0)))
            .unwrap_err()
            .to_string();
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

    /// The one const that could silently drift now that the URL lives in another crate.
    #[test]
    fn the_default_endpoint_is_the_clients_own() {
        assert_eq!(DEFAULT_BASE_URL, house_pricer_client::DEFAULT_BASE_URL);
    }
}
