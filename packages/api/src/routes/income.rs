use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};

use crate::error::AppResult;
use crate::extract::Json;
use crate::state::AppState;

// The domain types live in sure-core; re-export so the OpenAPI registration
// (`crate::routes::income::IncomeStream`, ...) and the handler annotations resolve.
pub use sure_core::{
    IncomeBasis, IncomePayment, IncomePaymentStatus, IncomeStream, IncomeStreamStep, MatchedBy,
    OwnedTaxScale, PayFrequency, PayTreatment, SaveIncomeStream, SaveIncomeStreamStep,
    SaveTaxScale, StoredTaxScale, TakeHomeSource, TaxScaleId,
};

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

// OTEL span names for this module's handlers.
const INCOME_LIST: &str = "income.list";
const INCOME_GET: &str = "income.get";
const INCOME_CREATE: &str = "income.create";
const INCOME_UPDATE: &str = "income.update";
const INCOME_DELETE: &str = "income.delete";
const INCOME_DETECT: &str = "income.detect";
const PAYMENTS_LIST: &str = "income.payments.list";
const PAYMENT_SET_STATUS: &str = "income.payments.set_status";
const PAYMENT_UNLINK: &str = "income.payments.unlink";
const TAX_LIST: &str = "tax.list";
const TAX_CREATE: &str = "tax.create";
const TAX_UPDATE: &str = "tax.update";
const TAX_DELETE: &str = "tax.delete";
const TAX_RESTORE: &str = "tax.restore";

/// A salary the ledger appears to contain.
#[derive(Debug, Serialize, ToSchema)]
pub struct DetectedStream {
    pub label: String,
    pub account_id: i64,
    pub category_id: Option<i64>,
    pub currency_code: String,
    pub pay_frequency: PayFrequency,
    pub last_paid_on: String,
    /// Where a stream recorded from this should start, so a payment already in the ledger is not
    /// credited a second time as a projection.
    pub next_payment_on: String,
    pub per_payment_minor: i64,
    /// The annual figure implied by the cadence. **Net** — it is what actually landed, so a stream
    /// created from it should be recorded as take-home rather than before tax.
    pub annual_net_minor: i64,
    pub payments_seen: usize,
    /// The days of the month payments land on. Two fixed days is the evidence for twice-monthly
    /// rather than every-fourteen-days, which are 24 and 26 payments a year respectively.
    pub days_of_month: Vec<u32>,
    /// How much the amounts vary, in basis points of the typical one. Near zero is a salary;
    /// anything wide is a payment a fixed annual figure would misrepresent.
    pub variability_bps: i64,
}

impl From<sure_app::detect::DetectedStream> for DetectedStream {
    fn from(d: sure_app::detect::DetectedStream) -> Self {
        DetectedStream {
            label: d.label,
            account_id: d.account_id,
            category_id: d.category_id,
            currency_code: d.currency_code,
            pay_frequency: d.pay_frequency,
            last_paid_on: d.last_paid_on,
            next_payment_on: d.next_payment_on,
            per_payment_minor: d.per_payment_minor,
            annual_net_minor: d.annual_net_minor,
            payments_seen: d.payments_seen,
            days_of_month: d.days_of_month,
            variability_bps: d.variability_bps,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DetectQuery {
    /// Limit the search to one account. Omit to search every account.
    pub account_id: Option<i64>,
}

/// Every income stream in the household, with its dated pay-scale steps attached.
///
/// Flat and unfiltered: the income screen wants every person's streams at once, and one request
/// per person would be N round trips for a few rows.
#[utoipa::path(get, path = "/api/income-streams", tag = "income",
    responses((status = 200, body = [IncomeStream])))]
#[tracing::instrument(
    name = INCOME_LIST,
    level = "debug",
    skip_all,
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn list(State(st): State<AppState>) -> AppResult<Json<Vec<IncomeStream>>> {
    Ok(Json(st.income.list_income_streams().await?))
}

/// One income stream.
#[utoipa::path(get, path = "/api/income-streams/{id}", tag = "income",
    params(("id" = i64, Path,)),
    responses((status = 200, body = IncomeStream), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_GET,
    level = "debug",
    skip_all,
    fields(stream_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn get_one(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<IncomeStream>> {
    Ok(Json(st.income.get_income_stream(id).await?))
}

/// Record income for someone in the household.
///
/// Nested under the person, and flat for every mutation below — the `valuations` arrangement. It
/// puts `person_id` in the path, where it cannot be omitted or contradicted by the body, and keeps
/// the mutation URLs stable.
#[utoipa::path(post, path = "/api/people/{person_id}/income-streams", tag = "income",
    params(("person_id" = i64, Path,)), request_body = SaveIncomeStream,
    responses((status = 201, body = IncomeStream), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_CREATE,
    level = "debug",
    skip_all,
    fields(person_id = %person_id),
    err(level = tracing::Level::WARN),
)]
pub async fn create(
    State(st): State<AppState>,
    Path(person_id): Path<i64>,
    Json(input): Json<SaveIncomeStream>,
) -> AppResult<(StatusCode, Json<IncomeStream>)> {
    Ok((
        StatusCode::CREATED,
        Json(st.income.create_income_stream(person_id, input).await?),
    ))
}

/// Replace an income stream, its pay-scale schedule included.
///
/// The steps sent here *are* the schedule afterwards, so removing one is omitting it — the
/// full-replace contract `PUT /api/forecast/assumptions` already has.
#[utoipa::path(put, path = "/api/income-streams/{id}", tag = "income",
    params(("id" = i64, Path,)), request_body = SaveIncomeStream,
    responses((status = 200, body = IncomeStream), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_UPDATE,
    level = "debug",
    skip_all,
    fields(stream_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn update(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveIncomeStream>,
) -> AppResult<Json<IncomeStream>> {
    Ok(Json(st.income.update_income_stream(id, input).await?))
}

/// Remove an income stream. Refused with 409 while a forecast change still points at it — repoint
/// or remove those first, so a promotion cannot quietly become a no-op.
#[utoipa::path(delete, path = "/api/income-streams/{id}", tag = "income",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = INCOME_DELETE,
    level = "debug",
    skip_all,
    fields(stream_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn delete(State(st): State<AppState>, Path(id): Path<i64>) -> AppResult<StatusCode> {
    st.income.delete_income_stream(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Salaries already visible in the ledger, so recording one is a confirmation rather than a
/// transcription.
///
/// Worth doing because the details people get wrong are exactly the ones the ledger already knows:
/// whether "fortnightly" means every fourteen days or twice a month, which day it lands on, and what
/// the net figure actually is after payroll has taken everything off.
#[utoipa::path(get, path = "/api/income-streams/detect", tag = "income",
    params(DetectQuery),
    responses((status = 200, body = [DetectedStream])))]
#[tracing::instrument(
    name = INCOME_DETECT,
    level = "debug",
    skip_all,
    err(level = tracing::Level::WARN),
)]
pub async fn detect(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<DetectQuery>,
) -> AppResult<Json<Vec<DetectedStream>>> {
    Ok(Json(
        st.forecast
            .detect_income(q.account_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

/// The tax rules the projection uses.
///
/// Editable because they are external facts with a shelf life: IRD changes a threshold and a
/// projection is quietly wrong until someone ships a binary. The built-in figures seed this on first
/// run and are what `restore` puts back.
#[utoipa::path(get, path = "/api/tax-scales", tag = "tax",
    responses((status = 200, body = [StoredTaxScale])))]
#[tracing::instrument(name = TAX_LIST, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn list_tax_scales(State(st): State<AppState>) -> AppResult<Json<Vec<StoredTaxScale>>> {
    Ok(Json(st.income.list_tax_scales().await?))
}

/// Add a scale — how you record next year's rates before they take effect.
#[utoipa::path(post, path = "/api/tax-scales", tag = "tax", request_body = SaveTaxScale,
    responses((status = 201, body = StoredTaxScale), (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = TAX_CREATE, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn create_tax_scale(
    State(st): State<AppState>,
    Json(input): Json<SaveTaxScale>,
) -> AppResult<(StatusCode, Json<StoredTaxScale>)> {
    Ok((
        // NZ PAYE is the one jurisdiction the API offers today; the enum exists so a second one
        // is a new value here rather than a schema change.
        StatusCode::CREATED,
        Json(
            st.income
                .create_tax_scale(TaxScaleId::NzPaye, input)
                .await?,
        ),
    ))
}

#[utoipa::path(put, path = "/api/tax-scales/{id}", tag = "tax",
    params(("id" = i64, Path,)), request_body = SaveTaxScale,
    responses((status = 200, body = StoredTaxScale), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = TAX_UPDATE, level = "debug", skip_all, fields(scale_id = %id), err(level = tracing::Level::WARN))]
pub async fn update_tax_scale(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SaveTaxScale>,
) -> AppResult<Json<StoredTaxScale>> {
    Ok(Json(st.income.update_tax_scale(id, input).await?))
}

/// Remove a scale. Refused when it is the last one — an empty table taxes every gross salary at
/// nothing, which reads as a windfall rather than a mistake.
#[utoipa::path(delete, path = "/api/tax-scales/{id}", tag = "tax",
    params(("id" = i64, Path,)),
    responses((status = 204), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(name = TAX_DELETE, level = "debug", skip_all, fields(scale_id = %id), err(level = tracing::Level::WARN))]
pub async fn delete_tax_scale(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<StatusCode> {
    st.income.delete_tax_scale(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Throw away every stored scale and put the built-in figures back — the way out of an edit that
/// went wrong.
#[utoipa::path(post, path = "/api/tax-scales/restore", tag = "tax",
    responses((status = 200, body = [StoredTaxScale])))]
#[tracing::instrument(name = TAX_RESTORE, level = "debug", skip_all, err(level = tracing::Level::WARN))]
pub async fn restore_tax_scales(
    State(st): State<AppState>,
) -> AppResult<Json<Vec<StoredTaxScale>>> {
    Ok(Json(st.income.restore_tax_scales().await?))
}

/// Filters for the payment list. `status` arrives as text and is parsed at this edge — an
/// unrecognised value is a 400 naming it, never a silently ignored filter (the `Interval`
/// pattern in `routes::reports`).
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PaymentsQuery {
    /// Earliest `due_on` (ISO-8601 date), inclusive.
    pub from: Option<String>,
    /// Latest `due_on` (ISO-8601 date), inclusive.
    pub to: Option<String>,
    /// Limit to one person's streams.
    pub person_id: Option<i64>,
    /// `expected`, `matched`, `confirmed` or `dismissed`.
    pub status: Option<String>,
}

/// Expected and matched payments, newest first.
///
/// Past-due `expected` rows are the "missed pay" signal; `matched` rows carry the reconstructed
/// gross → deductions → net decomposition of the deposit they claimed.
#[utoipa::path(get, path = "/api/income-payments", tag = "income",
    params(PaymentsQuery),
    responses((status = 200, body = [IncomePayment]), (status = 400, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PAYMENTS_LIST,
    level = "debug",
    skip_all,
    err(level = tracing::Level::WARN),
)]
pub async fn list_payments(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<PaymentsQuery>,
) -> AppResult<Json<Vec<IncomePayment>>> {
    let status: Option<IncomePaymentStatus> = match q.status.as_deref() {
        Some(s) => Some(
            s.parse()
                .map_err(|e: String| sure_core::AppError::bad_request(e))?,
        ),
        None => None,
    };
    Ok(Json(
        st.income
            .list_income_payments(q.from.as_deref(), q.to.as_deref(), q.person_id, status)
            .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetPaymentStatus {
    pub status: IncomePaymentStatus,
}

/// Move a payment between the human-owned statuses.
///
/// Legal moves: `matched → confirmed` (agree with the matcher), `expected → dismissed` (this
/// payday is not real — unpaid leave, a contract gap; kept so the matcher does not resurrect
/// it), and `dismissed → expected` (re-open). `matched` is the matcher's own state and
/// `expected`-from-`matched` is what DELETE …/link does, so neither is reachable from here —
/// a refused move is a 409 naming the states.
#[utoipa::path(patch, path = "/api/income-payments/{id}", tag = "income",
    params(("id" = i64, Path,)), request_body = SetPaymentStatus,
    responses((status = 200, body = IncomePayment), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PAYMENT_SET_STATUS,
    level = "debug",
    skip_all,
    fields(payment_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn set_payment_status(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<SetPaymentStatus>,
) -> AppResult<Json<IncomePayment>> {
    let current = st.income.get_income_payment(id).await?;
    let legal = matches!(
        (current.status, input.status),
        (IncomePaymentStatus::Matched, IncomePaymentStatus::Confirmed)
            | (
                IncomePaymentStatus::Expected,
                IncomePaymentStatus::Dismissed
            )
            | (
                IncomePaymentStatus::Dismissed,
                IncomePaymentStatus::Expected
            )
    );
    if !legal {
        return Err(sure_core::AppError::conflict(format!(
            "cannot move a payment from '{}' to '{}'",
            current.status.as_str(),
            input.status.as_str()
        )));
    }
    Ok(Json(
        st.income
            .set_income_payment_status(id, input.status)
            .await?,
    ))
}

/// Undo a match: the payment returns to `expected` with its decomposition cleared, and the
/// transaction is released for the matcher (or a person) to claim elsewhere.
#[utoipa::path(delete, path = "/api/income-payments/{id}/link", tag = "income",
    params(("id" = i64, Path,)),
    responses((status = 200, body = IncomePayment), (status = 404, body = crate::error::ErrorBody),
              (status = 409, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = PAYMENT_UNLINK,
    level = "debug",
    skip_all,
    fields(payment_id = %id),
    err(level = tracing::Level::WARN),
)]
pub async fn unlink_payment(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<IncomePayment>> {
    let current = st.income.get_income_payment(id).await?;
    if current.transaction_id.is_none() {
        return Err(sure_core::AppError::conflict(
            "this payment has no linked transaction to remove",
        ));
    }
    Ok(Json(st.income.unlink_income_payment(id).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        // Before `/{id}`, or axum matches "detect" as an id.
        .route("/income-streams/detect", get(detect))
        .route("/tax-scales", get(list_tax_scales).post(create_tax_scale))
        .route("/tax-scales/restore", post(restore_tax_scales))
        .route(
            "/tax-scales/{id}",
            axum::routing::put(update_tax_scale).delete(delete_tax_scale),
        )
        .route("/income-streams", get(list))
        .route(
            "/income-streams/{id}",
            get(get_one).put(update).delete(delete),
        )
        .route("/people/{person_id}/income-streams", post(create))
        .route("/income-payments", get(list_payments))
        .route(
            "/income-payments/{id}",
            axum::routing::patch(set_payment_status),
        )
        .route(
            "/income-payments/{id}/link",
            axum::routing::delete(unlink_payment),
        )
}
