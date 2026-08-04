//! Student-loan endpoint: a bulk upload of myIR "TAP SLS Transactions" exports. The heavy
//! lifting (unzipping, reading the workbooks, reconciling overlapping windows, flipping
//! IR's sign) lives in `sure_providers::myir`; this handler is thin glue, mirroring
//! `routes::brokerage`'s Sharesies import.
//!
//! Akahu reports an IR student loan's balance but no transactions, so the account's ledger
//! comes from two places joined at a cutover: these exports behind it, and
//! `sure_app::tasks::balance_delta` — which differences the daily balance feed — from it
//! onward. The cutover isn't a parameter here: it's read from whichever provider connection
//! on this account has opted into deriving, so the two halves cannot drift apart.

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::routing::post;
use axum::Router;
use chrono::NaiveDate;
use sure_core::AccountKind;

use crate::config::Limits;
use crate::error::{AppError, AppResult};
use crate::extract::Json;
use crate::state::AppState;

pub use sure_core::StudentLoanImportResult;

const STUDENT_LOAN_IMPORT: &str = "student_loan.import";

/// Bulk-import myIR student-loan exports: a zip of `.xlsx` downloads, or a single bare
/// `.xlsx`. Idempotent — re-uploading the same exports imports nothing new, so overlapping
/// download windows are free.
#[utoipa::path(post, path = "/api/accounts/{id}/student-loan/import", tag = "transactions",
    params(("id" = i64, Path,)),
    request_body(content = Vec<u8>, description = "A myIR export .xlsx, or a .zip of them", content_type = "application/zip"),
    responses((status = 200, body = StudentLoanImportResult), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = STUDENT_LOAN_IMPORT,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn import(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    body: Bytes,
) -> AppResult<Json<StudentLoanImportResult>> {
    let account = st.accounts.get(id).await?;
    if account.kind != AccountKind::StudentLoan {
        return Err(AppError::validation(
            "account is not a student loan account",
        ));
    }

    let until = derive_cutover(&st, id).await?;

    // Unzip + XLSX parse is CPU-bound: keep it off the async runtime's worker threads.
    let export =
        tokio::task::spawn_blocking(move || sure_providers::myir::parse_export(&body, until))
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .map_err(|e| AppError::validation(format!("could not read export: {e}")))?;

    let rows: Vec<sure_app::ports::ImportRow> = export
        .transactions
        .into_iter()
        .map(|t| sure_app::ports::ImportRow {
            external_id: t.external_id,
            posted_at: t.posted_at,
            amount_minor: t.amount_minor,
            currency_code: t.currency_code,
            description: t.description,
            merchant: t.merchant,
            category_name: t.category.as_ref().map(|c| c.name.clone()),
            category_kind: t.category.as_ref().and_then(|c| c.kind),
            category_group: t.category.and_then(|c| c.group),
            is_one_off: false,
        })
        .collect();

    // Keyed on the account, like the Sharesies import — this is a manual upload, not a
    // standing connection, so it needs no `providers` row of its own.
    let provider_tag = format!("myir#{id}");
    let (imported, skipped) = st
        .providers
        .import_transactions(id, &account.currency_code, &provider_tag, &rows)
        .await?;

    // Transfer auto-linking (a voluntary payment ↔ the matching bank transaction) is not
    // done here: the bank side is often synced *after* this upload, so it wouldn't exist to
    // match yet. `sure_app::tasks::transfer_link` reconciles both regardless of order.

    Ok(Json(StudentLoanImportResult {
        imported,
        skipped,
        account_id: export.account_id,
        covered_from: export.covered_from,
        covered_to: export.covered_to,
        warnings: export.warnings,
    }))
}

/// The first date `balance_delta` derives this account's movements for, if any connection
/// on it has opted in. Rows from that date onward are held back so the same movement isn't
/// posted twice — once labelled from myIR, once derived from the balance.
async fn derive_cutover(st: &AppState, account_id: i64) -> AppResult<Option<NaiveDate>> {
    Ok(st
        .providers
        .list()
        .await?
        .into_iter()
        .filter(|p| p.account_id == account_id && p.enabled)
        .filter(|p| {
            p.config
                .get("derive_transactions_from_balance")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|p| {
            p.config
                .get("derive_from")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").ok())
        })
        .min())
}

pub fn router(limits: &Limits) -> Router<AppState> {
    Router::new().route(
        "/accounts/{id}/student-loan/import",
        post(import).layer(DefaultBodyLimit::max(limits.max_import_body_bytes)),
    )
}
