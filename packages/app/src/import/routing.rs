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
use sure_core::{
    Account, AccountMetadata, AppError, AppResult, ImportMatch, ImportSource, Ownership, Person,
};

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

/// The wire spelling of "leave this one alone" — see [`Assignment::Skip`].
pub const SKIP: &str = "skip";

/// What the request said to do with one thing in the upload. Two answers, and the second one is
/// why this is an enum rather than an `i64`: *omitting* an item is not a decision, it only means
/// this tier has nothing to say and the tiers below should carry on. Saying `skip` is a decision,
/// and it outranks every tier there is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assignment {
    /// Import it into this account, whatever the evidence tiers would have concluded.
    Account(i64),
    /// Import nothing of it. The one instruction no tier may override — which is the difference
    /// between the UI's "skip this one" doing what it says and doing nothing at all.
    Skip,
}

impl Assignment {
    /// The account named, where one is. `None` for a skip, which names none — so an account is
    /// never counted as claimed by an item that isn't going there.
    pub fn account_id(self) -> Option<i64> {
        match self {
            Assignment::Account(id) => Some(id),
            Assignment::Skip => None,
        }
    }
}

/// Everything the resolver knows before it looks at any one item, gathered once per upload.
pub struct Routing {
    /// What the request said outright.
    pub assigned: HashMap<String, Assignment>,
    /// What a previous import of the same source account settled on.
    pub prior: HashMap<String, i64>,
    /// What the rows themselves suggest, for the items nothing above could place.
    pub by_history: HashMap<String, i64>,
    /// The household, for the items whose source names whose they are.
    pub people: Vec<Person>,
}

/// Which Sure account a source account belongs to, and on what evidence. In priority order:
/// what the request said, then a previous import of that same source account, then the
/// account's stored number, then its name, then whose the export says it is, then the
/// transactions the account already holds. (The one-candidate tier sits outside this, in
/// [`only_candidate`], because it is a fact about the whole upload rather than one item.)
pub fn resolve<'a>(
    item: &ParsedItem,
    accounts: &'a [Account],
    routing: &Routing,
) -> Option<(&'a Account, ImportMatch)> {
    let source_account = item.source_account.as_str();
    let by_id = |id: i64, how: ImportMatch| accounts.iter().find(|a| a.id == id).map(|a| (a, how));
    // Exhaustive, and the `Skip` arm returns rather than falls through: a skip that merely
    // failed to name an account would be indistinguishable from silence here, and the five
    // tiers below would go on to place the item the caller just said not to import.
    match routing.assigned.get(source_account) {
        Some(Assignment::Skip) => return None,
        Some(Assignment::Account(id)) => {
            if let Some(found) = by_id(*id, ImportMatch::Assigned) {
                return Some(found);
            }
        }
        None => {}
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
    if let Some(tail) = source_account.splitn(3, '-').nth(2)
        && let Some(found) = only_match(accounts, |a| a.name.contains(tail))
    {
        return Some((found, ImportMatch::AccountName));
    }
    // Above the history tier because it is still an identifier the *source* stated, rather
    // than an inference from the rows — and because on a first import there is no history to
    // infer from, which is exactly when two loans are impossible to tell apart.
    if let Some(found) = match_by_holder(item.holder.as_deref(), accounts, &routing.people) {
        return Some((found, ImportMatch::AccountOwner));
    }
    routing
        .by_history
        .get(source_account)
        .and_then(|id| by_id(*id, ImportMatch::TransactionHistory))
}

/// The account belonging to the person the export names, when that is a single answer at
/// both steps: one household member answering to the name, and one of the candidate accounts
/// being theirs.
///
/// The only tier that can separate two student loans on a *first* import. A myIR export names
/// `012-345-678-SLS004`, which is an IRD number Sure stores nowhere and a suffix that means
/// nothing to it, so every identifier tier above returns nothing — but the same preamble also
/// carries the borrower's name, and Sure already knows who owns which account.
///
/// Matching is deliberately one-directional: IR writes `Surname, Given Names` with a middle
/// initial, so the household's name has to be found *inside* the export's, not the other way
/// round. "Ari" matches "Reed, Ari K"; "Ari Reed" matches it too; "Sam" does not. Anything
/// short of exactly one person and exactly one of their accounts declines — a misrouted loan
/// is years of someone else's repayments on the wrong balance.
fn match_by_holder<'a>(
    holder: Option<&str>,
    accounts: &'a [Account],
    people: &[Person],
) -> Option<&'a Account> {
    let person = person_named(holder, people)?;
    only_match(accounts, |a| {
        a.ownership
            == Ownership::Person {
                person_id: person.id,
            }
    })
}

/// The one household member an export's stated name can mean, or `None` if none or several
/// could. Separate from [`match_by_holder`] because "which person is this?" and "which of
/// their accounts is this?" fail differently: a name nobody answers to is a name Sure doesn't
/// recognise, while a person with no account of this kind is a positive statement that the
/// file belongs to *someone else* — which is what lets [`only_candidate`] refuse.
fn person_named<'a>(holder: Option<&str>, people: &'a [Person]) -> Option<&'a Person> {
    let stated = name_tokens(holder?);
    if stated.is_empty() {
        return None;
    }
    let mut named = people.iter().filter(|p| {
        let theirs = name_tokens(&p.name);
        !theirs.is_empty() && theirs.is_subset(&stated)
    });
    let person = named.next()?;
    // Two household members answering to one name — picking would be guessing.
    named.next().is_none().then_some(person)
}

/// A name reduced to the parts worth comparing: lowercased, split on anything that isn't a
/// letter (so `O'Brien` and `Mary-Jane` break the same way on both sides), and single letters
/// dropped — IR's middle initial is noise, and a one-letter household name would otherwise be
/// a subset of every export there is.
fn name_tokens(name: &str) -> HashSet<String> {
    name.split(|c: char| !c.is_alphabetic())
        .filter(|part| part.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
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
///
/// "The only one there is" stops being an answer when the file says whose it is and that isn't
/// whose the account is. A household that has added one partner's loan and not the other's
/// would otherwise have the second export land silently on the first loan, which reads as a
/// successful import and is years of the wrong person's repayments. A holder that names
/// *nobody* in the household is not a contradiction — an unfamiliar spelling shouldn't refuse
/// an import the tier would otherwise get right — so only a positive mismatch vetoes.
pub fn only_candidate<'a>(
    source: ImportSource,
    items: &[ParsedItem],
    accounts: &'a [Account],
    people: &[Person],
) -> Option<(&'a Account, ImportMatch)> {
    let [item] = items else {
        return None;
    };
    if !source.routes_by_sole_candidate() {
        return None;
    }
    let sole = only_match(accounts, |a| source.accepts(a.kind))?;
    // A joint account contradicts nobody — it is everyone's — so only an account owned by a
    // *different* named person vetoes.
    if let Some(person) = person_named(item.holder.as_deref(), people)
        && sole.ownership.person_id().is_some_and(|id| id != person.id)
    {
        return None;
    }
    Some((sole, ImportMatch::OnlyCandidate))
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
/// A skip names no account, so there is nothing here to check: it is refused by nothing and
/// imports nothing.
pub fn check_assignments(
    source: ImportSource,
    assigned: &HashMap<String, Assignment>,
    accounts: &[Account],
) -> AppResult<()> {
    for id in assigned.values().filter_map(|a| a.account_id()) {
        let Some(account) = accounts.iter().find(|a| a.id == id) else {
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
    assigned: &HashMap<String, Assignment>,
    prior: &HashMap<String, i64>,
) -> AppResult<HashMap<String, i64>> {
    // Anything an identifier already placed is out of the running on both sides: its item
    // needs no guess, and its account is taken. A skipped item claims no account — it names
    // none — so an account a skip merely mentions stays available to the item that wants it.
    let placed: HashSet<i64> = accounts
        .iter()
        .filter(|a| {
            let claimed = |m: &HashMap<String, i64>| m.values().any(|id| *id == a.id);
            let assigned_here = assigned.values().any(|x| x.account_id() == Some(a.id));
            assigned_here || claimed(prior) || stored_number(a).is_some()
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

/// `12-3456-0000123-50:8,12-3456-0000123-51:skip` → the pairs it names.
///
/// `skip` is not a spelling of "no account". It is a statement, and it has to be one: without
/// it, leaving an item unassigned only means *this* tier has nothing to say, and the tiers below
/// — a previous import, a stored account number, the account's name, its transaction history —
/// go on to place the item anyway. The UI's "skip this one" was exactly that omission, so it
/// zeroed the row on screen and then imported it regardless.
pub fn parse_assignments(raw: Option<&str>) -> AppResult<HashMap<String, Assignment>> {
    let mut out = HashMap::new();
    for pair in raw.unwrap_or_default().split(',').filter(|p| !p.is_empty()) {
        let (source_account, value) = pair.rsplit_once(':').ok_or_else(|| {
            AppError::validation(format!(
                "'{pair}' is not an assignment — expected <source account>:<account id> or \
                 <source account>:skip"
            ))
        })?;
        let assignment = match value.trim() {
            SKIP => Assignment::Skip,
            id => Assignment::Account(id.parse().map_err(|_| {
                AppError::validation(format!(
                    "'{id}' in '{pair}' is not an account id, and is not 'skip'"
                ))
            })?),
        };
        out.insert(source_account.trim().to_string(), assignment);
    }
    Ok(out)
}

/// `12-3456-0000123-50:2026-08-01` → the dates it names.
///
/// Only ever consulted where the cutover derivation is *blocked* (see
/// [`ImportBlockReason::resolvable_by_stating_cutover`]), which is what keeps
/// [`CutoverRule`](sure_core::CutoverRule)'s "it is never a parameter" true where it matters: a
/// feed that has posted, or that states its own derive-from date, still decides the window
/// itself and this is ignored.
pub fn parse_cutovers(raw: Option<&str>) -> AppResult<HashMap<String, NaiveDate>> {
    let mut out = HashMap::new();
    for pair in raw.unwrap_or_default().split(',').filter(|p| !p.is_empty()) {
        let (source_account, date) = pair.rsplit_once(':').ok_or_else(|| {
            AppError::validation(format!(
                "'{pair}' is not a cutover — expected <source account>:<YYYY-MM-DD>"
            ))
        })?;
        let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").map_err(|_| {
            AppError::validation(format!(
                "'{}' in '{pair}' is not a date — expected YYYY-MM-DD",
                date.trim()
            ))
        })?;
        out.insert(source_account.trim().to_string(), date);
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
            excluded_from_net_worth: false,
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

    /// An account owned outright by one person, rather than the household.
    fn owned_by(id: i64, name: &str, kind: AccountKind, person_id: i64) -> Account {
        Account {
            ownership: Ownership::Person { person_id },
            ..account(id, name, kind, None)
        }
    }

    fn person(id: i64, name: &str) -> Person {
        Person {
            id,
            name: name.to_string(),
            color: None,
            sort_order: id,
            placeholder: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn item(source_account: &str, rows: Vec<(&str, i64)>) -> ParsedItem {
        ParsedItem {
            source_account: source_account.to_string(),
            label: None,
            holder: None,
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
            people: vec![],
        }
    }

    /// An item a source stated the owner of, the way a myIR export does.
    fn held_by(source_account: &str, holder: &str) -> ParsedItem {
        ParsedItem {
            holder: Some(holder.to_string()),
            ..item(source_account, vec![])
        }
    }

    /// The shape this whole tier exists for: two student loans, and an export whose only
    /// distinguishing mark is the name in its preamble. Every identifier tier is blind here —
    /// an SLS account id is stored nowhere, and on a first import there is no history either.
    #[test]
    fn two_student_loans_are_told_apart_by_who_the_export_names() {
        let accounts = vec![
            owned_by(1, "Student loan", AccountKind::StudentLoan, 10),
            owned_by(2, "Student loan", AccountKind::StudentLoan, 20),
        ];
        let routing = Routing {
            people: vec![person(10, "Ari"), person(20, "Sam")],
            ..empty()
        };
        // IR writes `Surname, Given Names` with a middle initial; the household writes "Sam".
        let it = held_by("012-345-678-SLS004", "Reed, Sam J");
        let (found, how) = resolve(&it, &accounts, &routing).unwrap();
        assert_eq!(found.id, 2);
        assert_eq!(how, ImportMatch::AccountOwner);

        // The other partner's export goes to the other loan, on the same evidence.
        let theirs = held_by("098-765-432-SLS004", "Reed, Ari K");
        assert_eq!(resolve(&theirs, &accounts, &routing).unwrap().0.id, 1);
    }

    /// A full name still matches: the household's name has to be found *inside* the export's,
    /// not equal it, because IR's version carries a surname and an initial the roster won't.
    #[test]
    fn a_household_name_is_matched_inside_the_exports_longer_one() {
        let accounts = vec![owned_by(1, "Loan", AccountKind::StudentLoan, 10)];
        let people = vec![person(10, "Ari Reed"), person(20, "Sam")];
        assert_eq!(
            match_by_holder(Some("Reed, Ari K"), &accounts, &people).map(|a| a.id),
            Some(1)
        );
        // …and a name that isn't in there at all matches nobody, rather than the nearest one.
        assert!(match_by_holder(Some("Nguyen, Toni"), &accounts, &people).is_none());
        // A one-letter roster name would be a subset of every export there is, so it is dropped
        // along with IR's middle initial rather than matching "Reed, Ari K" on its initial.
        assert!(match_by_holder(Some("Reed, Ari K"), &accounts, &[person(10, "K")]).is_none());
    }

    /// Ambiguity declines at both steps, the same way every other tier does.
    #[test]
    fn an_ambiguous_owner_resolves_to_nothing() {
        // Two household members answering to one export name.
        let accounts = vec![owned_by(1, "Loan", AccountKind::StudentLoan, 10)];
        let two = vec![person(10, "Ari"), person(20, "Ari Reed")];
        assert!(match_by_holder(Some("Reed, Ari K"), &accounts, &two).is_none());

        // …and one person with two accounts this source accepts: the name says whose, not which.
        let both = vec![
            owned_by(1, "Loan (IR)", AccountKind::StudentLoan, 10),
            owned_by(2, "Loan (overseas)", AccountKind::StudentLoan, 10),
        ];
        assert!(match_by_holder(Some("Reed, Ari K"), &both, &[person(10, "Ari")]).is_none());
    }

    /// The mirror of the tier: an export that positively names someone else must not land on
    /// the one loan there happens to be. Importing a partner's whole repayment history onto
    /// your own balance reads as a success and is not a recoverable mistake.
    #[test]
    fn the_only_candidate_is_refused_when_the_export_names_someone_else() {
        let accounts = vec![owned_by(1, "Student loan", AccountKind::StudentLoan, 10)];
        let people = vec![person(10, "Ari"), person(20, "Sam")];
        let theirs = [held_by("012-345-678-SLS004", "Reed, Sam J")];
        assert!(only_candidate(ImportSource::MyirSls, &theirs, &accounts, &people).is_none());

        // The owner's own export still routes there, and so does one naming nobody Sure knows —
        // an unfamiliar spelling shouldn't refuse an import this tier would otherwise get right.
        for holder in ["Reed, Ari K", "Nguyen, Toni"] {
            let mine = [held_by("012-345-678-SLS004", holder)];
            let (found, how) =
                only_candidate(ImportSource::MyirSls, &mine, &accounts, &people).unwrap();
            assert_eq!((found.id, how), (1, ImportMatch::OnlyCandidate));
        }
    }

    /// A joint account is everyone's, so it contradicts no name — the veto is for an account
    /// owned by a *different* individual, not for any account that isn't the named one's.
    #[test]
    fn a_joint_account_is_not_contradicted_by_a_named_holder() {
        let accounts = vec![account(1, "Student loan", AccountKind::StudentLoan, None)];
        let people = vec![person(10, "Ari"), person(20, "Sam")];
        let it = [held_by("012-345-678-SLS004", "Reed, Sam J")];
        assert!(only_candidate(ImportSource::MyirSls, &it, &accounts, &people).is_some());
    }

    /// A source that names nobody loses only this tier. Every other source sets `holder: None`,
    /// so nothing about an ASB or Sharesies upload changes.
    #[test]
    fn a_source_that_names_no_owner_is_unaffected() {
        let accounts = vec![owned_by(1, "Loan", AccountKind::StudentLoan, 10)];
        let people = vec![person(10, "Ari")];
        assert!(match_by_holder(None, &accounts, &people).is_none());
        assert!(match_by_holder(Some("   "), &accounts, &people).is_none());
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
            assigned: HashMap::from([("12-3456-0000123-50".to_string(), Assignment::Account(2))]),
            prior: HashMap::from([("12-3456-0000123-50".to_string(), 1)]),
            ..empty()
        };
        let it = item("12-3456-0000123-50", vec![]);
        let (found, how) = resolve(&it, &accounts, &routing).unwrap();
        assert_eq!(found.id, 2);
        assert_eq!(how, ImportMatch::Assigned);
    }

    #[test]
    fn a_stored_number_beats_a_name_that_merely_contains_the_tail() {
        let accounts = vec![
            account(1, "Everyday", AccountKind::Bank, Some("12-3456-0000123-50")),
            account(2, "Savings (0000123-50)", AccountKind::Savings, None),
        ];
        let it = item("12-3456-0000123-50", vec![]);
        let (found, how) = resolve(&it, &accounts, &empty()).unwrap();
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
        assert!(resolve(&item("12-3456-0000123-50", vec![]), &accounts, &empty()).is_none());
    }

    /// The tier that replaces having the account in the URL: one loan, one export, one answer.
    #[test]
    fn a_single_export_routes_to_the_only_account_of_its_kind() {
        let accounts = vec![
            account(1, "Everyday", AccountKind::Bank, None),
            account(2, "Student loan", AccountKind::StudentLoan, None),
        ];
        let one = [item("012-345-678-SLS004", vec![])];
        let (found, how) = only_candidate(ImportSource::MyirSls, &one, &accounts, &[]).unwrap();
        assert_eq!(found.id, 2);
        assert_eq!(how, ImportMatch::OnlyCandidate);
    }

    #[test]
    fn two_accounts_of_the_same_kind_decline_the_only_candidate_tier() {
        let accounts = vec![
            account(1, "Loan A", AccountKind::StudentLoan, None),
            account(2, "Loan B", AccountKind::StudentLoan, None),
        ];
        let one = [item("012-345-678-SLS004", vec![])];
        assert!(only_candidate(ImportSource::MyirSls, &one, &accounts, &[]).is_none());
    }

    /// …and it never fires for an upload describing several things, where "the only account"
    /// cannot be the answer to more than one of them.
    #[test]
    fn a_multi_item_upload_declines_the_only_candidate_tier() {
        let accounts = vec![account(2, "Student loan", AccountKind::StudentLoan, None)];
        let two = [
            item("012-345-678-SLS004", vec![]),
            item("098-765-432-SLS004", vec![]),
        ];
        assert!(only_candidate(ImportSource::MyirSls, &two, &accounts, &[]).is_none());
    }

    #[test]
    fn assignments_parse_and_malformed_ones_are_refused() {
        let parsed =
            parse_assignments(Some("12-3456-0000123-50:8, 12-3456-0000123-51:12")).unwrap();
        assert_eq!(
            parsed.get("12-3456-0000123-50"),
            Some(&Assignment::Account(8))
        );
        assert_eq!(
            parsed.get("12-3456-0000123-51"),
            Some(&Assignment::Account(12))
        );
        assert!(parse_assignments(Some("nonsense")).is_err());
        assert!(parse_assignments(Some("12-3456-0000123-50:notanid")).is_err());
        assert!(parse_assignments(None).unwrap().is_empty());
    }

    #[test]
    fn skip_parses_as_a_decision_of_its_own() {
        let parsed = parse_assignments(Some("12-3456-0000123-50:8,12-3456-0000123-51:skip"))
            .expect("both halves are legal");
        assert_eq!(parsed.get("12-3456-0000123-51"), Some(&Assignment::Skip));
        // …and it claims no account, so the account it *would* have gone to stays available to
        // whatever item genuinely wants it.
        assert_eq!(Assignment::Skip.account_id(), None);
        assert_eq!(Assignment::Account(8).account_id(), Some(8));
    }

    /// The bug this exists to prevent: the UI's "skip this one" sent nothing at all, so the
    /// assignment tier had nothing to say and the *five tiers below it* went on to place the item
    /// anyway. The row read as skipped on screen and imported regardless.
    #[test]
    fn a_skip_outranks_every_tier_that_would_have_placed_the_item() {
        let accounts = vec![account(
            1,
            "Everyday",
            AccountKind::Bank,
            Some("12-3456-0000123-50"),
        )];
        let it = item("12-3456-0000123-50", vec![]);
        // The stored-number tier would place this, and a previous import would too.
        let placed = Routing {
            prior: HashMap::from([("12-3456-0000123-50".to_string(), 1)]),
            ..empty()
        };
        assert!(
            resolve(&it, &accounts, &placed).is_some(),
            "without the skip"
        );

        let skipped = Routing {
            assigned: HashMap::from([("12-3456-0000123-50".to_string(), Assignment::Skip)]),
            ..placed
        };
        assert!(resolve(&it, &accounts, &skipped).is_none());
    }

    #[test]
    fn cutovers_parse_and_a_bad_date_is_refused() {
        let parsed = parse_cutovers(Some("12-3456-0000123-50:2026-08-01")).unwrap();
        assert_eq!(
            parsed.get("12-3456-0000123-50"),
            NaiveDate::from_ymd_opt(2026, 8, 1).as_ref()
        );
        assert!(parse_cutovers(Some("12-3456-0000123-50:01/08/2026")).is_err());
        assert!(parse_cutovers(Some("12-3456-0000123-50:today")).is_err());
        assert!(parse_cutovers(Some("nonsense")).is_err());
        assert!(parse_cutovers(None).unwrap().is_empty());
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
