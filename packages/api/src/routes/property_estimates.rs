//! The opt-in flow for third-party property estimates (House Pricer — see
//! `sure_providers::house_pricer`).
//!
//! Three routes, and the shape of them is the point:
//!
//! 1. `GET  /api/accounts/{id}/property-estimate/preview` — the **pre-flight**. Looks an address
//!    up and reports what matched, storing nothing and changing nothing.
//! 2. `POST /api/accounts/{id}/property-estimate` — **subscribe**. Repeats the lookup
//!    server-side, saves the link, and records the first valuation straight away.
//! 3. `DELETE /api/accounts/{id}/property-estimate` — **unsubscribe**.
//!
//! Nothing is polled until someone has been shown a specific match and confirmed it. The
//! subscription pins the upstream's own `unitOfPropertyId`, and that id is **always** taken from
//! a match this server just made, never from the request body: a client that could name its own
//! would defeat the drift guard the monthly poll depends on
//! (`sure_app::tasks::property_estimates`). It also means the confirm step cannot subscribe an
//! account to an address that no longer matches — the lookup has to succeed twice.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json as AxumJson, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sure_core::{AccountMetadata, HousePricerLink};
use utoipa::{IntoParams, ToSchema};

use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

// OTEL span names for this module's handlers.
const PROPERTY_ESTIMATE_PREVIEW: &str = "property_estimates.preview";
const PROPERTY_ESTIMATE_SUBSCRIBE: &str = "property_estimates.subscribe";
const PROPERTY_ESTIMATE_UNSUBSCRIBE: &str = "property_estimates.unsubscribe";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct LookupQuery {
    /// The address to look up. Defaults to the account's own stored address — so the common
    /// case is a button with nothing to type — and can be overridden when the stored one doesn't
    /// match (the upstream normalises to `"<street>, <suburb>"`, which is not always what a
    /// person typed into the address fields).
    pub q: Option<String>,
}

/// What a pre-flight found. Deliberately not the whole upstream response: the fields below are
/// what a person needs to decide "yes, that is my house", and every other field in that payload
/// is a detail of somebody's home that has no reason to cross this boundary.
#[derive(Debug, Serialize, ToSchema)]
pub struct EstimatePreview {
    /// The query that produced this match — echoed back because it is what gets stored, and the
    /// client may not have supplied it (it defaults to the account's address).
    pub query: String,
    /// The upstream's own normalised address for the match. **The field the UI must show**: it
    /// is how someone confirms the feed found their property and not one nearby.
    pub matched_address: String,
    /// The upstream's stable id for the matched property.
    pub property_id: String,
    /// The estimate, in minor units of `currency_code`.
    #[schema(value_type = i64)]
    pub value_minor: i64,
    pub currency_code: String,
    /// Which model produced `value_minor`, and what the other said — shown so the choice of
    /// model, and the spread between them, is visible before subscribing rather than only
    /// afterwards on the valuation.
    pub model_note: String,
    /// Which source answered (`"house_pricer"`).
    pub source: String,
    /// The area this source can answer for, for the UI to explain a miss with.
    pub coverage: String,
}

/// The area the configured source covers, so a `404` can be explained rather than just reported.
#[derive(Debug, Serialize, ToSchema)]
pub struct EstimateCoverage {
    pub source: String,
    pub coverage: String,
}

/// Build the address to look up: what the caller asked for, else the account's own address.
///
/// `"<line1>, <city>"` — the shape the upstream's own `streetAddress` comes back in, which is
/// what its matcher is happiest with. `address_line2` is deliberately left out: a unit number
/// belongs to a dwelling within a title, and including it turns a match into a miss.
fn address_of(metadata: &AccountMetadata) -> Option<String> {
    let AccountMetadata::Property(meta) = metadata else {
        return None;
    };
    let line1 = meta.address_line1.as_deref()?.trim();
    if line1.is_empty() {
        return None;
    }
    Some(match meta.city.as_deref().map(str::trim) {
        Some(city) if !city.is_empty() => format!("{line1}, {city}"),
        // No city stored: still worth asking, since the feed covers one city and a street name
        // alone is often unambiguous within it.
        Some(_) | None => line1.to_string(),
    })
}

/// Resolve the query for a request, or say which of the two ways to supply one is missing.
async fn resolve_query(st: &AppState, id: i64, q: Option<String>) -> AppResult<String> {
    let supplied = q.map(|q| q.trim().to_string()).filter(|q| !q.is_empty());
    if let Some(q) = supplied {
        return Ok(q);
    }
    let account = st.accounts.get(id).await?;
    address_of(&account.metadata).ok_or_else(|| {
        AppError::BadRequest(
            "this account has no street address to look up; add one or pass ?q=".into(),
        )
    })
}

/// Look up an address against the configured estimate source without storing anything.
///
/// The **pre-flight**: this is what makes the opt-in informed rather than a guess, so it exists
/// as its own route rather than as a side effect of subscribing. A `404` is the ordinary answer
/// for a property the source doesn't cover, and its body names the coverage area so the UI can
/// say why.
#[utoipa::path(get, path = "/api/accounts/{id}/property-estimate/preview", tag = "accounts",
    params(("id" = i64, Path,), LookupQuery),
    responses((status = 200, body = EstimatePreview), (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROPERTY_ESTIMATE_PREVIEW,
    level = "debug",
    skip_all,
    // The address itself is deliberately not a field here: it is personal data, and this span is
    // on by default at debug. The account id is enough to correlate.
    fields(account_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn preview(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<LookupQuery>,
) -> AppResult<Json<EstimatePreview>> {
    let query = resolve_query(&st, id, params.q).await?;
    lookup(&st, &query)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound("matching property"))
}

/// One lookup, mapped onto this layer's error vocabulary.
///
/// `AppError::upstream` rather than a bare `?`: a plain `anyhow` conversion becomes
/// `Internal`, i.e. `500`, which tells a client "a bug here, it will fail again" when what
/// happened is a third party having a bad minute. Same reasoning, and the same call, as
/// `sure_app::stock_prices::price_at`.
async fn lookup(st: &AppState, query: &str) -> AppResult<Option<EstimatePreview>> {
    let provider = st.property_estimate_provider.as_ref();
    let found = provider
        .fetch_estimate(query)
        .await
        .map_err(|err| AppError::upstream(&err))?;
    Ok(found.map(|estimate| EstimatePreview {
        query: query.to_string(),
        matched_address: estimate.matched_address,
        property_id: estimate.property_id,
        value_minor: estimate.value_minor,
        currency_code: estimate.currency_code,
        model_note: estimate.model_note,
        source: provider.kind().to_string(),
        coverage: provider.coverage().to_string(),
    }))
}

/// Subscribe this account to monthly estimates, and record one immediately.
///
/// The lookup runs again here rather than trusting what the preview returned, which is what makes
/// the stored `property_id` trustworthy: it is a match this server made, so the poll's drift
/// check compares against something real. The immediate valuation means the feature does
/// something visible now instead of in thirty days.
#[utoipa::path(post, path = "/api/accounts/{id}/property-estimate", tag = "accounts",
    params(("id" = i64, Path,), LookupQuery),
    responses((status = 200, body = sure_core::Account), (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROPERTY_ESTIMATE_SUBSCRIBE,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn subscribe(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<LookupQuery>,
) -> AppResult<Json<sure_core::Account>> {
    let account = st.accounts.get(id).await?;
    let query = match params
        .q
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty())
    {
        Some(q) => q,
        None => address_of(&account.metadata).ok_or_else(|| {
            AppError::BadRequest(
                "this account has no street address to look up; add one or pass ?q=".into(),
            )
        })?,
    };

    let found = lookup(&st, &query)
        .await?
        .ok_or(AppError::NotFound("matching property"))?;

    // Checked before subscribing, not merely at poll time: the poll would refuse this every
    // month and log it where nobody is looking, whereas here the person is waiting for an answer
    // and can be told. Same reasoning as the poll's own currency guard — there is no FX in reach,
    // so the alternative is recording an NZD figure against another currency at parity.
    if !found
        .currency_code
        .eq_ignore_ascii_case(&account.currency_code)
    {
        return Err(AppError::Validation(format!(
            "{} quotes estimates in {}, but this account is denominated in {}",
            found.source, found.currency_code, account.currency_code
        )));
    }

    let account = st
        .accounts
        .set_house_pricer_link(
            id,
            Some(HousePricerLink {
                query: found.query.clone(),
                property_id: found.property_id.clone(),
                matched_address: found.matched_address.clone(),
            }),
        )
        .await?;

    // The first estimate, now. Uses the same note format the monthly poll writes, so the series
    // reads consistently rather than having a differently-shaped first row.
    //
    // `Utc::now()` directly, as `routes::stock_prices` does: the `Clock` port exists so the
    // *scheduled* paths are deterministic in tests, and this layer has no injected clock to
    // reach for. The date only decides which day's row is upserted.
    st.valuations
        .upsert_from_estimate(
            id,
            &Utc::now().date_naive().to_string(),
            found.value_minor,
            &found.currency_code,
            &format!(
                "{} ({})",
                sure_app::tasks::property_estimates::NOTE_PREFIX,
                found.model_note
            ),
        )
        .await?;

    Ok(Json(account))
}

/// Stop polling this account. Idempotent, and leaves every estimate already recorded in place —
/// they are history, not a live subscription, and deleting someone's valuation series as a side
/// effect of turning a feed off would be a surprise.
#[utoipa::path(delete, path = "/api/accounts/{id}/property-estimate", tag = "accounts",
    params(("id" = i64, Path,)),
    responses((status = 200, body = sure_core::Account), (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROPERTY_ESTIMATE_UNSUBSCRIBE,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn unsubscribe(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<sure_core::Account>> {
    st.accounts.set_house_pricer_link(id, None).await.map(Json)
}

/// Which source is configured and what it covers — so the UI can label the button and explain a
/// miss without hardcoding "Christchurch" in the web layer.
#[utoipa::path(get, path = "/api/property-estimate-source", tag = "accounts",
    responses((status = 200, body = EstimateCoverage)))]
pub async fn source(State(st): State<AppState>) -> AxumJson<EstimateCoverage> {
    let provider = st.property_estimate_provider.as_ref();
    AxumJson(EstimateCoverage {
        source: provider.kind().to_string(),
        coverage: provider.coverage().to_string(),
    })
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/property-estimate/preview", get(preview))
        .route(
            "/accounts/{id}/property-estimate",
            post(subscribe).delete(unsubscribe),
        )
        .route("/property-estimate-source", get(source))
}

#[cfg(test)]
mod tests {
    use sure_core::PropertyMeta;

    use super::*;

    fn property(line1: Option<&str>, city: Option<&str>) -> AccountMetadata {
        AccountMetadata::Property(PropertyMeta {
            address_line1: line1.map(Into::into),
            city: city.map(Into::into),
            ..Default::default()
        })
    }

    #[test]
    fn builds_the_query_in_the_shape_the_upstream_answers_in() {
        // `"<street>, <city>"` — the form `streetAddress` comes back in.
        assert_eq!(
            address_of(&property(Some("123 Kowhai Street"), Some("Riccarton"))).as_deref(),
            Some("123 Kowhai Street, Riccarton")
        );
    }

    #[test]
    fn falls_back_to_the_street_alone_when_there_is_no_city() {
        for city in [None, Some(""), Some("   ")] {
            assert_eq!(
                address_of(&property(Some("123 Kowhai Street"), city)).as_deref(),
                Some("123 Kowhai Street"),
                "{city:?}"
            );
        }
    }

    #[test]
    fn trims_what_a_form_left_behind() {
        assert_eq!(
            address_of(&property(Some("  123 Kowhai Street "), Some(" Riccarton "))).as_deref(),
            Some("123 Kowhai Street, Riccarton")
        );
    }

    #[test]
    fn has_nothing_to_look_up_without_a_street() {
        // Each becomes a 400 telling the caller to add an address or pass `?q=`, rather than an
        // empty query the upstream would 400 on for less obvious reasons.
        for meta in [
            property(None, Some("Riccarton")),
            property(Some("  "), None),
        ] {
            assert_eq!(address_of(&meta), None);
        }
    }

    #[test]
    fn a_non_property_account_has_no_address_to_look_up() {
        // The DAL refuses to subscribe one too (`set_house_pricer_link`); this is the earlier of
        // the two gates, and the one that produces the better message.
        assert_eq!(
            address_of(&AccountMetadata::Depository(Default::default())),
            None
        );
    }
}
