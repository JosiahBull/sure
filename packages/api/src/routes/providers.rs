use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::{AppError, AppResult};
use crate::providers::{ProviderKind, Registry, SyncContext};
use crate::state::AppState;

pub use sure_dal::providers::{Provider, ProviderSync, SaveProvider, SyncRequest};

/// The provider kinds this server supports.
#[utoipa::path(get, path = "/api/provider-kinds", tag = "providers",
    responses((status = 200, body = [ProviderKind])))]
pub async fn kinds() -> Json<Vec<ProviderKind>> {
    Json(Registry::new().kinds())
}

#[utoipa::path(get, path = "/api/providers", tag = "providers", responses((status = 200, body = [Provider])))]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Provider>>> {
    Ok(Json(sure_dal::providers::list(&st.db).await?))
}

#[utoipa::path(post, path = "/api/providers", tag = "providers", request_body = SaveProvider,
    responses((status = 201, body = Provider), (status = 422, body = crate::error::ErrorBody)))]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveProvider>,
) -> AppResult<(StatusCode, Json<Provider>)> {
    if Registry::new().get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    Ok((
        StatusCode::CREATED,
        Json(sure_dal::providers::create(&st.db, input).await?),
    ))
}

#[utoipa::path(put, path = "/api/providers/{id}", tag = "providers", params(("id" = i64, Path,)),
    request_body = SaveProvider,
    responses((status = 200, body = Provider), (status = 404, body = crate::error::ErrorBody)))]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveProvider>,
) -> AppResult<Json<Provider>> {
    if Registry::new().get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    Ok(Json(sure_dal::providers::update(&st.db, id, input).await?))
}

#[utoipa::path(delete, path = "/api/providers/{id}", tag = "providers", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    sure_dal::providers::delete(&st.db, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Fetch (the provider trait) is orchestrated here; all persistence lives in the DAL.
/// Sync a provider: fetch upstream transactions, import new ones (dedupe on
/// external id), and record the result.
#[utoipa::path(post, path = "/api/providers/{id}/sync", tag = "providers", params(("id" = i64, Path,)),
    request_body = SyncRequest,
    responses((status = 200, body = ProviderSync), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
pub async fn sync(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SyncRequest>,
) -> AppResult<Json<ProviderSync>> {
    let provider = sure_dal::providers::get(&st.db, id).await?;
    let account_ccy = sure_dal::providers::account_currency(&st.db, provider.account_id).await?;

    let registry = Registry::new();
    let p = registry.get(&provider.kind).ok_or_else(|| {
        AppError::validation(format!("unknown provider kind '{}'", provider.kind))
    })?;
    let ctx = SyncContext {
        config: &provider.config,
        account_currency: &account_ccy,
        payload: req.payload.as_deref(),
    };

    let fetched = match p.fetch(ctx).await {
        Ok(txns) => txns,
        Err(e) => {
            let _ =
                sure_dal::providers::record_sync(&st.db, id, 0, 0, "error", Some(&e.to_string()))
                    .await?;
            return Err(AppError::validation(format!("sync failed: {e}")));
        }
    };

    let provider_tag = format!("{}#{}", provider.kind, id);
    let rows: Vec<sure_dal::providers::ImportRow> = fetched
        .into_iter()
        .map(|t| sure_dal::providers::ImportRow {
            external_id: t.external_id,
            posted_at: t.posted_at,
            amount_minor: t.amount_minor,
            currency_code: t.currency_code,
            description: t.description,
            merchant: t.merchant,
        })
        .collect();

    let (imported, skipped) = sure_dal::providers::import_transactions(
        &st.db,
        provider.account_id,
        &account_ccy,
        &provider_tag,
        &rows,
    )
    .await?;
    sure_dal::providers::update_last_synced(&st.db, id).await?;
    Ok(Json(
        sure_dal::providers::record_sync(&st.db, id, imported, skipped, "ok", None).await?,
    ))
}

#[utoipa::path(get, path = "/api/providers/{id}/syncs", tag = "providers", params(("id" = i64, Path,)),
    responses((status = 200, body = [ProviderSync])))]
pub async fn list_syncs(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<ProviderSync>>> {
    Ok(Json(sure_dal::providers::list_syncs(&st.db, id).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/provider-kinds", get(kinds))
        .route("/providers", get(list).post(create))
        .route("/providers/{id}", axum::routing::put(update).delete(delete))
        .route("/providers/{id}/sync", post(sync))
        .route("/providers/{id}/syncs", get(list_syncs))
}
