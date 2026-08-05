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
//! Where that read can't answer the question — a feed is connected but hasn't posted anything
//! yet, or the date it did post won't parse — the upload is *refused* (see [`decide_cutover`])
//! rather than imported whole. "Nothing else posts here" and "we couldn't tell" would
//! otherwise produce the same silent `cutover: None`, and the cost of guessing wrong is a
//! permanently doubled ledger against the cost of one re-upload.
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

use std::collections::{HashMap, HashSet};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::post;
use axum::Router;
use chrono::NaiveDate;
use serde::Deserialize;
use sure_app::reports::ReportQuery;
use sure_core::{Account, AccountKind, AccountMetadata, AsbMatch, Provider};
use sure_providers::asb::AsbExport;
use utoipa::IntoParams;

use crate::config::Limits;
use crate::error::{AppError, AppResult};
use crate::extract::Json;
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
    // Only for the exports the deterministic tiers leave unmatched, and decided across the
    // whole upload at once so two exports can't claim one account — see `match_by_history`.
    let by_history = match_by_history(&st, &upload.exports, &accounts, &assigned, &prior).await?;

    let mut exports = Vec::new();
    for export in upload.exports {
        let matched = resolve(&export.account, &accounts, &assigned, &prior, &by_history);
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
    let cutover = cutover_for(st, account, &provider_tag).await?;
    export.hold_back_from(cutover);
    warnings.append(&mut export.warnings);

    let account_balance_minor = latest_balance(st, account).await;
    // The strongest signal available that this export belongs to this account and reaches
    // all the way back: ASB's stated closing balance against Sure's own. A warning, not a
    // refusal — a legitimate export can predate the latest valuation, and an account whose
    // history is only now being reconstructed genuinely holds less than the bank says.
    if let (Some(stated), Some(held)) = (export.ledger_balance_minor, account_balance_minor) {
        if stated != held {
            warnings.push(format!(
                "the export closes at {} but {} holds {} — check the export is for that \
                 account, and that its date range reaches back far enough; the two also differ \
                 while the account's own history is still incomplete",
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
    // Re-read rather than reuse the figure from before the insert. For an account whose
    // balance *is* its transaction sum — every kind this route accepts, unless a feed has
    // left a valuation — that earlier figure is now stale by exactly what was imported, so
    // comparing against it would warn on every successful import. Re-read, and the check
    // still bites wherever there is a balance recorded independently of these rows to
    // reconcile against, and is silent where there isn't one.
    if let Some(balance) = latest_balance(st, account).await {
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
/// account's stored number, then its name, then the transactions the account already holds.
/// Only an unambiguous match counts — two accounts answering to one number resolves to neither.
///
/// Content matching is last deliberately. Every tier above it is an *identifier*; matching rows
/// is inference, and while the inference is strong when there is a lot of it (see
/// [`match_by_history`]), a stored number that disagrees with it means something is wrong with
/// the number, which is a thing to tell the user rather than quietly route around.
fn resolve<'a>(
    asb_account: &str,
    accounts: &'a [Account],
    assigned: &HashMap<String, i64>,
    prior: &HashMap<String, i64>,
    by_history: &HashMap<String, i64>,
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
    if let Some(tail) = asb_account.splitn(3, '-').nth(2) {
        if let Some(found) = only_match(accounts, |a| a.name.contains(tail)) {
            return Some((found, Some(AsbMatch::AccountName)));
        }
    }
    by_history
        .get(asb_account)
        .and_then(|id| by_id(*id, AsbMatch::TransactionHistory))
}

/// How much agreement is enough to route an export by its contents alone.
mod history_match {
    /// Days either side a row may differ by and still be the same transaction. A feed and the
    /// bank's own export disagree about *when* far more often than about what: over one
    /// account's overlapping year, exact dates matched 1 row in 161 while a single day's
    /// tolerance matched 161 of 161, because Akahu stamps these a day earlier than ASB does.
    ///
    /// One day, not more. The same comparison over a busier account showed a tail of genuine
    /// +6 and +13 day offsets, and widening far enough to catch those starts matching rows that
    /// merely happen to share an amount — which is the failure that misfiles an account.
    pub const DAY_TOLERANCE: i64 = 1;
    /// Matched rows needed before a match counts at all, however good the rate looks. This is
    /// the guard that matters: a savings export scored a *perfect* 2 of 2 against the chequing
    /// account — a transfer pair, seen from both sides — and on rate alone that would have filed
    /// seven years of one account's history into another.
    pub const MIN_ROWS: usize = 10;
    /// …and the share of the overlapping window that must match, so a long run of coincidences
    /// can't win either.
    pub const MIN_RATE: f64 = 0.8;
    /// Existing rows read per upload. Comfortably past a decade of a busy account, and bounds
    /// the comparison whatever is in the database.
    pub const MAX_ROWS_READ: i64 = 200_000;
}

/// Route the exports nothing else could place, by matching their rows against the transactions
/// each candidate account already holds.
///
/// Signed amounts, and a day's tolerance on the date (see [`history_match`]). Signs matter: a
/// transfer out of one account is an inflow to another, so comparing magnitudes would make the
/// two sides of every internal transfer look like each other.
///
/// Decided for the whole upload at once rather than per export, because the constraint that
/// makes it safe is a global one — **one account cannot be the answer to two exports**. Highest
/// evidence wins first, and each winner takes its account out of the running, so a thin export
/// can't claim an account a 997-row match has already earned.
async fn match_by_history(
    st: &AppState,
    exports: &[AsbExport],
    accounts: &[Account],
    assigned: &HashMap<String, i64>,
    prior: &HashMap<String, i64>,
) -> AppResult<HashMap<String, i64>> {
    // Anything an identifier already placed is out of the running on both sides: its export
    // needs no guess, and its account is taken.
    let placed: HashSet<i64> = accounts
        .iter()
        .filter(|a| {
            let claimed = |m: &HashMap<String, i64>| m.values().any(|id| *id == a.id);
            claimed(assigned) || claimed(prior) || stored_number(a).is_some()
        })
        .map(|a| a.id)
        .collect();
    let unplaced: Vec<&AsbExport> = exports
        .iter()
        .filter(|e| !assigned.contains_key(&e.account) && !prior.contains_key(&e.account))
        .collect();
    let candidates: Vec<i64> = accounts
        .iter()
        .map(|a| a.id)
        .filter(|id| !placed.contains(id))
        .collect();
    if unplaced.is_empty() || candidates.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = st
        .transactions
        .amounts_for_matching(&candidates, history_match::MAX_ROWS_READ)
        .await?;
    // `amount -> dates`, per account: the shape the scoring walks.
    let mut have: HashMap<i64, HashMap<i64, Vec<NaiveDate>>> = HashMap::new();
    for (account_id, date, amount_minor) in rows {
        if let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
            have.entry(account_id)
                .or_default()
                .entry(amount_minor)
                .or_default()
                .push(date);
        }
    }

    // Every (export, account) pair worth considering, best evidence first.
    let mut scored: Vec<(usize, &str, i64)> = Vec::new();
    for export in &unplaced {
        for account_id in &candidates {
            let Some(theirs) = have.get(account_id) else {
                continue;
            };
            let (matched, window) = score_history(export, theirs);
            let rate = if window == 0 {
                0.0
            } else {
                matched as f64 / window as f64
            };
            if matched >= history_match::MIN_ROWS && rate >= history_match::MIN_RATE {
                scored.push((matched, export.account.as_str(), *account_id));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)).then(a.2.cmp(&b.2)));

    let mut out: HashMap<String, i64> = HashMap::new();
    let mut taken: HashSet<i64> = HashSet::new();
    for (matched, asb_account, account_id) in scored {
        if out.contains_key(asb_account) || taken.contains(&account_id) {
            continue;
        }
        tracing::debug!(
            asb_account,
            account_id,
            matched,
            "routed an ASB export by its transaction history"
        );
        out.insert(asb_account.to_string(), account_id);
        taken.insert(account_id);
    }
    Ok(out)
}

/// `(matched, window)` — how many of this export's rows are already on the account, out of how
/// many fall inside the window the account's own rows cover. Greedy and one-to-one: each
/// existing row can be claimed once, so a repeated amount can't match itself many times over.
fn score_history(export: &AsbExport, theirs: &HashMap<i64, Vec<NaiveDate>>) -> (usize, usize) {
    let Some((first, last)) =
        theirs
            .values()
            .flatten()
            .fold(None, |acc: Option<(NaiveDate, NaiveDate)>, d| match acc {
                None => Some((*d, *d)),
                Some((lo, hi)) => Some((lo.min(*d), hi.max(*d))),
            })
    else {
        return (0, 0);
    };
    let mut used: HashMap<i64, HashSet<usize>> = HashMap::new();
    let (mut matched, mut window) = (0usize, 0usize);
    for row in &export.transactions {
        let Some(date) = row
            .posted_at
            .get(..10)
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        else {
            continue;
        };
        if date < first || date > last {
            continue;
        }
        window += 1;
        let Some(dates) = theirs.get(&row.amount_minor) else {
            continue;
        };
        let spent = used.entry(row.amount_minor).or_default();
        for (i, theirs_date) in dates.iter().enumerate() {
            if spent.contains(&i) {
                continue;
            }
            if (*theirs_date - date).num_days().abs() <= history_match::DAY_TOLERANCE {
                spent.insert(i);
                matched += 1;
                break;
            }
        }
    }
    (matched, window)
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
        Property(_) | Mortgage(_) | Loan(_) | StudentLoan(_) | Vehicle(_) | Shares(_)
        | Brokerage(_) | Crypto(_) | Generic(_) => None,
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

/// The date from which another feed already owns this account's movements — everything from
/// it is the other feed's to post, and this import stops there. `None` only when nothing else
/// posts to the account at all.
///
/// Reads the ledger, then the provider list, because the ledger alone cannot tell the two
/// apart: see [`decide_cutover`], which makes the decision.
async fn cutover_for(
    st: &AppState,
    account: &Account,
    provider_tag: &str,
) -> AppResult<Option<NaiveDate>> {
    let earliest = st
        .transactions
        .earliest_posted_at_from_other_feed(account.id, provider_tag)
        .await?;
    // Listed unconditionally rather than only when the ledger came back empty: it is a
    // handful of rows, and keeping the two reads together keeps the decision in one pure
    // function that a test can drive.
    let providers = st.providers.list().await?;
    decide_cutover(
        account.id,
        &account.name,
        earliest.as_deref(),
        &providers,
        |kind| st.provider_registry.get(kind).is_some(),
    )
}

/// The cutover decision, from what the ledger and the provider list say. Pure, so both ways
/// of *not* knowing stay covered by a unit test rather than by a live database.
///
/// `earliest_from_other_feed` is `MIN(posted_at)` over the rows some other feed wrote.
/// `supplies_transactions` answers whether a provider kind posts transactions at all (the
/// registry): a row whose kind is no longer registered has nothing pending to post.
///
/// Neither way of failing may return `None`. `None` means "no other feed owns any of this
/// account", which holds nothing back and imports the file whole — and the only warning on
/// this path fires on rows that *were* held back, so a failure to establish the cutover would
/// be entirely silent. Both are therefore a 422 before anything is written:
///
/// * A connected, enabled feed with no rows yet. `routes::providers::link` deliberately keeps
///   a link whose first sync failed, so this is also the state for the seconds after linking
///   and for as long as credentials are wrong. Import seven years into that window and Akahu's
///   next poll lands its own two on top of the same two: dedupe is `(provider, external_id)`
///   and cannot see across `asb#N` and `akahu#M`, so every transaction in the overlap exists
///   twice, permanently.
/// * A `posted_at` that won't parse. It is `MIN()` under SQLite's BINARY collation, where a
///   non-ISO date sorts ahead of every ISO one (`'0' < '2'`), so a single legacy row is
///   exactly the one that decides the window — and an unreadable date in the account's
///   history is a defect worth surfacing in its own right.
fn decide_cutover(
    account_id: i64,
    account_name: &str,
    earliest_from_other_feed: Option<&str>,
    providers: &[Provider],
    supplies_transactions: impl Fn(&str) -> bool,
) -> AppResult<Option<NaiveDate>> {
    if let Some(at) = earliest_from_other_feed {
        // Stored as a full timestamp; the cutover is a whole day.
        return match NaiveDate::parse_from_str(at.get(..10).unwrap_or(at), "%Y-%m-%d") {
            Ok(date) => Ok(Some(date)),
            Err(_) => Err(AppError::validation(format!(
                "{account_name}'s earliest transaction from another feed is dated '{at}', which \
                 is not a date this import can read, so it cannot tell which period that feed \
                 owns — correct that row's date, then import again"
            ))),
        };
    }

    let waiting: Vec<&str> = providers
        .iter()
        .filter(|p| p.account_id == account_id && p.enabled && supplies_transactions(&p.kind))
        .map(|p| p.name.as_str())
        .collect();
    if waiting.is_empty() {
        return Ok(None);
    }
    Err(AppError::validation(format!(
        "{account_name} is connected to {}, which has not posted a transaction yet, so this \
         import cannot tell which period belongs to it — importing now would count that period \
         twice once it syncs. Sync it (or disable it), then import again",
        waiting.join(", ")
    )))
}

/// The account's current balance, if it has one, as the balances report derives it: its newest
/// valuation on or before today, else the running sum of its transactions
/// (`sure_app::reports::account_value_at`). Taken from the report service rather than
/// re-derived here, so the figure an export is checked against is the one the account page
/// shows.
///
/// It has to be that derivation and not the newest valuation alone: every kind
/// [`accepts_asb_csv`] admits seeds its opening balance as a *transaction* (the DAL's
/// `opening_balance_ledger`) and accumulates from its own transaction stream, so such an
/// account has no valuation at all unless a provider sync wrote one — and reading valuations
/// meant the reconciliation this feeds, the strongest wrong-account signal on this path, never
/// ran for the only kinds the route accepts.
///
/// `None` — no comparison, no warning — in four cases, each of which has nothing to compare:
/// a read failure (this only feeds a warning, so it must not fail the import); an archived
/// account, which the balances report doesn't cover; a balance recorded in another currency,
/// where minor units against the export's would be arithmetic on two different things; and a
/// zero, which on this path is the absence of a balance rather than a balance of zero — an
/// account nothing has been recorded on yet derives 0 from an empty ledger, and "the export
/// closes at 3,412.09 but the account holds 0.00" on every first import would train the reader
/// to ignore the one that matters.
async fn latest_balance(st: &AppState, account: &Account) -> Option<i64> {
    let report = match st.reports.balances(&ReportQuery::default()).await {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(account_id = account.id, error = %e, "asb import: could not read the account's balance");
            return None;
        }
    };
    report
        .accounts
        .into_iter()
        .find(|a| a.account_id == account.id && a.currency_code == account.currency_code)
        .map(|a| a.value_minor)
        .filter(|value| *value != 0)
}

/// Minor units as a plain decimal string, for a message a person reads.
fn major(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

pub fn router(limits: &Limits) -> Router<AppState> {
    Router::new()
        .route(
            "/accounts/{id}/asb/import",
            // The raised limit is layered onto the upload only, then `delete` is added after
            // it, so undo keeps the global body limit: it takes no body, and a 50 MiB
            // allowance on a route that ignores what arrives is an allowance nobody chose.
            post(import)
                .layer(DefaultBodyLimit::max(limits.max_import_body_bytes))
                .delete(undo),
        )
        .route(
            "/asb/import",
            post(upload).layer(DefaultBodyLimit::max(limits.max_import_body_bytes)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider row in the shape the DAL hands one back. Only the four fields the cutover
    /// decision reads carry anything; no real identifier belongs in a fixture (CLAUDE.md
    /// rule 3).
    fn provider(name: &str, kind: &str, account_id: i64, enabled: bool) -> Provider {
        Provider {
            id: 1,
            name: name.to_string(),
            kind: kind.to_string(),
            account_id,
            config: serde_json::Value::Null,
            enabled,
            last_synced_at: None,
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
        }
    }

    /// Every kind these tests name is one the registry knows, unless a test says otherwise.
    fn registered(_kind: &str) -> bool {
        true
    }

    fn refused(earliest: Option<&str>, providers: &[Provider]) -> String {
        let err = decide_cutover(8, "Everyday", earliest, providers, registered)
            .expect_err("the import should have been refused");
        // A 422, not a 500: the upload is wrong (or premature), not the server.
        assert!(
            matches!(err, AppError::Validation(_)),
            "expected a validation error, got {err:?}"
        );
        err.to_string()
    }

    #[test]
    fn a_timestamp_from_another_feed_becomes_a_whole_day_cutover() {
        let got = decide_cutover(
            8,
            "Everyday",
            Some("2022-01-01T12:00:00+00:00"),
            &[],
            registered,
        )
        .expect("a parseable date is not a refusal");
        assert_eq!(got, NaiveDate::from_ymd_opt(2022, 1, 1));
    }

    #[test]
    fn an_account_nothing_else_posts_to_has_no_cutover() {
        let got = decide_cutover(8, "Everyday", None, &[], registered)
            .expect("no other feed is a legitimate answer, not a refusal");
        assert_eq!(got, None);
    }

    #[test]
    fn an_enabled_feed_that_has_posted_nothing_yet_refuses_the_import() {
        // The state after a link whose first sync failed (`routes::providers::link` keeps the
        // link deliberately), and the one where importing the file whole doubles every row
        // that feed later posts for the same period.
        let msg = refused(None, &[provider("Akahu", "akahu", 8, true)]);
        assert!(msg.contains("Akahu"), "the feed to sync is named: {msg}");
        assert!(msg.contains("Everyday"), "the account is named: {msg}");
    }

    #[test]
    fn every_waiting_feed_is_named_so_the_user_syncs_all_of_them() {
        let msg = refused(
            None,
            &[
                provider("Akahu", "akahu", 8, true),
                provider("Statements", "csv", 8, true),
            ],
        );
        assert!(msg.contains("Akahu") && msg.contains("Statements"), "{msg}");
    }

    #[test]
    fn a_disabled_feed_or_one_on_another_account_is_not_waiting() {
        let providers = [
            provider("Akahu", "akahu", 8, false),
            provider("Akahu", "akahu", 9, true),
        ];
        let got = decide_cutover(8, "Everyday", None, &providers, registered)
            .expect("neither of these posts to account 8");
        assert_eq!(got, None);
    }

    #[test]
    fn a_feed_whose_kind_is_no_longer_registered_is_not_waiting() {
        // Nothing can sync it, so it has no window pending and must not block the import.
        let providers = [provider("Retired", "decommissioned", 8, true)];
        let got = decide_cutover(8, "Everyday", None, &providers, |_| false)
            .expect("an unregistered kind posts nothing");
        assert_eq!(got, None);
    }

    #[test]
    fn a_posted_at_that_is_not_a_date_refuses_the_import() {
        // A CSV provider stores the date cell verbatim, and `MIN()` under SQLite's BINARY
        // collation sorts a `0`-leading day ahead of every ISO date — so this one row is
        // exactly the one that decides the window, and it can't.
        let msg = refused(Some("03/07/2019"), &[]);
        assert!(
            msg.contains("03/07/2019"),
            "the offending value is quoted: {msg}"
        );
    }

    #[test]
    fn a_posted_at_too_short_to_hold_a_date_refuses_rather_than_panics() {
        assert!(refused(Some("2019-07"), &[]).contains("2019-07"));
    }

    #[test]
    fn a_posted_at_with_no_char_boundary_at_ten_refuses_rather_than_panics() {
        // `str::get` returns `None` mid-codepoint rather than panicking the way slicing
        // would; the point of this test is that the handler stays a 422 either way.
        assert!(!refused(Some("2019-07-0\u{1f600}3"), &[]).is_empty());
    }

    // ------------------------------------------- routing an export by its transaction history

    /// An export holding `rows` of `(date, amount)`. Only the fields the scoring reads carry
    /// anything, and no real identifier belongs in a fixture (CLAUDE.md rule 3).
    fn export_of(rows: &[(&str, i64)]) -> AsbExport {
        AsbExport {
            account: "12-3136-0000123-50".to_string(),
            transactions: rows
                .iter()
                .map(
                    |(date, amount_minor)| sure_app::ports::ProviderTransaction {
                        external_id: format!("asb:x:{date}{amount_minor}"),
                        posted_at: format!("{date}T12:00:00+00:00"),
                        amount_minor: *amount_minor,
                        currency_code: None,
                        description: "X".to_string(),
                        merchant: None,
                        category: None,
                    },
                )
                .collect(),
            ..AsbExport::default()
        }
    }

    /// What an account already holds, in the shape the scoring walks.
    fn holds(rows: &[(&str, i64)]) -> HashMap<i64, Vec<NaiveDate>> {
        let mut out: HashMap<i64, Vec<NaiveDate>> = HashMap::new();
        for (date, amount_minor) in rows {
            out.entry(*amount_minor)
                .or_default()
                .push(NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("fixture date parses"));
        }
        out
    }

    /// The measurement this whole tier rests on: a feed and the bank's own export disagree about
    /// *when*, routinely by a day, and not about the amount. Exact dates score close to nothing.
    #[test]
    fn a_days_tolerance_is_what_makes_the_match_work_at_all() {
        let export = export_of(&[
            ("2025-08-04", 77_184),
            ("2025-08-06", -3_280),
            ("2025-08-11", -77_184),
        ]);
        // The same three transactions, each stamped a day earlier — as Akahu stamps them —
        // plus one of the account's own the export doesn't have, so the window it covers
        // reaches past the export's last row. The window is the *account's* span by design.
        let theirs = holds(&[
            ("2025-08-03", 77_184),
            ("2025-08-05", -3_280),
            ("2025-08-10", -77_184),
            ("2025-08-20", -9_99),
        ]);
        assert_eq!(score_history(&export, &theirs), (3, 3));

        // Three days out is not the same transaction, and must not be treated as one.
        let far = holds(&[
            ("2025-07-31", 77_184),
            ("2025-08-02", -3_280),
            ("2025-08-07", -77_184),
            ("2025-08-20", -9_99),
        ]);
        assert_eq!(score_history(&export, &far).0, 0);
    }

    /// Signed, so the two sides of an internal transfer don't look like each other — the same
    /// amount on the same day with the sign flipped is the shape every transfer has.
    #[test]
    fn an_inflow_does_not_match_the_outflow_it_mirrors() {
        let export = export_of(&[("2025-08-13", -2_954_15)]);
        let theirs = holds(&[("2025-08-13", 2_954_15)]);
        assert_eq!(score_history(&export, &theirs), (0, 1));
    }

    /// One-to-one: a repeated amount can't match one existing row over and over, which would let
    /// a standing order inflate a score until it looked like proof.
    #[test]
    fn one_existing_row_is_claimed_only_once() {
        let export = export_of(&[
            ("2025-08-04", -20_00),
            ("2025-08-04", -20_00),
            ("2025-08-04", -20_00),
        ]);
        let theirs = holds(&[("2025-08-04", -20_00)]);
        assert_eq!(score_history(&export, &theirs), (1, 3));
    }

    /// Only the window the account's own rows cover is scored — an export reaching back seven
    /// years must not be marked down for the six the feed never had.
    #[test]
    fn rows_outside_the_accounts_own_window_are_not_counted_against_it() {
        let export = export_of(&[
            ("2019-01-01", -1_00), // long before the feed
            ("2025-08-04", -20_00),
            ("2026-09-09", -3_00), // long after it
        ]);
        let theirs = holds(&[("2025-08-04", -20_00)]);
        // One row in the window, and it matched: a clean 1/1 rather than 1/3.
        assert_eq!(score_history(&export, &theirs), (1, 1));
    }

    /// The guard that matters most. A savings export scored a *perfect* 2 of 2 against the
    /// chequing account on a transfer pair seen from both sides; on rate alone that would have
    /// filed seven years of one account's history into another.
    #[test]
    fn a_perfect_score_on_too_few_rows_is_not_enough() {
        let export = export_of(&[("2025-08-04", -500_00), ("2025-08-05", -250_00)]);
        let theirs = holds(&[("2025-08-04", -500_00), ("2025-08-05", -250_00)]);
        let (matched, window) = score_history(&export, &theirs);
        assert_eq!((matched, window), (2, 2), "it really does score 100%");
        assert!(
            matched < history_match::MIN_ROWS,
            "and is refused anyway, on {matched} rows of evidence"
        );
    }

    /// The rate floor is the second guard, and it bites where the row floor doesn't: enough
    /// matches to clear `MIN_ROWS`, but most of the window unaccounted for.
    #[test]
    fn a_low_rate_over_enough_rows_is_still_not_enough() {
        // Forty days, forty distinct amounts.
        let dates: Vec<String> = (1..=40)
            .map(|d| {
                NaiveDate::from_ymd_opt(2025, 8, 1)
                    .and_then(|s| s.checked_add_days(chrono::Days::new(d)))
                    .expect("in range")
                    .to_string()
            })
            .collect();
        let rows: Vec<(&str, i64)> = dates
            .iter()
            .enumerate()
            .map(|(i, d)| (d.as_str(), -1_00 * (i as i64 + 1)))
            .collect();
        let export = export_of(&rows);
        // Twelve of the forty are on the account, and its last row is the export's last, so the
        // whole export sits inside the window it covers.
        let mut theirs_rows = rows[..12].to_vec();
        theirs_rows.push(rows[39]);
        let theirs = holds(&theirs_rows);

        let (matched, window) = score_history(&export, &theirs);
        assert_eq!(
            window, 40,
            "the whole export is inside the account's window"
        );
        assert!(
            matched >= history_match::MIN_ROWS,
            "{matched} matches clears the row floor, so only the rate can refuse this"
        );
        let rate = matched as f64 / window as f64;
        assert!(
            rate < history_match::MIN_RATE,
            "a rate of {rate} should be refused"
        );
    }
}
