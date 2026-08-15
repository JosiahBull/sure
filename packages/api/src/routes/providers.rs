use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};

pub use sure_core::{
    LinkGroupMember, LinkProviderAccount, LinkProviderGroup, Ownership, Provider, ProviderAccount,
    ProviderKind, ProviderSync, SaveAccount, SaveProvider, SyncOutcome, SyncReport, SyncRequest,
};

// OTEL span names for this module's handlers.
const PROVIDERS_KINDS: &str = "providers.kinds";
const PROVIDERS_DISCOVER_ACCOUNTS: &str = "providers.discover_accounts";
const PROVIDERS_LIST: &str = "providers.list";
const PROVIDERS_CREATE: &str = "providers.create";
const PROVIDERS_LINK: &str = "providers.link";
const PROVIDERS_LINK_GROUP: &str = "providers.link_group";
const PROVIDERS_UPDATE: &str = "providers.update";
const PROVIDERS_DELETE: &str = "providers.delete";
const PROVIDERS_SYNC: &str = "providers.sync";
const PROVIDERS_LIST_SYNCS: &str = "providers.list_syncs";

/// The provider kinds this server supports.
#[utoipa::path(get, path = "/api/provider-kinds", tag = "providers",
    responses((status = 200, body = [ProviderKind])))]
#[tracing::instrument(
    name = PROVIDERS_KINDS,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
)]
pub async fn kinds(State(st): State<AppState>) -> Json<Vec<ProviderKind>> {
    Json(st.provider_registry.kinds())
}

/// Upstream accounts a discovery-capable provider kind can see, excluding any already
/// linked to a local account — including one linked through a *different* login, which is
/// how a joint account arrives twice. Each remaining account carries
/// [`ProviderAccount::joint`], so the dialog can say why it is about to force joint
/// ownership on it.
///
/// The filtering and the flag both come from `sure_app`'s `survey_accounts`, which
/// `link` also consults before accepting anything — one definition, so what this hides and
/// what that refuses cannot drift apart.
#[utoipa::path(get, path = "/api/provider-kinds/{kind}/accounts", tag = "providers",
    params(("kind" = String, Path,)),
    responses((status = 200, body = [ProviderAccount]), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_DISCOVER_ACCOUNTS,
    level = "debug",
    skip_all,
    fields(provider_kind = %kind),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn discover_accounts(
    State(st): State<AppState>,
    Path(kind): Path<String>,
) -> AppResult<Json<Vec<ProviderAccount>>> {
    Ok(Json(st.sync.linkable_accounts(&kind).await?))
}

#[utoipa::path(get, path = "/api/providers", tag = "providers", responses((status = 200, body = [Provider])))]
#[tracing::instrument(
    name = PROVIDERS_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<Provider>>> {
    Ok(Json(st.providers.list().await?))
}

#[utoipa::path(post, path = "/api/providers", tag = "providers", request_body = SaveProvider,
    responses((status = 201, body = Provider), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_CREATE,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Json(input): Json<SaveProvider>,
) -> AppResult<(StatusCode, Json<Provider>)> {
    if st.provider_registry.get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    Ok((StatusCode::CREATED, Json(st.providers.create(input).await?)))
}

/// Attribute a to-be-created account to the household when the feed shows it is joint.
///
/// Enforced here rather than left to the dialog's locked `<select>`: an account two people's
/// logins both report is not one person's money, and a client is free to send whatever it
/// likes. The dialog locks the control so the rule is visible; this is what makes it true.
///
/// Only ever applied to a `new_account`. Attaching a joint feed to an account that already
/// exists leaves that account's ownership alone — it is an account the user already
/// attributed deliberately, and silently re-attributing it would be a side effect of a
/// request that never mentioned ownership.
fn force_joint_ownership(joint: bool, new_account: Option<&mut SaveAccount>) {
    if let (true, Some(account)) = (joint, new_account) {
        account.ownership = Ownership::Joint;
    }
}

/// Link an upstream account (from [`discover_accounts`]) to a local account, creating it
/// first if `new_account` is given rather than `existing_account_id`. Triggers an
/// immediate best-effort sync so the account isn't empty until the next scheduled poll.
///
/// Refused with a 422 when the household already reaches this same underlying account
/// through another login — see `sure_app`'s `check_linkable`. A joint account is reported
/// once per holder's login under a different id and nickname each time, so it is the one
/// duplicate the `external_id` filter on [`discover_accounts`] cannot catch by itself.
///
/// A `new_account` here is validated as `ValidationMode::Linked` (see `sure_core`): a feed
/// reports a name, a kind and a currency, so the fields the account form insists on — a
/// mortgage's principal, a property's city — are not demanded of it. Sync fills in whatever
/// the upstream does report.
#[utoipa::path(post, path = "/api/providers/link", tag = "providers", request_body = LinkProviderAccount,
    responses((status = 201, body = Provider), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_LINK,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn link(
    State(st): State<AppState>,
    Json(mut input): Json<LinkProviderAccount>,
) -> AppResult<(StatusCode, Json<Provider>)> {
    if st.provider_registry.get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    // Refuses an account the household already reaches through another login, and tells us
    // whether this one is joint. Both before anything is written: a duplicate link is not
    // something to undo afterwards, because the second copy syncs the same transactions into
    // a second account and counts the balance twice in net worth.
    let joint = st
        .sync
        .check_linkable(&input.kind, &input.external_id)
        .await?;
    force_joint_ownership(joint, input.new_account.as_mut());

    let kind = input.kind.clone();
    let provider = st.providers.link(input).await?;

    // Best-effort, and before the sync so a sync failure doesn't skip it: the account number
    // the feed reports is what lets a bank export route itself to this account later, and
    // nothing else records it. Fills in every still-blank sibling of the same kind too.
    if let Err(e) = st.sync.adopt_account_numbers(&kind).await {
        tracing::warn!(kind = %kind, error = %e, "could not adopt upstream account numbers");
    }

    // Best-effort: a failed initial sync (e.g. not-yet-configured credentials) is already
    // durably recorded as an "error" sync row and doesn't undo the link — the user can
    // retry via "Sync now" once fixed.
    let id = provider.id;
    if let Err(e) = st.sync.sync_provider(provider, None).await {
        tracing::warn!(provider_id = id, error = %e, "initial sync after linking failed");
    }
    let provider = st.providers.get(id).await?;

    Ok((StatusCode::CREATED, Json(provider)))
}

/// Link several upstream accounts to one local account at once (e.g. every currency wallet
/// of a Sharesies brokerage account into a single Brokerage account). Creates the account
/// once, links every member, then best-effort syncs each. See [`LinkProviderGroup`]; a
/// `new_account` is validated in `Linked` mode, as in [`link`].
#[utoipa::path(post, path = "/api/providers/link-group", tag = "providers", request_body = LinkProviderGroup,
    responses((status = 201, body = [Provider]), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_LINK_GROUP,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn link_group(
    State(st): State<AppState>,
    Json(mut input): Json<LinkProviderGroup>,
) -> AppResult<(StatusCode, Json<Vec<Provider>>)> {
    if st.provider_registry.get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    // Every member, before any of them is written — the group is created in one transaction
    // and a member the household already has would double that account just as a single link
    // would. A group is joint if any member is: they are wallets of one real account.
    let mut joint = false;
    for member in &input.members {
        joint |= st
            .sync
            .check_linkable(&input.kind, &member.external_id)
            .await?;
    }
    force_joint_ownership(joint, input.new_account.as_mut());

    let kind = input.kind.clone();
    let providers = st.providers.link_group(input).await?;

    // Same rationale as `link` — a group's members are wallets of one account, and it is still
    // that account's number a bank export would be routed by.
    if let Err(e) = st.sync.adopt_account_numbers(&kind).await {
        tracing::warn!(kind = %kind, error = %e, "could not adopt upstream account numbers");
    }

    // Best-effort initial sync per member, same rationale as `link`.
    let ids: Vec<i64> = providers.iter().map(|p| p.id).collect();
    for provider in providers {
        let id = provider.id;
        if let Err(e) = st.sync.sync_provider(provider, None).await {
            tracing::warn!(provider_id = id, error = %e, "initial sync after group-linking failed");
        }
    }
    let mut refreshed = Vec::with_capacity(ids.len());
    for id in ids {
        refreshed.push(st.providers.get(id).await?);
    }

    Ok((StatusCode::CREATED, Json(refreshed)))
}

#[utoipa::path(put, path = "/api/providers/{id}", tag = "providers", params(("id" = i64, Path,)),
    request_body = SaveProvider,
    responses((status = 200, body = Provider), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_UPDATE,
    level = "debug",
    skip_all,
    fields(provider_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveProvider>,
) -> AppResult<Json<Provider>> {
    if st.provider_registry.get(&input.kind).is_none() {
        return Err(AppError::validation(format!(
            "unknown provider kind '{}'",
            input.kind
        )));
    }
    Ok(Json(st.providers.update(id, input).await?))
}

#[utoipa::path(delete, path = "/api/providers/{id}", tag = "providers", params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_DELETE,
    level = "debug",
    skip_all,
    fields(provider_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.providers.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sync a provider: fetch upstream transactions, import new ones (dedupe on
/// external id), and record the result.
///
/// **A 200 is not the same as a healthy sync.** Read `status` on the body: `disconnected` means
/// the upstream no longer has the account this connection points at, which is a state of the
/// connection rather than a request that failed — the only fix is to link the account again, and
/// `detail` says so. It answers 200 rather than 4xx deliberately, so a caller syncing several
/// connections in turn keeps going; a failure that may not recur is still a 422.
///
/// Single-flight per provider: while one sync of this provider is running — whether started
/// here, by the initial sync after linking, or by the 6-hourly poll — a second request gets a
/// 409 instead of a duplicate run.
///
/// Rate limited per provider on top of that: within the cooldown of a *successful* sync, the
/// upstream is not contacted and the previous run is returned with `fresh: false`. A failed
/// sync can be retried immediately.
// Below the doc comment on purpose: utoipa publishes that as the public OpenAPI description, and
// which internal type holds the guards is not the caller's business. Both live in `SyncService`
// (`sure_app::sync`) precisely so they span all three callers; see its `in_flight` and
// `cooldown` docs for why duplicating a run is worse than refusing it (upstream rate limits are
// per household, and this route holds an in-flight permit for its whole 300s deadline).
#[utoipa::path(post, path = "/api/providers/{id}/sync", tag = "providers", params(("id" = i64, Path,)),
    request_body = SyncRequest,
    responses((status = 200, body = SyncReport), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PROVIDERS_SYNC,
    level = "debug",
    skip_all,
    fields(provider_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn sync(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<SyncRequest>,
) -> AppResult<Json<SyncReport>> {
    let provider = st.providers.get(id).await?;
    Ok(Json(
        st.sync
            .sync_provider(provider, req.payload.as_deref())
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/providers/{id}/syncs", tag = "providers", params(("id" = i64, Path,)),
    responses((status = 200, body = [ProviderSync])))]
#[tracing::instrument(
    name = PROVIDERS_LIST_SYNCS,
    level = "debug",
    skip_all,
    fields(provider_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list_syncs(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<Vec<ProviderSync>>> {
    Ok(Json(st.providers.list_syncs(id).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/provider-kinds", get(kinds))
        .route("/provider-kinds/{kind}/accounts", get(discover_accounts))
        .route("/providers", get(list).post(create))
        .route("/providers/link", post(link))
        .route("/providers/link-group", post(link_group))
        .route("/providers/{id}", axum::routing::put(update).delete(delete))
        .route("/providers/{id}/sync", post(sync))
        .route("/providers/{id}/syncs", get(list_syncs))
}
