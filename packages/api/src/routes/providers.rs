use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::error::{AppError, AppResult};
use crate::providers::{ProviderAccount, ProviderKind, Registry, SyncContext};
use crate::state::AppState;
use sure_dal::Db;

pub use sure_dal::providers::{LinkProviderAccount, Provider, ProviderSync, SaveProvider, SyncRequest};

/// The provider kinds this server supports.
#[utoipa::path(get, path = "/api/provider-kinds", tag = "providers",
    responses((status = 200, body = [ProviderKind])))]
pub async fn kinds() -> Json<Vec<ProviderKind>> {
    Json(Registry::new().kinds())
}

/// Upstream accounts a discovery-capable provider kind can see, excluding any already
/// linked to a local account.
#[utoipa::path(get, path = "/api/provider-kinds/{kind}/accounts", tag = "providers",
    params(("kind" = String, Path,)),
    responses((status = 200, body = [ProviderAccount]), (status = 422, body = crate::error::ErrorBody)))]
pub async fn discover_accounts(
    State(st): State<AppState>,
    Path(kind): Path<String>,
) -> AppResult<Json<Vec<ProviderAccount>>> {
    let registry = Registry::new();
    let provider = registry
        .get(&kind)
        .ok_or_else(|| AppError::validation(format!("unknown provider kind '{kind}'")))?;

    let discovered = provider
        .list_accounts()
        .await
        .map_err(|e| AppError::validation(e.to_string()))?;

    let already_linked: HashSet<String> = sure_dal::providers::list(&st.db)
        .await?
        .into_iter()
        .filter(|p| p.kind == kind)
        .filter_map(|p| {
            p.config
                .get("external_account_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    Ok(Json(
        discovered
            .into_iter()
            .filter(|a| !already_linked.contains(&a.external_id))
            .collect(),
    ))
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

/// Link an upstream account (from [`discover_accounts`]) to a local account, creating it
/// first if `new_account` is given rather than `existing_account_id`. Triggers an
/// immediate best-effort sync so the account isn't empty until the next scheduled poll.
#[utoipa::path(post, path = "/api/providers/link", tag = "providers", request_body = LinkProviderAccount,
    responses((status = 201, body = Provider), (status = 422, body = crate::error::ErrorBody)))]
pub async fn link(
    State(st): State<AppState>,
    Json(input): Json<LinkProviderAccount>,
) -> AppResult<(StatusCode, Json<Provider>)> {
    if Registry::new().get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    let provider = sure_dal::providers::link(&st.db, input).await?;

    // Best-effort: a failed initial sync (e.g. not-yet-configured credentials) is already
    // durably recorded as an "error" sync row and doesn't undo the link — the user can
    // retry via "Sync now" once fixed.
    let id = provider.id;
    if let Err(e) = sync_provider(&st.db, provider, None).await {
        tracing::warn!(provider_id = id, error = %e, "initial sync after linking failed");
    }
    let provider = sure_dal::providers::get(&st.db, id).await?;

    Ok((StatusCode::CREATED, Json(provider)))
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

/// Fetch upstream transactions for one provider, import the new ones (dedupe on external
/// id), and durably record the outcome — shared by the manual sync route below and the
/// background [`crate::provider_poll::ProviderPollTask`]. A fetch failure is still
/// recorded (as an "error" sync row) before the error is propagated.
pub(crate) async fn sync_provider(
    db: &Db,
    provider: Provider,
    payload: Option<&str>,
) -> AppResult<ProviderSync> {
    let id = provider.id;
    let account_ccy = sure_dal::providers::account_currency(db, provider.account_id).await?;

    let registry = Registry::new();
    let p = registry.get(&provider.kind).ok_or_else(|| {
        AppError::validation(format!("unknown provider kind '{}'", provider.kind))
    })?;
    let ctx = SyncContext {
        config: &provider.config,
        account_currency: &account_ccy,
        payload,
        last_synced_at: provider.last_synced_at.as_deref(),
    };

    let fetched = match p.fetch(ctx).await {
        Ok(txns) => txns,
        Err(e) => {
            let _ = sure_dal::providers::record_sync(db, id, 0, 0, "error", Some(&e.to_string()))
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
            category_name: t.category.as_ref().map(|c| c.name.clone()),
            category_group: t.category.and_then(|c| c.group),
        })
        .collect();

    let (imported, skipped) = sure_dal::providers::import_transactions(
        db,
        provider.account_id,
        &account_ccy,
        &provider_tag,
        &rows,
    )
    .await?;

    // Best-effort: a provider's transaction history often doesn't reach back to when the
    // account was opened (a mortgage's full term, say), so the imported transactions alone
    // would leave the displayed balance drifting from reality. Refresh a same-day
    // provider-sourced valuation from the upstream's live balance where the provider can
    // report one, so the balance stays accurate regardless of transaction completeness.
    // Never lets a balance-fetch problem fail the sync — the transaction import already
    // succeeded, which is the part `status`/`imported`/`skipped` below describe.
    match p.current_balance(ctx).await {
        Ok(Some(bal)) => {
            let today = chrono::Utc::now().date_naive().to_string();
            if let Err(e) = sure_dal::valuations::upsert_from_provider(
                db,
                provider.account_id,
                &today,
                bal.minor,
                &bal.currency_code,
            )
            .await
            {
                tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider balance valuation");
            }
            // Also best-effort: lets a credit_card/revolving_credit account show
            // "remaining borrowing" (the web UI computes limit minus what's owed). A
            // no-op for any account kind with no such concept.
            if let Some(limit_minor) = bal.limit_minor {
                if let Err(e) =
                    sure_dal::accounts::set_credit_limit(db, provider.account_id, limit_minor).await
                {
                    tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider credit limit");
                }
            }
            // Same idea for a mortgage/loan's original borrowed amount, so the web UI
            // can show how much of it has been paid down. A no-op for any other kind.
            if let Some(initial_principal_minor) = bal.initial_principal_minor {
                if let Err(e) = sure_dal::accounts::set_original_amount(
                    db,
                    provider.account_id,
                    initial_principal_minor,
                )
                .await
                {
                    tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider original loan amount");
                }
            }
            // Backfill the institution name too, but only if unset — see
            // `set_institution_if_unset`'s doc comment for why this one never overwrites.
            if let Some(institution) = bal.institution {
                if let Err(e) =
                    sure_dal::accounts::set_institution_if_unset(db, provider.account_id, &institution)
                        .await
                {
                    tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider institution");
                }
            }
        }
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(provider_id = id, error = %e, "could not fetch provider balance");
        }
    }

    sure_dal::providers::update_last_synced(db, id).await?;
    sure_dal::providers::record_sync(db, id, imported, skipped, "ok", None).await
}

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
    Ok(Json(
        sync_provider(&st.db, provider, req.payload.as_deref()).await?,
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
        .route("/provider-kinds/{kind}/accounts", get(discover_accounts))
        .route("/providers", get(list).post(create))
        .route("/providers/link", post(link))
        .route("/providers/{id}", axum::routing::put(update).delete(delete))
        .route("/providers/{id}/sync", post(sync))
        .route("/providers/{id}/syncs", get(list_syncs))
}
