use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::error::AppResult;
use crate::state::AppState;

// Account kinds/classes + typed metadata come from sure-core; the data model lives there
// too. Re-export so the OpenAPI paths (`crate::routes::accounts::*`) resolve.
pub use sure_core::{
    Account, AccountClass, AccountKind, AccountMetadata, AreaUnit, BrokerageMeta, CryptoMeta,
    DepositoryMeta, GenericMeta, LoanMeta, MileageUnit, MortgageMeta, PropertyMeta, RateType,
    RepaymentFrequency, SaveAccount, SetSecuredBy, SharesMeta, TaxTreatment, VehicleMeta,
};

// OTEL span names for this module's handlers.
const ACCOUNTS_LIST: &str = "accounts.list";
const ACCOUNTS_GET: &str = "accounts.get";
const ACCOUNTS_CREATE: &str = "accounts.create";
const ACCOUNTS_UPDATE: &str = "accounts.update";
const ACCOUNTS_DELETE: &str = "accounts.delete";
const ACCOUNTS_SET_SECURED_BY: &str = "accounts.set_secured_by";

/// Query params for `GET /accounts`.
#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub include_archived: Option<bool>,
}

/// List accounts (optionally including archived).
#[utoipa::path(get, path = "/api/accounts", tag = "accounts",
    params(("include_archived" = Option<bool>, Query,)),
    responses((status = 200, body = [Account])))]
#[tracing::instrument(
    name = ACCOUNTS_LIST,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(
    State(st): State<AppState>,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Vec<Account>>> {
    Ok(Json(
        st.accounts
            .list(q.include_archived.unwrap_or(false))
            .await?,
    ))
}

/// Fetch one account.
#[utoipa::path(get, path = "/api/accounts/{id}", tag = "accounts",
    params(("id" = i64, Path,)),
    responses((status = 200, body = Account), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ACCOUNTS_GET,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn get_one(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<Json<Account>> {
    Ok(Json(st.accounts.get(id).await?))
}

/// Create an account.
#[utoipa::path(post, path = "/api/accounts", tag = "accounts",
    request_body = SaveAccount,
    responses((status = 201, body = Account), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ACCOUNTS_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveAccount>,
) -> AppResult<(StatusCode, Json<Account>)> {
    Ok((StatusCode::CREATED, Json(st.accounts.create(input).await?)))
}

/// Replace an account.
#[utoipa::path(put, path = "/api/accounts/{id}", tag = "accounts",
    params(("id" = i64, Path,)), request_body = SaveAccount,
    responses((status = 200, body = Account), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ACCOUNTS_UPDATE,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveAccount>,
) -> AppResult<Json<Account>> {
    Ok(Json(st.accounts.update(id, input).await?))
}

/// Delete an account and its transactions/valuations (cascade). Refused with 409 while
/// debts are secured against it (an asset acts as their parent).
#[utoipa::path(delete, path = "/api/accounts/{id}", tag = "accounts",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ACCOUNTS_DELETE,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.accounts.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Link a liability to the asset securing it (e.g. a mortgage to a house), or unlink
/// with `null`.
#[utoipa::path(put, path = "/api/accounts/{id}/secured-by", tag = "accounts",
    params(("id" = i64, Path,)), request_body = SetSecuredBy,
    responses((status = 200, body = Account), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ACCOUNTS_SET_SECURED_BY,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn set_secured_by(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SetSecuredBy>,
) -> AppResult<Json<Account>> {
    Ok(Json(
        st.accounts
            .set_secured_by(id, input.secured_by_account_id)
            .await?,
    ))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts", get(list).post(create))
        .route("/accounts/{id}", get(get_one).put(update).delete(delete))
        .route(
            "/accounts/{id}/secured-by",
            axum::routing::put(set_secured_by),
        )
}
