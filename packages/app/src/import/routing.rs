//! Deciding which Sure account a thing inside an upload belongs to.
//!
//! Nothing inside a bank export names a Sure account — the file knows `12-3456-0000123-50`
//! and Sure knows "Emergency Fund" — so every import has to bridge that gap or ask. The tiers
//! below are ordered by how much they *prove*, and only an unambiguous answer counts: two
//! accounts answering to one number resolves to neither.
//!
//! Content matching is last deliberately. Every tier above it is an *identifier*; matching
//! rows is inference, and while the inference is strong when there is a lot of it (see
//! [`match_by_history`]), a stored number that disagrees with it means something is wrong with
//! the number, which is a thing to tell someone rather than quietly route around.
//!
//! Lifted from `sure_api::routes::asb`, where it grew, and widened from "an ASB export" to
//! "any parsed item" so every source routes the same way.

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use sure_core::{Account, AccountMetadata, AppError, AppResult, ImportMatch, ImportSource};

use crate::ports::{ImportAdapter, ParsedItem, TransactionRepo};

/// How much agreement is enough to route an item by its contents alone.
pub mod history_match {
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

/// Everything the resolver knows before it looks at any one item, gathered once per upload.
pub struct Routing {
    /// What the request said outright.
    pub assigned: HashMap<String, i64>,
    /// What a previous import of the same source account settled on.
    pub prior: HashMap<String, i64>,
    /// What the rows themselves suggest, for the items nothing above could place.
    pub by_history: HashMap<String, i64>,
}

/// Which Sure account a source account belongs to, and on what evidence. In priority order:
/// what the request said, then a previous import of that same source account, then the
/// account's stored number, then its name, then the one candidate if there is exactly one,
/// then the transactions the account already holds.
pub fn resolve<'a>(
    source_account: &str,
    accounts: &'a [Account],
    routing: &Routing,
) -> Option<(&'a Account, ImportMatch)> {
    let by_id = |id: i64, how: ImportMatch| accounts.iter().find(|a| a.id == id).map(|a| (a, how));
    if let Some(found) = routing
        .assigned
        .get(source_account)
        .and_then(|id| by_id(*id, ImportMatch::Assigned))
    {
        return Some(found);
    }
    if let Some(found) = routing
        .prior
        .get(source_account)
        .and_then(|id| by_id(*id, ImportMatch::PreviousImport))
    {
        return Some(found);
    }
    if let Some(found) = only_match(accounts, |a| {
        stored_number(a).is_some_and(|n| n == source_account)
    }) {
        return Some((found, ImportMatch::AccountNumber));
    }
    // The distinctive tail — `0000123-50` — is what a name carries when two accounts would
    // otherwise be indistinguishable. Ten characters, so a coincidental hit is unlikely; a
    // hint all the same, which is why it's reported as one.
    if let Some(tail) = source_account.splitn(3, '-').nth(2) {
        if let Some(found) = only_match(accounts, |a| a.name.contains(tail)) {
            return Some((found, ImportMatch::AccountName));
        }
    }
    routing
        .by_history
        .get(source_account)
        .and_then(|id| by_id(*id, ImportMatch::TransactionHistory))
}

/// The sole account this source could mean, when the upload describes exactly one thing and
/// exactly one account accepts that source.
///
/// The narrow case a per-account route used to cover for free by having the account in its
/// path: a household has one student loan and one brokerage, and an export of either names
/// something (`012-345-678-SLS004`) that matches no Sure field at all, so every tier above
/// returns nothing and the upload would be reported as unroutable while the answer is the only
/// one there is. Both conditions are load-bearing — with two loans it declines, because then
/// picking would be guessing with someone's money.
pub fn only_candidate(
    source: ImportSource,
    items: usize,
    accounts: &[Account],
) -> Option<(&Account, ImportMatch)> {
    if items != 1 || !source.routes_by_sole_candidate() {
        return None;
    }
    only_match(accounts, |a| source.accepts(a.kind)).map(|a| (a, ImportMatch::OnlyCandidate))
}

/// Refuse an assignment that names an account this upload cannot go to.
///
/// The per-account routes this replaced fetched the account first, so a bad id was a 404 and a
/// wrong kind a 422 — before a byte was read. An assignment is the same statement of intent, so it
/// gets the same treatment: falling through to the other tiers would quietly import somewhere the
/// caller didn't ask for, or nowhere, and report success either way.
///
/// `accounts` is every account, not just the accepting ones, so "that isn't the right kind of
/// account" can be told apart from "there is no such account".
pub fn check_assignments(
    source: ImportSource,
    assigned: &HashMap<String, i64>,
    accounts: &[Account],
) -> AppResult<()> {
    for id in assigned.values() {
        let Some(account) = accounts.iter().find(|a| a.id == *id) else {
            return Err(AppError::NotFound("account"));
        };
        if !source.accepts(account.kind) {
            return Err(AppError::validation(format!(
                "a {} can't be imported into {} — it is a {} account",
                source.label(),
                account.name,
                account.kind.as_str()
            )));
        }
    }
    Ok(())
}

/// Route the items nothing else could place, by matching their rows against the transactions
/// each candidate account already holds.
///
/// Signed amounts, and a day's tolerance on the date (see [`history_match`]). Signs matter: a
/// transfer out of one account is an inflow to another, so comparing magnitudes would make the
/// two sides of every internal transfer look like each other.
///
/// Decided for the whole upload at once rather than per item, because the constraint that
/// makes it safe is a global one — **one account cannot be the answer to two items**. Highest
/// evidence wins first, and each winner takes its account out of the running, so a thin item
/// can't claim an account a 997-row match has already earned.
pub async fn match_by_history(
    transactions: &dyn TransactionRepo,
    items: &[ParsedItem],
    accounts: &[Account],
    assigned: &HashMap<String, i64>,
    prior: &HashMap<String, i64>,
) -> AppResult<HashMap<String, i64>> {
    // Anything an identifier already placed is out of the running on both sides: its item
    // needs no guess, and its account is taken.
    let placed: HashSet<i64> = accounts
        .iter()
        .filter(|a| {
            let claimed = |m: &HashMap<String, i64>| m.values().any(|id| *id == a.id);
            claimed(assigned) || claimed(prior) || stored_number(a).is_some()
        })
        .map(|a| a.id)
        .collect();
    let unplaced: Vec<&ParsedItem> = items
        .iter()
        .filter(|i| {
            !assigned.contains_key(&i.source_account) && !prior.contains_key(&i.source_account)
        })
        .collect();
    let candidates: Vec<i64> = accounts
        .iter()
        .map(|a| a.id)
        .filter(|id| !placed.contains(id))
        .collect();
    if unplaced.is_empty() || candidates.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = transactions
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

    // Every (item, account) pair worth considering, best evidence first.
    let mut scored: Vec<(usize, &str, i64)> = Vec::new();
    for item in &unplaced {
        for account_id in &candidates {
            let Some(theirs) = have.get(account_id) else {
                continue;
            };
            let (matched, window) = score_history(item, theirs);
            let rate = if window == 0 {
                0.0
            } else {
                matched as f64 / window as f64
            };
            if matched >= history_match::MIN_ROWS && rate >= history_match::MIN_RATE {
                scored.push((matched, item.source_account.as_str(), *account_id));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)).then(a.2.cmp(&b.2)));

    let mut out: HashMap<String, i64> = HashMap::new();
    let mut taken: HashSet<i64> = HashSet::new();
    for (matched, source_account, account_id) in scored {
        if out.contains_key(source_account) || taken.contains(&account_id) {
            continue;
        }
        tracing::debug!(
            source_account,
            account_id,
            matched,
            "routed an uploaded export by its transaction history"
        );
        out.insert(source_account.to_string(), account_id);
        taken.insert(account_id);
    }
    Ok(out)
}

/// `(matched, window)` — how many of this item's rows are already on the account, out of how
/// many fall inside the window the account's own rows cover. Greedy and one-to-one: each
/// existing row can be claimed once, so a repeated amount can't match itself many times over.
fn score_history(item: &ParsedItem, theirs: &HashMap<i64, Vec<NaiveDate>>) -> (usize, usize) {
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
    for row in &item.rows {
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

/// `12-3456-0000123-50:8,12-3456-0000123-51:12` → the pairs it names.
pub fn parse_assignments(raw: Option<&str>) -> AppResult<HashMap<String, i64>> {
    let mut out = HashMap::new();
    for pair in raw.unwrap_or_default().split(',').filter(|p| !p.is_empty()) {
        let (source_account, id) = pair.rsplit_once(':').ok_or_else(|| {
            AppError::validation(format!(
                "'{pair}' is not an assignment — expected <source account>:<account id>"
            ))
        })?;
        let id: i64 = id.trim().parse().map_err(|_| {
            AppError::validation(format!("'{id}' in '{pair}' is not an account id"))
        })?;
        out.insert(source_account.trim().to_string(), id);
    }
    Ok(out)
}

/// Which source account each Sure account has already had imported, recovered from the
/// external ids those imports wrote. The mapping had no table of its own until `imports`
/// existed — the ids *are* the record for everything imported before it — so a repeat upload
/// of the same files still routes itself.
pub async fn prior_imports(
    transactions: &dyn TransactionRepo,
    adapter: &dyn ImportAdapter,
) -> AppResult<HashMap<String, i64>> {
    let prefix = format!("{}#", adapter.source().tag_stem());
    Ok(transactions
        .sample_external_ids(&prefix)
        .await?
        .into_iter()
        .filter_map(|(account_id, external_id)| {
            Some((adapter.source_account_of(&external_id)?, account_id))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ImportRow, ParsedExtras, ParsedUpload};
    use sure_core::{AccountKind, DepositoryMeta, Ownership};

    fn account(id: i64, name: &str, kind: AccountKind, number: Option<&str>) -> Account {
        Account {
            id,
            name: name.to_string(),
            kind,
            class: kind.class(),
            currency_code: "NZD".into(),
            institution: None,
            archived: false,
            metadata: match number {
                Some(n) => AccountMetadata::Depository(DepositoryMeta {
                    account_number: Some(n.to_string()),
                    ..Default::default()
                }),
                None => AccountMetadata::Depository(DepositoryMeta::default()),
            },
            sort_order: 0,
            secured_by_account_id: None,
            ownership: Ownership::Joint,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn item(source_account: &str, rows: Vec<(&str, i64)>) -> ParsedItem {
        ParsedItem {
            source_account: source_account.to_string(),
            label: None,
            sources: vec![],
            rows: rows
                .into_iter()
                .map(|(posted_at, amount_minor)| ImportRow {
                    external_id: format!("{posted_at}:{amount_minor}"),
                    posted_at: posted_at.to_string(),
                    amount_minor,
                    currency_code: None,
                    description: String::new(),
                    merchant: None,
                    category_name: None,
                    category_group: None,
                    category_kind: None,
                    is_one_off: false,
                })
                .collect(),
            covered_from: None,
            covered_to: None,
            stated_closing_minor: None,
            opening_balance: None,
            extras: ParsedExtras::None,
            warnings: vec![],
        }
    }

    fn empty() -> Routing {
        Routing {
            assigned: HashMap::new(),
            prior: HashMap::new(),
            by_history: HashMap::new(),
        }
    }

    /// A held date and a bank's own export disagree by a day far more often than they agree
    /// exactly — the tolerance is the whole reason content matching works at all.
    #[test]
    fn a_days_tolerance_is_what_makes_the_match_work_at_all() {
        // Twelve amounts the account already holds, all dated the 5th, plus a sentinel on the
        // 9th so the window the scoring considers reaches past the dates being tested. Without
        // it every row below would fall outside the account's own span and never be compared.
        let mut theirs: HashMap<i64, Vec<NaiveDate>> = HashMap::new();
        for d in 1..=12 {
            theirs
                .entry(d * 100)
                .or_default()
                .push(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        }
        theirs
            .entry(-1)
            .or_default()
            .push(NaiveDate::from_ymd_opt(2026, 1, 9).unwrap());

        // A day late — what Akahu against ASB's own export actually looks like. All twelve.
        let one_day = item(
            "12-3456-0000123-50",
            (1..=12).map(|d| ("2026-01-06", d * 100)).collect(),
        );
        assert_eq!(score_history(&one_day, &theirs), (12, 12));

        // Two days late, and none of them: the tolerance is one day exactly, because widening
        // it far enough to catch a genuine +6 offset starts matching rows that merely happen to
        // share an amount, which is the failure that misfiles a whole account.
        let two_days = item(
            "12-3456-0000123-50",
            (1..=12).map(|d| ("2026-01-07", d * 100)).collect(),
        );
        assert_eq!(score_history(&two_days, &theirs), (0, 12));
    }

    /// An outflow of 50 and an inflow of 50 are the two sides of one transfer, not the same
    /// row: comparing magnitudes would make every internal transfer look like a match.
    #[test]
    fn an_inflow_does_not_match_the_outflow_it_mirrors() {
        let it = item("12-3456-0000123-50", vec![("2026-01-05", 5000)]);
        let mut theirs: HashMap<i64, Vec<NaiveDate>> = HashMap::new();
        theirs
            .entry(-5000)
            .or_default()
            .push(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert_eq!(score_history(&it, &theirs).0, 0);
    }

    /// A repeated amount must not match one existing row over and over.
    #[test]
    fn one_existing_row_is_claimed_only_once() {
        let it = item(
            "12-3456-0000123-50",
            vec![("2026-01-05", 1000), ("2026-01-05", 1000)],
        );
        let mut theirs: HashMap<i64, Vec<NaiveDate>> = HashMap::new();
        theirs
            .entry(1000)
            .or_default()
            .push(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert_eq!(score_history(&it, &theirs), (1, 2));
    }

    /// Rows outside the window the account's own history covers are not evidence either way.
    #[test]
    fn rows_outside_the_accounts_own_window_are_not_counted() {
        let it = item(
            "12-3456-0000123-50",
            vec![("2020-01-01", 1000), ("2026-01-05", 1000)],
        );
        let mut theirs: HashMap<i64, Vec<NaiveDate>> = HashMap::new();
        theirs
            .entry(1000)
            .or_default()
            .push(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert_eq!(score_history(&it, &theirs), (1, 1));
    }

    /// The guard that matters most: a transfer pair seen from both sides is a *perfect* 2 of 2,
    /// and on rate alone it would file one account's whole history into another.
    #[test]
    fn a_perfect_score_on_too_few_rows_is_not_enough() {
        let (matched, window) = (2usize, 2usize);
        let rate = matched as f64 / window as f64;
        assert_eq!(rate, 1.0);
        assert!(matched < history_match::MIN_ROWS);
    }

    #[test]
    fn an_assignment_beats_every_other_tier() {
        let accounts = vec![
            account(1, "Everyday", AccountKind::Bank, Some("12-3456-0000123-50")),
            account(2, "Savings", AccountKind::Savings, None),
        ];
        let routing = Routing {
            assigned: HashMap::from([("12-3456-0000123-50".to_string(), 2)]),
            prior: HashMap::from([("12-3456-0000123-50".to_string(), 1)]),
            by_history: HashMap::new(),
        };
        let (found, how) = resolve("12-3456-0000123-50", &accounts, &routing).unwrap();
        assert_eq!(found.id, 2);
        assert_eq!(how, ImportMatch::Assigned);
    }

    #[test]
    fn a_stored_number_beats_a_name_that_merely_contains_the_tail() {
        let accounts = vec![
            account(1, "Everyday", AccountKind::Bank, Some("12-3456-0000123-50")),
            account(2, "Savings (0000123-50)", AccountKind::Savings, None),
        ];
        let (found, how) = resolve("12-3456-0000123-50", &accounts, &empty()).unwrap();
        assert_eq!(found.id, 1);
        assert_eq!(how, ImportMatch::AccountNumber);
    }

    /// Two accounts answering to one number resolves to neither — picking would be guessing.
    #[test]
    fn an_ambiguous_number_resolves_to_nothing() {
        let accounts = vec![
            account(1, "One", AccountKind::Bank, Some("12-3456-0000123-50")),
            account(2, "Two", AccountKind::Savings, Some("12-3456-0000123-50")),
        ];
        assert!(resolve("12-3456-0000123-50", &accounts, &empty()).is_none());
    }

    /// The tier that replaces having the account in the URL: one loan, one export, one answer.
    #[test]
    fn a_single_export_routes_to_the_only_account_of_its_kind() {
        let accounts = vec![
            account(1, "Everyday", AccountKind::Bank, None),
            account(2, "Student loan", AccountKind::StudentLoan, None),
        ];
        let (found, how) = only_candidate(ImportSource::MyirSls, 1, &accounts).unwrap();
        assert_eq!(found.id, 2);
        assert_eq!(how, ImportMatch::OnlyCandidate);
    }

    #[test]
    fn two_accounts_of_the_same_kind_decline_the_only_candidate_tier() {
        let accounts = vec![
            account(1, "Loan A", AccountKind::StudentLoan, None),
            account(2, "Loan B", AccountKind::StudentLoan, None),
        ];
        assert!(only_candidate(ImportSource::MyirSls, 1, &accounts).is_none());
    }

    /// …and it never fires for an upload describing several things, where "the only account"
    /// cannot be the answer to more than one of them.
    #[test]
    fn a_multi_item_upload_declines_the_only_candidate_tier() {
        let accounts = vec![account(2, "Student loan", AccountKind::StudentLoan, None)];
        assert!(only_candidate(ImportSource::MyirSls, 2, &accounts).is_none());
    }

    #[test]
    fn assignments_parse_and_malformed_ones_are_refused() {
        let parsed =
            parse_assignments(Some("12-3456-0000123-50:8, 12-3456-0000123-51:12")).unwrap();
        assert_eq!(parsed.get("12-3456-0000123-50"), Some(&8));
        assert_eq!(parsed.get("12-3456-0000123-51"), Some(&12));
        assert!(parse_assignments(Some("nonsense")).is_err());
        assert!(parse_assignments(Some("12-3456-0000123-50:notanid")).is_err());
        assert!(parse_assignments(None).unwrap().is_empty());
    }

    /// `ParsedUpload` is constructed in one place per adapter; keep the test helper honest
    /// about its shape so a field added there doesn't silently default here.
    #[test]
    fn the_test_helper_builds_a_whole_upload() {
        let upload = ParsedUpload {
            source: ImportSource::AsbCsv,
            items: vec![item("12-3456-0000123-50", vec![("2026-01-05", 100)])],
            warnings: vec![],
        };
        assert_eq!(upload.items[0].rows.len(), 1);
    }
}
