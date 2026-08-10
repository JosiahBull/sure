use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;

use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

use sure_core::ValuationQuery;
pub use sure_core::{NewValuation, Valuation, ValuationSource};

/// The wire twin of [`ValuationQuery`]. Its one job is turning `?source=` from text into the
/// domain enum at the edge — the query string is the one legal place a domain value is a
/// string (CLAUDE.md rule 1) — so an unrecognised value is a 400 rather than a filter that
/// silently matches everything and reports a series nobody asked for.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
pub struct ValuationQueryParams {
    /// `manual` | `cron` | `provider` | `brokerage` | `equity`.
    pub source: Option<String>,
    /// At most this many, newest first.
    pub limit: Option<i64>,
}

impl TryFrom<ValuationQueryParams> for ValuationQuery {
    type Error = AppError;

    fn try_from(p: ValuationQueryParams) -> Result<Self, Self::Error> {
        let source = match p.source.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => Some(
                s.parse::<ValuationSource>()
                    .map_err(|e| AppError::validation(format!("invalid source: {e}")))?,
            ),
            None => None,
        };
        Ok(ValuationQuery {
            source,
            limit: p.limit,
        })
    }
}

// OTEL span names for this module's handlers.
const VALUATIONS_LIST: &str = "valuations.list";
const VALUATIONS_CREATE: &str = "valuations.create";
const VALUATIONS_UPDATE: &str = "valuations.update";
const VALUATIONS_DELETE: &str = "valuations.delete";

/// List an account's valuations, newest first, optionally narrowed by source.
///
/// The narrowing is not a nicety: a provider- or brokerage-linked account gains one row per
/// day forever, so a client after the handful someone entered by hand would otherwise download
/// thousands to find them.
#[utoipa::path(get, path = "/api/accounts/{id}/valuations", tag = "valuations",
    params(("id" = i64, Path,), ValuationQueryParams),
    responses((status = 200, body = [Valuation]),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = VALUATIONS_LIST,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<ValuationQueryParams>,
) -> AppResult<Json<Vec<Valuation>>> {
    let q: ValuationQuery = params.try_into()?;
    Ok(Json(st.valuations.list_for_account(id, q).await?))
}

/// Record a valuation for an account.
#[utoipa::path(post, path = "/api/accounts/{id}/valuations", tag = "valuations",
    params(("id" = i64, Path,)), request_body = NewValuation,
    responses((status = 201, body = Valuation), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = VALUATIONS_CREATE,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<NewValuation>,
) -> AppResult<(StatusCode, Json<Valuation>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.valuations.create(id, input).await?),
    ))
}

/// Edit a valuation entered by hand — its date, amount or note.
///
/// Manual only: the other sources are derived and would be recomputed over, so editing one is
/// refused rather than silently undone by the next sync.
#[utoipa::path(put, path = "/api/valuations/{id}", tag = "valuations",
    params(("id" = i64, Path,)), request_body = NewValuation,
    responses((status = 200, body = Valuation), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = VALUATIONS_UPDATE,
    level = "debug",
    skip_all,
    fields(valuation_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<NewValuation>,
) -> AppResult<Json<Valuation>> {
    Ok(Json(st.valuations.update(id, input).await?))
}

/// Delete a valuation.
#[utoipa::path(delete, path = "/api/valuations/{id}", tag = "valuations",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = VALUATIONS_DELETE,
    level = "debug",
    skip_all,
    fields(valuation_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.valuations.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{id}/valuations", get(list).post(create))
        .route(
            "/valuations/{id}",
            axum::routing::put(update).delete(delete),
        )
}
