//! ASB endpoints: a bulk upload of ASB "Export transactions" CSVs. The parsing — preamble,
//! transaction types, ASB's fixed-width text artifacts, unzipping, reconciling several
//! windows of one account — lives in `sure_providers::asb`; these handlers are thin glue,
//! mirroring `routes::student_loan`'s myIR import.
//!
//! Akahu serves an account roughly two years of history and ASB's own export reaches seven,
//! so the two overlap, and dedupe (`(provider, external_id)`) cannot see across them: the
//! same transaction from both sources is two rows. The cutover therefore isn't a parameter.
//! It's read from each account's own ledger — the earliest date any *other* feed has posted
//! — so the halves cannot be made to overlap by getting an argument wrong.
//!
//! Two entry points, differing only in how the target account is decided:
//!
//! * [`import`] — `/accounts/{id}/asb/import`. The account is the path. For one account's
//!   export(s), and the escape hatch when routing can't work it out.
//! * [`upload`] — `/asb/import`. For a zip spanning several accounts, which is how a bank
//!   with a chequing account and half a dozen savings pots is actually exported. Each
//!   export is routed by [`resolve`], and anything unresolved is reported rather than
//!   guessed at.
//!
//! `?dry_run=true` runs everything except the insert and reports what a commit would do.
//! It's the same code path up to one branch, so a preview cannot describe an import that
//! wouldn't happen.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::post;
use axum::{Json, Router};
use chrono::NaiveDate;
use serde::Deserialize;
use sure_core::{Account, AccountKind, AccountMetadata, AsbMatch};
use sure_providers::asb::AsbExport;
use utoipa::IntoParams;

use crate::config::Limits;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub use sure_core::{AsbImportResult, AsbUndoResult, AsbUploadResult};

const ASB_IMPORT: &str = "asb.import";
const ASB_UPLOAD: &str = "asb.upload";
const ASB_UNDO: &str = "asb.undo";

/// Provider-tag stem for every ASB import. `asb#<account id>` per account, so one account's
/// rows can be found (and undone) without touching another source's.
const TAG: &str = "asb#";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct ImportQuery {
    /// Parse and report, but write nothing. Defaults to committing.
    pub dry_run: Option<bool>,
    /// Whether to also record the opening balance the export implies — the account's value
    /// immediately before the first row — as a one-off transaction. On by default: without it
    /// the reconstructed history starts from nothing rather than from what the account held.
    /// Skipped anyway when the export states no closing balance, or when the account already
    /// has a row from before that date.
    pub opening_balance: Option<bool>,
    /// Which Sure account each ASB account number belongs to, as
    /// `12-3136-0000123-50:8,12-3136-0000123-51:12`. Overrides whatever [`resolve`] would
    /// have worked out, and is how the UI commits exactly what its preview showed.
    pub assign: Option<String>,
}

/// What the request asked for, resolved from the query string once.
struct Options {
    dry_run: bool,
    opening_balance: bool,
}

impl From<&ImportQuery> for Options {
    fn from(q: &ImportQuery) -> Self {
        Self {
            dry_run: q.dry_run.unwrap_or(false),
            // On unless asked otherwise: an imported history that starts from nothing is
            // wrong, and the preview shows the figure before anything is written.
            opening_balance: q.opening_balance.unwrap_or(true),
        }
    }
}

/// Whether an ASB transaction export makes sense for this kind of account. ASB exports the
/// same CSV for everyday, savings, and card accounts; the rest of Sure's kinds either have
/// no ASB statement (a property, a share holding) or already have their own importer.
///
/// Exhaustively matched rather than defaulted, so a new `AccountKind` has to come here and
/// decide (CLAUDE.md rule 2).
fn accepts_asb_csv(kind: AccountKind) -> bool {
    use AccountKind::*;
    match kind {
        Cash | Bank | Savings | CreditCard | RevolvingCredit => true,
        Mortgage | StudentLoan | Loan | Liability | Vehicle | RealEstate | Asset | SharesNz
        | SharesUs | SharesPrivate | Brokerage | Crypto => false,
    }
}

/// Import an ASB transaction export into one account: a `.csv`, or a `.zip` of them (several
/// windows of the same account are reconciled). Idempotent — re-uploading the same export
/// imports nothing new, so overlapping download windows are free. For a zip spanning several
/// accounts use `POST /api/asb/import`.
#[utoipa::path(post, path = "/api/accounts/{id}/asb/import", tag = "transactions",
    params(("id" = i64, Path,), ImportQuery),
    request_body(content = Vec<u8>, description = "An ASB transaction export .csv, or a .zip of them", content_type = "application/zip"),
    responses((status = 200, body = AsbImportResult), (status = 404, body = crate::error::ErrorBody),
              (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ASB_IMPORT,
    level = "debug",
    skip_all,
    fields(account_id = %id, dry_run = %q.dry_run.unwrap_or(false)),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn import(
    State(st): State<AppState>,
    Path(id): Path<i64>,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> AppResult<Json<AsbImportResult>> {
    let account = st.accounts.get(id).await?;
    guard_kind(&account)?;

    let mut upload = parse(body).await?;
    let export = match upload.exports.len() {
        1 => upload.exports.remove(0),
        // Nothing here can say which of the several accounts belongs to this one, and
        // picking would be guessing with someone's money.
        n => {
            return Err(AppError::validation(format!(
                "this upload holds exports for {n} different ASB accounts ({}); import it \
                 from Settings → Accounts instead, where each one can be assigned",
                upload
                    .exports
                    .iter()
                    .map(|e| e.account.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }
    };

    let mut result = import_one(&st, export, Some((&account, None)), &Options::from(&q)).await?;
    result.warnings.extend(upload.warnings);
    Ok(Json(result))
}

/// Import ASB transaction exports, routing each to the account it belongs to. Takes a `.csv`
/// or a `.zip` of them — one zip can carry every account at once, which is how ASB's
/// one-file-per-account export is actually taken.
///
/// Nothing in an export names a Sure account, so each is matched by `assign`, then by a
/// previous import of the same ASB account, then by the account's stored number or name. An
/// export that matches nothing is reported and *not* imported.
#[utoipa::path(post, path = "/api/asb/import", tag = "transactions",
    params(ImportQuery),
    request_body(content = Vec<u8>, description = "An ASB transaction export .csv, or a .zip of them", content_type = "application/zip"),
    responses((status = 200, body = AsbUploadResult), (status = 422, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ASB_UPLOAD,
    level = "debug",
    skip_all,
    fields(dry_run = %q.dry_run.unwrap_or(false)),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn upload(
    State(st): State<AppState>,
    Query(q): Query<ImportQuery>,
    body: Bytes,
) -> AppResult<Json<AsbUploadResult>> {
    let opts = Options::from(&q);
    let upload = parse(body).await?;

    let accounts: Vec<Account> = st
        .accounts
        .list(false)
        .await?
        .into_iter()
        .filter(|a| accepts_asb_csv(a.kind))
        .collect();
    let assigned = parse_assignments(q.assign.as_deref())?;
    let prior = prior_imports(&st).await?;

    let mut exports = Vec::new();
    for export in upload.exports {
        let matched = resolve(&export.account, &accounts, &assigned, &prior);
        exports.push(import_one(&st, export, matched, &opts).await?);
    }
    Ok(Json(AsbUploadResult {
        dry_run: opts.dry_run,
        exports,
        warnings: upload.warnings,
    }))
}

/// Remove a previous ASB import from this account, leaving every other source untouched.
#[utoipa::path(delete, path = "/api/accounts/{id}/asb/import", tag = "transactions",
    params(("id" = i64, Path,)),
    responses((status = 200, body = AsbUndoResult), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = ASB_UNDO,
    level = "debug",
    skip_all,
    fields(account_id = %id),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn undo(
    State(st): State<AppState>,
    Path(id): Path<i64>,
) -> AppResult<Json<AsbUndoResult>> {
    // Confirms the account exists, so undoing against a bad id is a 404 rather than a
    // cheerful "deleted 0".
    st.accounts.get(id).await?;
    let deleted = st
        .transactions
        .delete_by_provider(id, &provider_tag(id))
        .await?;
    Ok(Json(AsbUndoResult { deleted }))
}

// --------------------------------------------------------------------------------------
// the shared path
// --------------------------------------------------------------------------------------

/// Unzipping and parsing a few thousand CSV rows is CPU-bound: keep it off the async
/// runtime's worker threads.
async fn parse(body: Bytes) -> AppResult<sure_providers::asb::AsbUpload> {
    tokio::task::spawn_blocking(move || sure_providers::asb::parse_upload(&body))
        .await
        .map_err(|e| AppError::Internal(e.into()))?
        .map_err(|e| AppError::validation(format!("could not read export: {e}")))
}

/// Everything that happens to one export once its target account is known: derive that
/// account's cutover, hold back what a live feed already owns, reconcile the closing
/// balance, and — unless this is a dry run — insert.
///
/// `target` is `None` when nothing identified an account. Then the export is described and
/// skipped: reporting "we don't know where this goes" is the only honest option, since
/// putting a savings account's history into a chequing account is not a recoverable mistake.
async fn import_one(
    st: &AppState,
    mut export: AsbExport,
    target: Option<(&Account, Option<AsbMatch>)>,
    opts: &Options,
) -> AppResult<AsbImportResult> {
    let dry_run = opts.dry_run;
    let mut warnings = std::mem::take(&mut export.warnings);
    let Some((account, matched_by)) = target else {
        warnings.push(
            "no account was matched to this export, so it wasn't imported — choose one and \
             import again"
                .to_string(),
        );
        return Ok(describe(&export, None, None, dry_run, warnings));
    };

    let provider_tag = provider_tag(account.id);
    let cutover = st
        .transactions
        .earliest_posted_at_from_other_feed(account.id, &provider_tag)
        .await?
        // Stored as a full timestamp; the cutover is a whole day.
        .and_then(|at| NaiveDate::parse_from_str(at.get(..10).unwrap_or(&at), "%Y-%m-%d").ok());
    export.hold_back_from(cutover);
    warnings.append(&mut export.warnings);

    let account_balance_minor = latest_balance(st, account.id).await;
    // The strongest signal available that this export belongs to this account and reaches
    // all the way back: ASB's stated closing balance against Sure's own. A warning, not a
    // refusal — a legitimate export can predate the latest valuation.
    if let (Some(stated), Some(held)) = (export.ledger_balance_minor, account_balance_minor) {
        if stated != held {
            warnings.push(format!(
                "the export closes at {} but {} holds {} — check the export is for that \
                 account, and that its date range reaches back far enough",
                major(stated),
                account.name,
                major(held)
            ));
        }
    }

    // The account's value before the file's first row. Withheld when something is already on
    // the ledger from before that date, because then this wouldn't be an opening balance —
    // it would be a large invented movement in the middle of existing history.
    let opening = match (opts.opening_balance, export.opening_balance_row()) {
        (true, Some(row)) => {
            let existing = st.transactions.earliest_posted_at(account.id).await?;
            let clear = existing
                .as_deref()
                .is_none_or(|at| at >= row.posted_at.as_str());
            if !clear {
                warnings.push(format!(
                    "{} already has transactions from before {}, so the opening balance of {} \
                     the export implies was not recorded — it would land in the middle of the \
                     ledger rather than ahead of it",
                    account.name,
                    &row.posted_at[..10],
                    major(row.amount_minor)
                ));
            }
            clear.then_some(row)
        }
        // Nothing to work back from, or the caller opted out.
        (true, None) | (false, _) => None,
    };

    let mut result = describe(
        &export,
        Some(account),
        matched_by,
        dry_run,
        std::mem::take(&mut warnings),
    );
    result.account_balance_minor = account_balance_minor;
    result.cutover = cutover.map(|d| d.to_string());
    result.opening_balance_minor = opening.as_ref().map(|r| r.amount_minor);
    result.opening_balance_as_of = opening.as_ref().map(|r| r.posted_at[..10].to_string());
    // Counted with the rows it goes in alongside, so the preview's figure is the number of
    // rows the commit actually writes.
    result.would_import += i64::from(opening.is_some());
    if dry_run {
        return Ok(result);
    }

    let mut rows: Vec<sure_app::ports::ImportRow> = export
        .transactions
        .into_iter()
        .map(|t| row(t, false))
        .collect();
    // A one-off, alone among the rows: it moves the account's value without being money
    // earned or spent, so balances count it and income reports don't.
    rows.extend(opening.map(|t| row(t, true)));

    let (imported, skipped) = st
        .providers
        .import_transactions(account.id, &account.currency_code, &provider_tag, &rows)
        .await?;
    result.imported = imported;
    result.skipped = skipped;

    // Does the account's ledger now land on the balance it is recorded at? An opening balance
    // is worked back from what the *export* says, so it is only exact if Sure holds the same
    // movements the bank does for the period the export covers. Where a live feed owns part of
    // that window and its rows disagree, the difference shows up here and nowhere else.
    let ledger_sum = st.transactions.sum_amount_minor(account.id).await?;
    result.ledger_sum_minor = Some(ledger_sum);
    if let Some(balance) = account_balance_minor {
        if ledger_sum != balance {
            result.warnings.push(format!(
                "{}'s transactions now sum to {} but the account is recorded at {}, a difference \
                 of {} — some period is either counted twice or missing, so the reconstructed \
                 history before today will be out by that much",
                account.name,
                major(ledger_sum),
                major(balance),
                major(balance - ledger_sum),
            ));
        }
    }

    // Transfer auto-linking is not done here: the counterparty account is often imported
    // later in the same upload, so it wouldn't exist to match yet.
    // `sure_app::tasks::transfer_link` reconciles both sides regardless of order.

    Ok(result)
}

/// One parsed row, in the shape the DAL inserts.
fn row(t: sure_app::ports::ProviderTransaction, is_one_off: bool) -> sure_app::ports::ImportRow {
    sure_app::ports::ImportRow {
        external_id: t.external_id,
        posted_at: t.posted_at,
        amount_minor: t.amount_minor,
        currency_code: t.currency_code,
        description: t.description,
        merchant: t.merchant,
        category_name: t.category.as_ref().map(|c| c.name.clone()),
        category_kind: t.category.as_ref().and_then(|c| c.kind),
        category_group: t.category.and_then(|c| c.group),
        is_one_off,
    }
}

/// The parts of a result that don't depend on whether anything was written.
fn describe(
    export: &AsbExport,
    account: Option<&Account>,
    matched_by: Option<AsbMatch>,
    dry_run: bool,
    warnings: Vec<String>,
) -> AsbImportResult {
    AsbImportResult {
        dry_run,
        imported: 0,
        skipped: 0,
        would_import: export.transactions.len() as i64,
        held_back: export.held_back,
        cutover: None,
        rows_total: export.rows_total,
        asb_account: export.account.clone(),
        account_id: account.map(|a| a.id),
        account_name: account.map(|a| a.name.clone()),
        matched_by,
        sources: export.sources.clone(),
        product: export.product.clone(),
        covered_from: export.covered_from.clone(),
        covered_to: export.covered_to.clone(),
        ledger_balance_minor: export.ledger_balance_minor,
        account_balance_minor: None,
        implied_opening_minor: export.implied_opening_minor(),
        // Filled in by the caller, which is what decides whether one is recorded.
        opening_balance_minor: None,
        opening_balance_as_of: None,
        ledger_sum_minor: None,
        warnings,
    }
}

// --------------------------------------------------------------------------------------
// routing an export to an account
// --------------------------------------------------------------------------------------

/// Which Sure account an ASB account number belongs to, and on what evidence. In priority
/// order: what the request said, then a previous import of that same ASB account, then the
/// account's stored number, then its name. Only an unambiguous match counts — two accounts
/// answering to one number resolves to neither.
fn resolve<'a>(
    asb_account: &str,
    accounts: &'a [Account],
    assigned: &HashMap<String, i64>,
    prior: &HashMap<String, i64>,
) -> Option<(&'a Account, Option<AsbMatch>)> {
    let by_id =
        |id: i64, how: AsbMatch| accounts.iter().find(|a| a.id == id).map(|a| (a, Some(how)));
    if let Some(found) = assigned
        .get(asb_account)
        .and_then(|id| by_id(*id, AsbMatch::Assigned))
    {
        return Some(found);
    }
    if let Some(found) = prior
        .get(asb_account)
        .and_then(|id| by_id(*id, AsbMatch::PreviousImport))
    {
        return Some(found);
    }
    if let Some(found) = only_match(accounts, |a| {
        stored_number(a).is_some_and(|n| n == asb_account)
    }) {
        return Some((found, Some(AsbMatch::AccountNumber)));
    }
    // The distinctive tail — `0000123-50` — is what a name carries when two accounts would
    // otherwise be indistinguishable. Ten characters, so a coincidental hit is unlikely; a
    // hint all the same, which is why it's reported as one.
    let tail = asb_account.splitn(3, '-').nth(2)?;
    only_match(accounts, |a| a.name.contains(tail)).map(|a| (a, Some(AsbMatch::AccountName)))
}

/// The one account satisfying `pred`, or `None` if none or several do.
fn only_match(accounts: &[Account], pred: impl Fn(&Account) -> bool) -> Option<&Account> {
    let mut hits = accounts.iter().filter(|a| pred(a));
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// The account number a depository account records, if it records one. Matched exhaustively
/// so a new metadata profile has to decide whether it has one (CLAUDE.md rule 2).
fn stored_number(account: &Account) -> Option<&str> {
    use AccountMetadata::*;
    match &account.metadata {
        Depository(meta) => meta.account_number.as_deref(),
        Property(_) | Mortgage(_) | Loan(_) | Vehicle(_) | Shares(_) | Brokerage(_) | Crypto(_)
        | Generic(_) => None,
    }
}

/// `12-3136-0000123-50:8,12-3136-0000123-51:12` → the pairs it names.
fn parse_assignments(raw: Option<&str>) -> AppResult<HashMap<String, i64>> {
    let mut out = HashMap::new();
    for pair in raw.unwrap_or_default().split(',').filter(|p| !p.is_empty()) {
        let (asb, id) = pair.rsplit_once(':').ok_or_else(|| {
            AppError::validation(format!(
                "'{pair}' is not an assignment — expected <asb account>:<account id>"
            ))
        })?;
        let id: i64 = id.trim().parse().map_err(|_| {
            AppError::validation(format!("'{id}' in '{pair}' is not an account id"))
        })?;
        out.insert(asb.trim().to_string(), id);
    }
    Ok(out)
}

/// Which ASB account number each account has already had imported, recovered from the
/// `asb:<number>:<row id>` external ids those imports wrote. The mapping has no table of its
/// own — the ids *are* the record — so a repeat upload of the same exports routes itself.
async fn prior_imports(st: &AppState) -> AppResult<HashMap<String, i64>> {
    Ok(st
        .transactions
        .sample_external_ids(TAG)
        .await?
        .into_iter()
        .filter_map(|(account_id, external_id)| {
            let rest = external_id.strip_prefix("asb:")?;
            let (asb_account, _) = rest.rsplit_once(':')?;
            Some((asb_account.to_string(), account_id))
        })
        .collect())
}

fn guard_kind(account: &Account) -> AppResult<()> {
    if accepts_asb_csv(account.kind) {
        return Ok(());
    }
    Err(AppError::validation(
        "an ASB transaction export can only be imported into a cash, bank, savings, credit \
         card, or revolving credit account",
    ))
}

fn provider_tag(account_id: i64) -> String {
    format!("{TAG}{account_id}")
}

/// The account's most recent recorded balance, if it has one. Best-effort: it only feeds a
/// warning, so a read failure must not fail the import.
async fn latest_balance(st: &AppState, account_id: i64) -> Option<i64> {
    match st.valuations.list_for_account(account_id).await {
        // Newest first, per `valuations::list_for_account`.
        Ok(vals) => vals.first().map(|v| v.value_minor),
        Err(e) => {
            tracing::warn!(account_id, error = %e, "asb import: could not read the account's balance");
            None
        }
    }
}

/// Minor units as a plain decimal string, for a message a person reads.
fn major(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

pub fn router(limits: &Limits) -> Router<AppState> {
    let body_limit = DefaultBodyLimit::max(limits.max_import_body_bytes);
    Router::new()
        .route(
            "/accounts/{id}/asb/import",
            post(import).delete(undo).layer(body_limit),
        )
        .route("/asb/import", post(upload).layer(body_limit))
}
