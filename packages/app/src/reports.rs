//! Backend-computed report data. All heavy aggregation (running balances, currency
//! normalisation, category roll-ups, flow graphs) happens here so callers only ever
//! handle ready-made numbers. Row loading goes through the [`ReportRepo`]/[`FxRatesRepo`]
//! ports; this module never touches `sqlx` directly. The wire-facing response DTOs
//! (`ToSchema`) and query-param extractors live in `sure-api`'s `routes::reports` —
//! genuinely computed/flattened shapes that are built from these plain result types —
//! and the query structs here mirror their fields so a handler is a single call plus a
//! field-copy.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Datelike, NaiveDate};

use sure_core::{
    AccountClass, AccountKind, AppError, AppResult, CategoryKind, Interval, Ownership,
};

use crate::fx::Fx;
use crate::ports::{Clock, FxRatesRepo, ReportRepo, SpendTransaction};

// ---- query params --------------------------------------------------------

#[derive(Debug, Default)]
pub struct ReportQuery {
    /// Inclusive start date (ISO-8601). Defaults to the earliest data.
    pub from: Option<String>,
    /// Inclusive end date (ISO-8601). Defaults to today.
    pub to: Option<String>,
    /// Include one-off transactions (default false).
    pub include_one_off: Option<bool>,
    /// Report currency; defaults to the configured base currency.
    pub currency: Option<String>,
    /// Restrict to one household member's spending, or to the joint bucket. Matches on a
    /// transaction's *effective* attribution — its own override, else its account's owner.
    /// `None` reports the whole household, which stays the default everywhere.
    pub attributed_to: Option<Ownership>,
}

#[derive(Debug, Default)]
pub struct NetWorthQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    /// Sampling interval; defaults to [`Interval::Month`]. Parsed at the HTTP edge
    /// (`sure-api`'s `routes::reports`), so an unrecognised value never reaches here.
    pub interval: Option<Interval>,
    pub currency: Option<String>,
    /// Restrict to the accounts one household member owns, or to the joint ones.
    ///
    /// Net worth is an account-level quantity — a balance belongs to whoever owns the
    /// account, and a per-transaction override says who a *movement* was for, not who owns
    /// the pot it moved through. So this filters accounts and ignores overrides, unlike the
    /// spend reports. Joint accounts are their own bucket rather than being split in half:
    /// there is no share percentage anywhere in the app, and inventing 50/50 would put a
    /// number on screen that nothing in the data supports.
    pub attributed_to: Option<Ownership>,
}

// ---- result shapes --------------------------------------------------------

#[derive(Debug)]
pub struct NetWorthPoint {
    pub as_of: String,
    pub net_worth_minor: i64,
    pub assets_minor: i64,
    pub liabilities_minor: i64,
}

#[derive(Debug)]
pub struct NetWorthSeries {
    pub currency: String,
    pub points: Vec<NetWorthPoint>,
    /// Currency codes whose accounts are **absent** from every point above, because no rate
    /// links them to `currency`. See [`Fx::try_factor`] for why they are left out rather
    /// than counted at parity.
    pub unconverted: Vec<String>,
    /// Newest date across the rates used (ISO-8601), or `None` if none are on record — a
    /// year-old date means a dead feed, not a stable market.
    pub rates_as_of: Option<String>,
}

#[derive(Debug)]
pub struct CategoryTotal {
    /// `None` for the uncategorised bucket.
    pub category_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub total_minor: i64,
}

#[derive(Debug)]
pub struct CategoryBreakdown {
    pub currency: String,
    pub from: String,
    pub to: String,
    pub income: Vec<CategoryTotal>,
    pub expense: Vec<CategoryTotal>,
}

/// Which side of the money-flow graph a node represents. Built directly (never parsed
/// from external text, so no `FromStr`); `as_str` renders it to the wire DTO's plain
/// `String` field once, in `sure-api`'s `From<SankeyNode>` impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SankeyNodeKind {
    Income,
    Center,
    Expense,
    Savings,
}

impl SankeyNodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SankeyNodeKind::Income => "income",
            SankeyNodeKind::Center => "center",
            SankeyNodeKind::Expense => "expense",
            SankeyNodeKind::Savings => "savings",
        }
    }
}

/// How many levels of category hierarchy each side of the money-flow graph fans out into.
/// Tied to [`sure_core::MAX_CATEGORY_DEPTH`] so the tree the API lets you build and the
/// tree the chart can draw are the same tree — but the builder still rolls deeper chains
/// up, because the import and snapshot-restore paths bypass that validation.
pub const SANKEY_MAX_DEPTH: usize = sure_core::MAX_CATEGORY_DEPTH as usize;

/// The uncategorised bucket's stand-in category key. SQLite rowids start at 1, so 0 can
/// never collide with a real category id.
pub(crate) const UNCATEGORISED: i64 = 0;

/// The money-flow graph's hub, which both sides link to.
const CENTER: &str = "center";

#[derive(Debug)]
pub struct SankeyNode {
    pub id: String,
    pub label: String,
    pub kind: SankeyNodeKind,
    /// The category this node stands for. `None` for `center`, `savings`, and the
    /// uncategorised bucket (which has no row in `categories`).
    pub category_id: Option<i64>,
    /// 0-based level within its own side: 0 = top-level (adjacent to the hub), 1 = its
    /// child, 2 = grandchild. `None` for `center`/`savings`, which sit on the spine rather
    /// than in either hierarchy.
    pub depth: Option<u8>,
    /// The top-level ancestor this node descends from, so a client can colour a whole
    /// branch from one key. Equals `category_id` at depth 0.
    pub root_id: Option<i64>,
    /// That top-level ancestor's own `color`, if the user set one — the branch's base
    /// shade, which the client darkens or lightens by `depth`. Deliberately the *root's*
    /// colour rather than this node's: the point is one hue per branch, so honouring a
    /// per-node override here would fight that.
    pub root_color: Option<String>,
}

#[derive(Debug)]
pub struct SankeyLink {
    pub source: String,
    pub target: String,
    pub value_minor: i64,
}

#[derive(Debug)]
pub struct SankeyGraph {
    pub currency: String,
    pub nodes: Vec<SankeyNode>,
    pub links: Vec<SankeyLink>,
}

#[derive(Debug)]
pub struct AccountBalance {
    pub account_id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub class: AccountClass,
    pub currency_code: String,
    pub value_minor: i64,
    /// Who owns this account. Carried on the row so the balance sheet can be regrouped by
    /// person in the client — it already regroups by kind and class the same way — rather
    /// than the report growing a per-person variant of itself.
    pub ownership: Ownership,
}

#[derive(Debug)]
pub struct BalancesReport {
    pub currency: String,
    pub as_of: String,
    /// Every account whose currency converts. An account in an unconvertible currency is
    /// still listed in `accounts` (in its own currency, which is a true figure) but is not
    /// inside this total, and its currency is named in `unconverted`.
    pub total_minor: i64,
    pub accounts: Vec<AccountBalance>,
    /// Currency codes excluded from `total_minor` for want of a rate.
    pub unconverted: Vec<String>,
    /// Newest date across the rates used (ISO-8601), or `None` if none are on record.
    pub rates_as_of: Option<String>,
}

#[derive(Debug)]
pub struct SecuredLiability {
    pub account_id: i64,
    pub name: String,
    pub kind: AccountKind,
    /// Amount owed, in the report currency (positive).
    pub balance_minor: i64,
}

#[derive(Debug)]
pub struct EquityPosition {
    pub account_id: i64,
    pub name: String,
    pub currency: String,
    pub as_of: String,
    /// The asset's value, in the report currency.
    pub value_minor: i64,
    /// Total secured debt, in the report currency (positive).
    pub total_debt_minor: i64,
    /// value − debt.
    pub equity_minor: i64,
    /// How much of the asset is owned outright: (value − debt) / value, clamped 0–100.
    pub paid_off_pct: f64,
    pub liabilities: Vec<SecuredLiability>,
}

// ---- helpers (pure, no repo access) ---------------------------------------

/// Parse a date supplied by the *caller* — a `?from=`/`?to=` query parameter. Tolerant by
/// design: an unparseable bound falls back to the report's own default (earliest row / today)
/// rather than erroring, and a client typo is its own visible symptom (the window it asked
/// for isn't the window it got). Leading-10 truncation is deliberate here so a UI that sends
/// a full datetime still bounds correctly.
pub(crate) fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}

/// Parse a date that came back out of the *database*, warning loudly when it won't parse.
///
/// Every write path now takes a [`sure_core::IsoDate`], so a stored value that fails here can
/// only be legacy data — a row written before that type existed, when a `31/07/2026` was
/// accepted with a 201 and then silently vanished from every report. Two choices, and neither
/// "just work":
///
/// * refuse to read it (return an error, or `expect`) — that turns one bad historical row
///   into a hard 500 on the balance sheet, net worth, category breakdown and forecast at
///   once, for data the user can no longer see in order to fix;
/// * skip it, as this has always done — the row is absent from the figure but present in the
///   transaction list, which is the *original* bug: a permanent, unexplained disagreement.
///
/// So: still skip (a report stays answerable), but never silently. The WARN names the column,
/// the offending text and — where the loader carries it — the owning account, which is enough
/// to go and repair the row (`SELECT … WHERE posted_at = '31/07/2026'`): the gap in the figure
/// now has a cause attached to it instead of being invisible. `account_id` is `None` for the
/// spend loader, whose row shape doesn't carry one; the column and value still identify it.
pub(crate) fn parse_stored_date(
    field: &str,
    account_id: Option<i64>,
    s: &str,
) -> Option<NaiveDate> {
    match NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d") {
        Ok(d) => Some(d),
        Err(_) => {
            tracing::warn!(
                field,
                account_id = ?account_id,
                value = s,
                "unparseable stored date: row excluded from this report — legacy data written \
                 before the date was a validated type; repair the row to make it count again"
            );
            None
        }
    }
}

/// Narrow an `i128` running total of minor units back to the `i64` the wire and the column
/// use — saturating, loudly — instead of panicking or wrapping.
///
/// This is the second half of the money-magnitude guard, and the half that covers rows
/// *already on disk*. [`sure_core::Money`] bounds what can be written from now on, but it
/// cannot retro-fix a row stored before it existed, and two paths still bypass it by design
/// (provider import, snapshot restore). Left alone, `[i64::MAX, i64::MAX].iter().sum()` gives
/// the worst pair of outcomes there is:
///
/// * debug (`overflow-checks` on): a panic inside the balance walk. `CatchPanicLayer` turns it
///   into a scrubbed 500 on the balance sheet, net worth, equity position and forecast at once
///   — and the rows responsible can't be found through the UI, because the pages that would
///   list them are the 500ing ones;
/// * release (the root `Cargo.toml` sets no `overflow-checks`): the total wraps to a small
///   negative and the balance sheet prints a plausible, wrong number with no error anywhere.
///
/// So: accumulate in `i128` — which no realistic number of `i64` rows can overflow — and clamp
/// once, here, with a WARN naming the report component, the owning account and the total that
/// didn't fit. A saturated figure is obviously wrong on screen (it is `i64::MAX` minor units)
/// and leaves a log line pointing at the account to go and repair, which is strictly better
/// than both a 500 and a plausible lie. Same posture as [`parse_stored_date`]: tolerate legacy
/// data, but never silently.
pub(crate) fn narrow_minor(what: &str, account_id: Option<i64>, total: i128) -> i64 {
    match i64::try_from(total) {
        Ok(v) => v,
        Err(_) => {
            let clamped = if total > 0 { i64::MAX } else { i64::MIN };
            tracing::warn!(
                what,
                account_id = ?account_id,
                total = %total,
                clamped,
                "money total does not fit in i64: saturated rather than overflowing — some row \
                 holds an amount past sure_core::MAX_MONEY_MINOR (legacy data, a provider \
                 import or a snapshot restore); find it and repair it to make this figure real"
            );
            clamped
        }
    }
}

/// Add signed minor-unit amounts without `Iterator::sum`'s two `i64` failure modes. The
/// accumulator is `i128`, so the addition itself cannot overflow for any number of rows SQLite
/// could return; only the final narrowing can clamp, and [`narrow_minor`] says so when it does.
pub(crate) fn sum_minor(
    what: &str,
    account_id: Option<i64>,
    amounts: impl Iterator<Item = i64>,
) -> i64 {
    narrow_minor(what, account_id, amounts.map(i128::from).sum::<i128>())
}

pub(crate) fn last_day_of_month(y: i32, m: u32) -> NaiveDate {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
}

/// An account's value as of a date.
///
/// Three cases, in order:
/// 1. A valuation on/before `date` — use the most recent one directly. Covers a brokerage
///    account (a valuation for every day), and a property/vehicle carrying its manual
///    valuation forward from the entry date.
/// 2. No valuation on/before `date`, but a later one exists — reconstruct *backwards* from
///    that known balance by subtracting the transaction movements that happened after
///    `date`, rather than summing forward from an assumed-zero opening. This is what keeps
///    a provider-synced liability like a mortgage correct across history: the provider only
///    reports *today's* balance, and the account's own transactions don't reconcile to it
///    from zero (a drawdown's sign, an untracked opening balance, …), so summing forward
///    would show a spurious positive "asset". Before the account's first transaction it
///    hadn't been opened/drawn down yet, so it's 0.
/// 3. No valuations at all — the running transaction balance (a plain cash account).
pub(crate) fn account_value_at(
    id: i64,
    currency: &str,
    date: NaiveDate,
    tx_by_acct: &HashMap<i64, Vec<(NaiveDate, i64)>>,
    val_by_acct: &HashMap<i64, Vec<(NaiveDate, i64, String)>>,
) -> (i64, String) {
    if let Some(vals) = val_by_acct.get(&id) {
        // Case 1.
        if let Some((_, value, ccy)) = vals
            .iter()
            .filter(|(d, _, _)| *d <= date)
            .max_by_key(|(d, _, _)| *d)
        {
            return (*value, ccy.clone());
        }
        // Case 2: anchor to the earliest known valuation and walk backwards.
        if let Some((anchor_date, anchor_value, ccy)) = vals.iter().min_by_key(|(d, _, _)| *d) {
            let first_txn = tx_by_acct
                .get(&id)
                .and_then(|txs| txs.iter().map(|(d, _)| *d).min());
            return match first_txn {
                Some(first) if date >= first => {
                    // `i128` throughout: both the sum of the movements and the subtraction from
                    // the anchor are unbounded in principle, and a stored amount past
                    // `MAX_MONEY_MINOR` must not be able to panic (debug) or wrap (release)
                    // this walk. Narrowed once, at the end, with a WARN if it had to clamp.
                    let after_date: i128 = tx_by_acct
                        .get(&id)
                        .map(|txs| {
                            txs.iter()
                                .filter(|(d, _)| *d > date && *d <= *anchor_date)
                                .map(|(_, a)| i128::from(*a))
                                .sum()
                        })
                        .unwrap_or(0);
                    let reconstructed = i128::from(*anchor_value) - after_date;
                    (
                        narrow_minor(
                            "account_value_at: valuation anchor minus later movements",
                            Some(id),
                            reconstructed,
                        ),
                        ccy.clone(),
                    )
                }
                // Before the account's first transaction (or it has none) → not yet opened.
                _ => (0, ccy.clone()),
            };
        }
    }
    // Case 3. The running balance of a plain cash account: the one aggregation the whole
    // balance sheet rests on, and the one that used to be a bare `.sum()` over `i64`.
    let balance = tx_by_acct
        .get(&id)
        .map(|txs| {
            sum_minor(
                "account_value_at: running transaction balance",
                Some(id),
                txs.iter().filter(|(d, _)| *d <= date).map(|(_, a)| *a),
            )
        })
        .unwrap_or(0);
    (balance, currency.to_string())
}

pub(crate) fn sample_dates(from: NaiveDate, to: NaiveDate, interval: Interval) -> Vec<NaiveDate> {
    if to < from {
        return vec![to];
    }
    let mut out = Vec::new();
    match interval {
        Interval::Day | Interval::Week => {
            let step = if interval == Interval::Week { 7 } else { 1 };
            let mut d = from;
            while d < to && out.len() < 400 {
                out.push(d);
                d += chrono::Duration::days(step);
            }
        }
        Interval::Month => {
            // month-ends within the window
            let (mut y, mut m) = (from.year(), from.month());
            while out.len() < 600 {
                let end = last_day_of_month(y, m);
                if end > to {
                    break;
                }
                if end >= from {
                    out.push(end);
                }
                if m == 12 {
                    y += 1;
                    m = 1;
                } else {
                    m += 1;
                }
            }
        }
    }
    if out.last() != Some(&to) {
        out.push(to);
    }
    out
}

/// Mortgage, student-loan and brokerage accounts carry the instrument's own bookkeeping —
/// loan drawdowns/amortisation/repayments, trades/FX — not household income or spending.
/// Unlike `credit_card`/`revolving_credit`, which are everyday transaction accounts, these
/// kinds should never feed the income/expense report, one-off toggle or not.
///
/// A student loan is the sharpest case: on a liability a repayment is a *positive* amount
/// (it moves the negative balance towards zero), so leaving it in would report years of
/// repayments as household income. It can't be reported as an expense either — that would
/// need the opposite sign, which is what the balance reconstruction in
/// [`account_value_at`] depends on. Both the myIR import and the balance-delta task feed
/// this account kind, so the exclusion has to live here rather than in either of them.
/// A plain `loan` is the same shape as a mortgage — a drawdown, then repayments that are
/// positive on the liability — so it belongs here for the same reason. It also has to be
/// here for the forecast to be correct: `sure_app::forecast` debits a projected loan
/// repayment from the cash pool, which is only free of double-counting because the loan's
/// own legs never reach a category baseline.
pub(crate) fn is_excluded_from_spend(kind: AccountKind) -> bool {
    matches!(
        kind,
        AccountKind::Mortgage
            | AccountKind::Loan
            | AccountKind::StudentLoan
            | AccountKind::Brokerage
    )
}

// ---- category lookups (shared by pie + sankey) ----------------------------

pub(crate) struct Categories {
    parents: HashMap<i64, Option<i64>>,
    names: HashMap<i64, String>,
    colors: HashMap<i64, Option<String>>,
    kinds: HashMap<i64, CategoryKind>,
}

impl Categories {
    pub(crate) async fn load(reports: &dyn ReportRepo) -> AppResult<Self> {
        let cats = reports.categories().await?;
        let mut c = Categories {
            parents: HashMap::new(),
            names: HashMap::new(),
            colors: HashMap::new(),
            kinds: HashMap::new(),
        };
        for cat in cats {
            c.parents.insert(cat.id, cat.parent_id);
            c.names.insert(cat.id, cat.name);
            c.colors.insert(cat.id, cat.color);
            c.kinds.insert(cat.id, cat.kind);
        }
        Ok(c)
    }

    pub(crate) fn top_ancestor(&self, id: i64) -> i64 {
        let mut cur = id;
        for _ in 0..64 {
            match self.parents.get(&cur) {
                Some(Some(p)) => cur = *p,
                // Either the walk reached a top-level category (`Some(None)`) or the id
                // isn't in the map at all — both mean `cur` is as far up as we can go.
                Some(None) | None => break,
            }
        }
        cur
    }

    /// The ancestor chain of `id`, root first and ending at `id` itself. An id that isn't
    /// in the map yields `[id]`, matching [`Self::top_ancestor`]'s treatment of one.
    ///
    /// Same 64-hop guard as [`Self::top_ancestor`], plus a seen-check: a parent cycle is
    /// impossible through the API (`sure_dal::categories::validate` rejects one) but a
    /// hand-edited database shouldn't be able to hang a report.
    pub(crate) fn chain(&self, id: i64) -> Vec<i64> {
        let mut chain = vec![id];
        let mut cur = id;
        for _ in 0..64 {
            match self.parents.get(&cur) {
                Some(Some(p)) if !chain.contains(p) => {
                    chain.push(*p);
                    cur = *p;
                }
                Some(Some(_)) | Some(None) | None => break,
            }
        }
        chain.reverse();
        chain
    }

    /// [`Self::chain`] cut to at most `max` levels, root first. A category nested deeper
    /// than `max` reports as its ancestor at level `max - 1`, so its spend rolls up there
    /// rather than being dropped or placed in a column the chart doesn't draw.
    pub(crate) fn chain_to_depth(&self, id: i64, max: usize) -> Vec<i64> {
        let mut chain = self.chain(id);
        chain.truncate(max);
        chain
    }

    /// A category's own `color`, if the user set one.
    pub(crate) fn color_of(&self, id: i64) -> Option<String> {
        self.colors.get(&id).cloned().flatten()
    }

    pub(crate) fn is_transfer(&self, id: i64) -> bool {
        self.kinds.get(&id) == Some(&CategoryKind::Transfer)
    }

    /// Every top-level (no parent) category id and its flow `kind` — the granularity the
    /// forecast's category assumptions resolve at, matching `category_breakdown`'s own
    /// top-level roll-up.
    pub(crate) fn top_level_kinds(&self) -> Vec<(i64, CategoryKind)> {
        self.parents
            .iter()
            .filter(|(_, parent)| parent.is_none())
            .filter_map(|(id, _)| self.kinds.get(id).map(|k| (*id, *k)))
            .collect()
    }

    pub(crate) fn name_of(&self, id: i64) -> String {
        self.names.get(&id).cloned().unwrap_or_else(|| "?".into())
    }

    pub(crate) fn kind_of(&self, id: i64) -> Option<CategoryKind> {
        self.kinds.get(&id).copied()
    }
}

#[cfg(test)]
impl Categories {
    /// A bare `Categories` for tests elsewhere in this crate that don't need a real
    /// `ReportRepo` — build it up with [`Self::insert_for_test`].
    pub(crate) fn default_for_test() -> Self {
        Categories {
            parents: HashMap::new(),
            names: HashMap::new(),
            colors: HashMap::new(),
            kinds: HashMap::new(),
        }
    }

    pub(crate) fn insert_for_test(
        &mut self,
        id: i64,
        parent_id: Option<i64>,
        name: &str,
        kind: CategoryKind,
    ) {
        self.parents.insert(id, parent_id);
        self.names.insert(id, name.to_string());
        self.colors.insert(id, None);
        self.kinds.insert(id, kind);
    }
}

/// Load transactions + valuations indexed per account, for point-in-time balances.
pub(crate) type Ledger = (
    HashMap<i64, Vec<(NaiveDate, i64)>>,
    HashMap<i64, Vec<(NaiveDate, i64, String)>>,
);

/// The whole ledger, from the first row on record. Only the forecast wants this: it fits
/// growth trends and dividend yields over all of an account's history, so there is no window
/// to push down. Every *report* goes through [`load_ledger_from`] instead.
pub(crate) async fn load_ledger(reports: &dyn ReportRepo) -> AppResult<Ledger> {
    load_ledger_window(reports, None).await
}

/// The ledger a report needs to value accounts on any date from `from` onwards.
///
/// The window is pushed into SQL rather than applied here, because applying it here is what
/// the whole `transactions`/`valuations` read used to be: every report — including the balance
/// sheet, which asks about exactly one day — materialised every row of both tables, ~30 MB per
/// copy on a 500k-row ledger and up to 64 copies at the in-flight ceiling.
///
/// What makes that safe is on the other side of the port: the repo returns the rows in the
/// window *plus* a per-account seed standing in for everything before it (one collapsed
/// transaction total, and the latest earlier valuation), so [`account_value_at`] still sees a
/// complete running balance and a complete "opened yet?" answer for every date it is asked
/// about. See `ReportRepo::transactions` for the contract and `sure_dal::reports` for how it
/// is met.
pub(crate) async fn load_ledger_from(
    reports: &dyn ReportRepo,
    from: NaiveDate,
) -> AppResult<Ledger> {
    load_ledger_window(reports, Some(from)).await
}

async fn load_ledger_window(
    reports: &dyn ReportRepo,
    from: Option<NaiveDate>,
) -> AppResult<Ledger> {
    let txns = reports.transactions(from).await?;
    let vals = reports.valuations(from).await?;
    let mut tx_by_acct: HashMap<i64, Vec<(NaiveDate, i64)>> = HashMap::new();
    for t in &txns {
        if let Some(d) =
            parse_stored_date("transactions.posted_at", Some(t.account_id), &t.posted_at)
        {
            tx_by_acct
                .entry(t.account_id)
                .or_default()
                .push((d, t.amount_minor));
        }
    }
    let mut val_by_acct: HashMap<i64, Vec<(NaiveDate, i64, String)>> = HashMap::new();
    for v in &vals {
        if let Some(d) = parse_stored_date("valuations.as_of", Some(v.account_id), &v.as_of) {
            val_by_acct.entry(v.account_id).or_default().push((
                d,
                v.value_minor,
                v.currency_code.clone(),
            ));
        }
    }
    Ok((tx_by_acct, val_by_acct))
}

/// Load transactions in the window, excluding transfers (either linked, or in a
/// transfer-kind category) and — optionally — one-offs.
pub(crate) async fn load_spend(
    reports: &dyn ReportRepo,
    cats: &Categories,
    from: NaiveDate,
    to: NaiveDate,
    include_one_off: bool,
    attributed_to: Option<Ownership>,
) -> AppResult<Vec<SpendTransaction>> {
    // The window goes to SQL as well as staying in the filter below: the repo is only asked
    // for a superset (its bounds are inclusive of whole days and blind to a date it can't
    // compare), and the per-row check here is what decides. So the surviving set is exactly
    // what it was when this loaded the whole table — including a row whose stored date won't
    // parse, which is dropped here as it always was.
    let rows = reports.spend_transactions(from, to).await?;
    Ok(rows
        .into_iter()
        .filter(|t| {
            // Whose spending this is was resolved by the loader (override, else account).
            if attributed_to.is_some_and(|owner| t.attribution != owner) {
                return false;
            }
            // Linked transactions are the two legs of a transfer — internal movement.
            if t.linked_transaction_id.is_some() {
                return false;
            }
            if is_excluded_from_spend(t.account_kind) {
                return false;
            }
            if !include_one_off && t.is_one_off {
                return false;
            }
            if let Some(cid) = t.category_id {
                if cats.is_transfer(cid) {
                    return false;
                }
            }
            match parse_stored_date("transactions.posted_at", None, &t.posted_at) {
                Some(d) => d >= from && d <= to,
                None => false,
            }
        })
        .collect())
}

// ---- money-flow roll-up (sankey) ------------------------------------------

/// Which half of the money-flow graph a roll-up belongs to.
///
/// Decided purely by the sign of the amount, never by [`CategoryKind`]: a refund booked
/// against an expense category *is* income, so one category legitimately appears on both
/// sides of the graph with different totals. That's the gross picture the chart wants —
/// netting the two would hide the refund entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlowSide {
    Income,
    Expense,
}

impl FlowSide {
    fn node_kind(self) -> SankeyNodeKind {
        match self {
            FlowSide::Income => SankeyNodeKind::Income,
            FlowSide::Expense => SankeyNodeKind::Expense,
        }
    }

    /// Node-id prefix. Ids stay `in:<category_id>` / `out:<category_id>`; they just now
    /// cover every level of the hierarchy rather than only the top one. A category has one
    /// parent, so it holds one position per side and the ids stay unique.
    fn prefix(self) -> &'static str {
        match self {
            FlowSide::Income => "in",
            FlowSide::Expense => "out",
        }
    }

    /// Orient a link between a node and whatever sits on its hub-ward side: income flows
    /// from the leaf towards the hub, expense the other way.
    fn link(self, node: String, inner: &str, value_minor: i64) -> SankeyLink {
        match self {
            FlowSide::Income => SankeyLink {
                source: node,
                target: inner.to_string(),
                value_minor,
            },
            FlowSide::Expense => SankeyLink {
                source: inner.to_string(),
                target: node,
                value_minor,
            },
        }
    }
}

/// One node of a side's roll-up forest, accumulated in base-currency *major* units so the
/// single rounding to minor units happens at emission (see [`crate::fx`]).
#[derive(Default)]
struct FlowNode {
    /// This category's spend including everything rolled up from below it.
    total_major: f64,
    depth: u8,
    root_id: Option<i64>,
    children: Vec<i64>,
}

/// One side's roll-up forest, keyed by category id ([`UNCATEGORISED`] for the bucket of
/// transactions with no category).
#[derive(Default)]
struct FlowForest {
    nodes: HashMap<i64, FlowNode>,
    roots: Vec<i64>,
}

impl FlowForest {
    /// Book `amount_major` against every node on `chain` (root first), creating nodes and
    /// parent→child edges on first sight. Every ancestor carries its descendants' spend,
    /// which is what makes a parent's link the sum of the subtree beneath it.
    fn add(&mut self, chain: &[i64], amount_major: f64) {
        let root_id = chain
            .first()
            .copied()
            .filter(|first| *first != UNCATEGORISED);
        for (depth, &id) in chain.iter().enumerate() {
            let entry = self.nodes.entry(id).or_insert(FlowNode {
                depth: depth as u8,
                root_id,
                ..FlowNode::default()
            });
            entry.total_major += amount_major;
            match depth.checked_sub(1) {
                None => {
                    if !self.roots.contains(&id) {
                        self.roots.push(id);
                    }
                }
                Some(parent_at) => {
                    // The parent was inserted on the previous iteration of this same chain.
                    let kids = &mut self
                        .nodes
                        .get_mut(&chain[parent_at])
                        .expect("parent inserted first")
                        .children;
                    if !kids.contains(&id) {
                        kids.push(id);
                    }
                }
            }
        }
    }
}

/// A flow node's display label: its category name, or the uncategorised bucket's.
fn flow_label(cats: &Categories, id: i64) -> String {
    if id == UNCATEGORISED {
        "Uncategorised".to_string()
    } else {
        cats.name_of(id)
    }
}

/// `ids` sorted biggest-first, then by label, then by id.
///
/// [`FlowForest::nodes`] is a `HashMap` and its iteration order isn't stable across
/// processes, so every list the emitter walks is sorted first. Otherwise two identical
/// requests return differently-ordered JSON — and because d3-sankey seeds its vertical
/// layout from node order, the picture would move between refreshes too.
fn flow_order(ids: &[i64], forest: &FlowForest, cats: &Categories, fx: &Fx) -> Vec<i64> {
    let value = |id: &i64| {
        forest
            .nodes
            .get(id)
            .map(|n| fx.base_minor(n.total_major))
            .unwrap_or(0)
    };
    let mut sorted = ids.to_vec();
    sorted.sort_by(|a, b| {
        value(b)
            .cmp(&value(a))
            .then_with(|| flow_label(cats, *a).cmp(&flow_label(cats, *b)))
            .then_with(|| a.cmp(b))
    });
    sorted
}

/// Emit `id`'s node, its whole subtree, and the link joining it to `inner` (its parent's
/// node id, or [`CENTER`] for a top-level category). Returns the value that link carries,
/// in minor units — 0 if nothing was emitted.
///
/// A category can hold transactions of its own *and* have children: $1,000 booked straight
/// onto "Employment" plus an "Employment > Salary". d3-sankey sizes a node as
/// `max(inflow, outflow)`, so that $1,000 shows up as a blank band on the node's inner
/// face — the money that came from the category itself rather than from anything further
/// out. That band is deliberate, and matches how the previous app drew it.
///
/// The `max` below only guarantees the band is never *negative*: a parent's f64 total and
/// its children's are rounded to minor units independently, so with several children the
/// children's rounded sum can exceed the parent's by a minor unit or two, which would
/// otherwise draw an inflow wider than the outflow.
#[allow(clippy::too_many_arguments)]
fn emit_flow_node(
    id: i64,
    inner: &str,
    side: FlowSide,
    forest: &FlowForest,
    cats: &Categories,
    fx: &Fx,
    nodes: &mut Vec<SankeyNode>,
    links: &mut Vec<SankeyLink>,
) -> i64 {
    let Some(node) = forest.nodes.get(&id) else {
        return 0;
    };
    let own_minor = fx.base_minor(node.total_major);
    if own_minor <= 0 {
        // Rounds to nothing at this currency's precision. Skipping the whole subtree is
        // safe: a child's total is never larger than its parent's.
        return 0;
    }

    let node_id = format!("{}:{id}", side.prefix());
    nodes.push(SankeyNode {
        id: node_id.clone(),
        label: flow_label(cats, id),
        kind: side.node_kind(),
        category_id: (id != UNCATEGORISED).then_some(id),
        depth: Some(node.depth),
        root_id: node.root_id,
        root_color: node.root_id.and_then(|root| cats.color_of(root)),
    });
    // Reserve this node's link slot: children are emitted after it so that the JSON reads
    // outward from the hub, but the link's value isn't known until they've been summed.
    let link_slot = links.len();
    links.push(side.link(node_id.clone(), inner, own_minor));

    let mut children_minor = 0;
    for child in flow_order(&node.children, forest, cats, fx) {
        children_minor += emit_flow_node(child, &node_id, side, forest, cats, fx, nodes, links);
    }

    let value_minor = own_minor.max(children_minor);
    links[link_slot].value_minor = value_minor;
    value_minor
}

// ---- service ---------------------------------------------------------------

/// The error for a `?currency=` that isn't in the `currencies` table.
///
/// A [`AppError::BadRequest`] (400), matching how `sure-api`'s `routes::reports` already
/// treats an unrecognised `interval` or `attributed_to`: an unusable query param is the
/// request's fault, and naming the offending code is the only way the caller can tell a typo
/// from an empty ledger. Shared with [`crate::forecast`], which takes the same param.
pub(crate) fn unknown_currency(code: &str) -> AppError {
    AppError::bad_request(format!(
        "unknown currency '{code}': not in the currencies table, so it has neither a \
         minor-unit scale nor any exchange rate"
    ))
}

pub struct ReportService {
    reports: Arc<dyn ReportRepo>,
    fx: Arc<dyn FxRatesRepo>,
    clock: Arc<dyn Clock>,
}

impl ReportService {
    pub fn new(
        reports: Arc<dyn ReportRepo>,
        fx: Arc<dyn FxRatesRepo>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self { reports, fx, clock }
    }

    async fn base_currency(&self, override_: Option<&str>) -> AppResult<String> {
        if let Some(c) = override_.filter(|s| !s.is_empty()) {
            return Ok(c.to_uppercase());
        }
        self.reports.base_currency().await
    }

    /// The report currency and the rate table loaded for it, refusing a `?currency=` that
    /// names no currency at all.
    ///
    /// The check is free: [`Fx`] already loads the `currencies` table for its decimal places,
    /// so [`Fx::try_dp`] answers "does this code exist" without another query. Every report
    /// goes through here so the answer is the same on all of them.
    ///
    /// Why it has to be an error rather than a best effort: with no `currencies` row a code
    /// has no minor-unit scale and no exchange rate (a rate row's currency is an FK into that
    /// table), so `?currency=ZZZ` produced a 200 describing a currency that does not exist —
    /// every account named in `unconverted`, every converted total zero. That reads as "the
    /// household is worth nothing in ZZZ", not as "ZZZ isn't a currency". `PUT /api/settings`
    /// has always refused an unknown base currency (`sure_dal::settings::update`); this is the
    /// same value arriving through a different door and it gets the same answer.
    async fn currency_and_fx(&self, override_: Option<&str>) -> AppResult<(String, Fx)> {
        let base = self.base_currency(override_).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;
        // Only an *override* is the caller's mistake. A `settings.base_currency_code` with no
        // `currencies` row is a server-side inconsistency, and answering a request that named
        // no currency at all with "unknown currency" would send the user hunting through a
        // query string they never wrote.
        if override_.is_some_and(|s| !s.is_empty()) && fx.try_dp(&base).is_none() {
            return Err(unknown_currency(&base));
        }
        Ok((base, fx))
    }

    /// Resolve the report window with data-driven defaults. A missing `from` defaults to
    /// the earliest transaction on record (true "all time"), not a rolling 12 months.
    async fn window(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> AppResult<(NaiveDate, NaiveDate)> {
        let today = self.clock.today();
        let to = to.and_then(parse_date).unwrap_or(today);
        let from = match from.and_then(parse_date) {
            Some(d) => d,
            None => self
                .reports
                .earliest_transaction_date()
                .await?
                .as_deref()
                .and_then(parse_date)
                .unwrap_or(to),
        };
        Ok((from, to))
    }

    /// Net worth over time, sampled at the requested interval.
    pub async fn net_worth(&self, q: &NetWorthQuery) -> AppResult<NetWorthSeries> {
        let (base, fx) = self.currency_and_fx(q.currency.as_deref()).await?;

        let mut accounts = self.reports.account_currencies().await?;
        if let Some(owner) = q.attributed_to {
            accounts.retain(|a| a.ownership == owner);
        }

        // Resolve the reporting window *before* reading any ledger row, because the window is
        // now what bounds that read. The default start used to be the minimum date over the
        // whole loaded ledger — the same date these two aggregates give, without materialising
        // every transaction and valuation in the database to find it. Both are restricted to
        // dates a report can actually read, matching what the old minimum-over-parsed-dates
        // skipped, and the fallback when the ledger is empty is unchanged.
        let today = self.clock.today();
        let to = q.to.as_deref().and_then(parse_date).unwrap_or(today);
        let from = match q.from.as_deref().and_then(parse_date) {
            Some(d) => d,
            None => {
                let earliest_tx = self.reports.earliest_transaction_date().await?;
                let earliest_val = self.reports.earliest_valuation_date().await?;
                [earliest_tx, earliest_val]
                    .iter()
                    .flatten()
                    .filter_map(|s| parse_date(s))
                    .min()
                    .unwrap_or_else(|| to - chrono::Duration::days(365))
            }
        };

        // `from.min(to)`, not `from`: an inverted window (`?from=2026-01-01&to=2020-01-01`)
        // samples a single point at `to`, *before* the window start, and a ledger seeded at
        // `from` cannot answer for a date below its own seed. Costs nothing in the normal case.
        let (tx_by_acct, val_by_acct) =
            load_ledger_from(self.reports.as_ref(), from.min(to)).await?;

        let sample_dates = sample_dates(from, to, q.interval.unwrap_or(Interval::Month));

        let mut points = Vec::with_capacity(sample_dates.len());
        for date in sample_dates {
            let mut assets = 0.0f64;
            let mut liabilities = 0.0f64;
            for a in &accounts {
                let (value_minor, ccy) =
                    account_value_at(a.id, &a.currency_code, date, &tx_by_acct, &val_by_acct);
                let Some(base_major) = fx.try_to_base_major(value_minor, &ccy) else {
                    // No rate: this account is outside the series entirely, and `ccy` is
                    // reported below. Counting it at parity is what made a $600 US holding
                    // read as $600 of net worth for years.
                    continue;
                };
                if base_major >= 0.0 {
                    assets += base_major;
                } else {
                    liabilities += base_major;
                }
            }
            points.push(NetWorthPoint {
                as_of: date.to_string(),
                net_worth_minor: fx.base_minor(assets + liabilities),
                assets_minor: fx.base_minor(assets),
                liabilities_minor: fx.base_minor(liabilities),
            });
        }

        Ok(NetWorthSeries {
            currency: base,
            points,
            unconverted: fx.unconverted(),
            rates_as_of: fx.rates_as_of().map(str::to_string),
        })
    }

    /// Income/expense totals per top-level category for the period.
    pub async fn category_breakdown(&self, q: &ReportQuery) -> AppResult<CategoryBreakdown> {
        let (base, fx) = self.currency_and_fx(q.currency.as_deref()).await?;
        let cats = Categories::load(self.reports.as_ref()).await?;
        let (from, to) = self.window(q.from.as_deref(), q.to.as_deref()).await?;
        let spend = load_spend(
            self.reports.as_ref(),
            &cats,
            from,
            to,
            q.include_one_off.unwrap_or(false),
            q.attributed_to,
        )
        .await?;

        // key 0 => uncategorised.
        let mut income: HashMap<i64, f64> = HashMap::new();
        let mut expense: HashMap<i64, f64> = HashMap::new();
        for t in &spend {
            let key = t.category_id.map(|c| cats.top_ancestor(c)).unwrap_or(0);
            // An unconvertible transaction is left out of the breakdown rather than added at
            // parity; `Fx` warns the pair once. This shape has no `unconverted` field to
            // carry the exclusion to the client the way [`NetWorthSeries`] does — a
            // per-category total is already a partial view, so a missing slice reads as one.
            let Some(base_major) = fx.try_to_base_major(t.amount_minor.abs(), &t.currency_code)
            else {
                continue;
            };
            if t.amount_minor >= 0 {
                *income.entry(key).or_default() += base_major;
            } else {
                *expense.entry(key).or_default() += base_major;
            }
        }

        let to_totals = |m: HashMap<i64, f64>| -> Vec<CategoryTotal> {
            let mut v: Vec<CategoryTotal> = m
                .into_iter()
                .map(|(key, total)| CategoryTotal {
                    category_id: (key != 0).then_some(key),
                    name: if key == 0 {
                        "Uncategorised".to_string()
                    } else {
                        cats.names.get(&key).cloned().unwrap_or_else(|| "?".into())
                    },
                    color: if key == 0 {
                        None
                    } else {
                        cats.colors.get(&key).cloned().flatten()
                    },
                    total_minor: fx.base_minor(total),
                })
                .collect();
            v.sort_by_key(|t| std::cmp::Reverse(t.total_minor));
            v
        };

        Ok(CategoryBreakdown {
            currency: base,
            from: from.to_string(),
            to: to.to_string(),
            income: to_totals(income),
            expense: to_totals(expense),
        })
    }

    /// Money-flow graph: income categories -> cash flow -> expense categories (+ savings),
    /// with up to [`SANKEY_MAX_DEPTH`] levels of category hierarchy fanning out from the
    /// hub on each side, so `Partly Group -> Employment -> Income -> Cash flow` reads as
    /// three columns rather than collapsing to one.
    pub async fn sankey(&self, q: &ReportQuery) -> AppResult<SankeyGraph> {
        let (base, fx) = self.currency_and_fx(q.currency.as_deref()).await?;
        let cats = Categories::load(self.reports.as_ref()).await?;
        let (from, to) = self.window(q.from.as_deref(), q.to.as_deref()).await?;
        let spend = load_spend(
            self.reports.as_ref(),
            &cats,
            from,
            to,
            q.include_one_off.unwrap_or(false),
            q.attributed_to,
        )
        .await?;

        let mut income = FlowForest::default();
        let mut expense = FlowForest::default();
        for t in &spend {
            let chain = match t.category_id {
                Some(cid) => cats.chain_to_depth(cid, SANKEY_MAX_DEPTH),
                None => vec![UNCATEGORISED],
            };
            // As in `category_breakdown`: no rate means the row is outside the graph, not
            // drawn at parity.
            let Some(base_major) = fx.try_to_base_major(t.amount_minor.abs(), &t.currency_code)
            else {
                continue;
            };
            // Sign of the amount, not the category's `kind` — see `FlowSide`.
            if t.amount_minor >= 0 {
                income.add(&chain, base_major);
            } else {
                expense.add(&chain, base_major);
            }
        }

        let mut nodes = vec![SankeyNode {
            id: CENTER.to_string(),
            label: "Cash flow".into(),
            kind: SankeyNodeKind::Center,
            category_id: None,
            depth: None,
            root_id: None,
            root_color: None,
        }];
        let mut links = Vec::new();

        let emit_side = |forest: &FlowForest,
                         side: FlowSide,
                         nodes: &mut Vec<SankeyNode>,
                         links: &mut Vec<SankeyLink>|
         -> i64 {
            // Each root's emitted value has already been through `fx.base_minor`, whose
            // `as i64` cast *saturates* — so a poisoned category total arrives here as
            // `i64::MAX` rather than as an error, and summing two of those is the same
            // panic-or-wrap this report was fixed for. `i128` accumulation, one loud narrowing.
            sum_minor(
                "sankey: side total across root categories",
                None,
                flow_order(&forest.roots, forest, &cats, &fx)
                    .into_iter()
                    .map(|root| {
                        emit_flow_node(root, CENTER, side, forest, &cats, &fx, nodes, links)
                    }),
            )
        };
        let income_minor = emit_side(&income, FlowSide::Income, &mut nodes, &mut links);
        let expense_minor = emit_side(&expense, FlowSide::Expense, &mut nodes, &mut links);

        // Surplus flows to savings. Taken from the emitted root links rather than from a
        // separately-rounded `total_income - total_expense`, so the hub's inflow and
        // outflow balance to the cent.
        if income_minor > expense_minor {
            nodes.push(SankeyNode {
                id: "savings".into(),
                label: "Savings".into(),
                kind: SankeyNodeKind::Savings,
                category_id: None,
                depth: None,
                root_id: None,
                root_color: None,
            });
            links.push(SankeyLink {
                source: CENTER.into(),
                target: "savings".into(),
                // Both sides are already-saturated `i64`s in the worst case, so subtract in
                // `i128` — `i64::MAX - i64::MIN` overflows, and the guard is worthless if the
                // very next line can still panic.
                value_minor: narrow_minor(
                    "sankey: surplus to savings",
                    None,
                    i128::from(income_minor) - i128::from(expense_minor),
                ),
            });
        }

        Ok(SankeyGraph {
            currency: base,
            nodes,
            links,
        })
    }

    /// Current value of each (non-archived) account plus a base-currency total.
    pub async fn balances(&self, q: &ReportQuery) -> AppResult<BalancesReport> {
        let (base, fx) = self.currency_and_fx(q.currency.as_deref()).await?;
        let as_of =
            q.to.as_deref()
                .and_then(parse_date)
                .unwrap_or_else(|| self.clock.today());

        let accounts = self.reports.active_accounts().await?;
        // One day is the whole window here: the balance sheet asks every account what it is
        // worth on `as_of` and nothing else, so the ledger it needs is that day's rows plus the
        // per-account seed standing in for all of history before it.
        let (tx_by_acct, val_by_acct) = load_ledger_from(self.reports.as_ref(), as_of).await?;

        let mut out = Vec::new();
        let mut total = 0.0;
        for a in &accounts {
            let (value_minor, ccy) =
                account_value_at(a.id, &a.currency_code, as_of, &tx_by_acct, &val_by_acct);
            // The row is listed either way — its own-currency balance is a true figure. Only
            // the base-currency roll-up has to leave it out when no rate reaches it.
            if let Some(base_major) = fx.try_to_base_major(value_minor, &ccy) {
                total += base_major;
            }
            out.push(AccountBalance {
                ownership: a.ownership,
                account_id: a.id,
                name: a.name.clone(),
                kind: a.kind,
                class: a.kind.class(),
                currency_code: ccy,
                value_minor,
            });
        }

        Ok(BalancesReport {
            currency: base,
            as_of: as_of.to_string(),
            total_minor: fx.base_minor(total),
            accounts: out,
            unconverted: fx.unconverted(),
            rates_as_of: fx.rates_as_of().map(str::to_string),
        })
    }

    /// The equity position of an asset: its value, the liabilities secured against it,
    /// total debt, equity, and the paid-off percentage.
    pub async fn equity_position(&self, id: i64, q: &ReportQuery) -> AppResult<EquityPosition> {
        let (base, fx) = self.currency_and_fx(q.currency.as_deref()).await?;
        let as_of =
            q.to.as_deref()
                .and_then(parse_date)
                .unwrap_or_else(|| self.clock.today());

        let asset = self.reports.account(id).await?;
        let liabs = self.reports.secured_liabilities(id).await?;

        // As in `balances`: a single day, so the window is that day plus its seed.
        let (tx_by_acct, val_by_acct) = load_ledger_from(self.reports.as_ref(), as_of).await?;

        let (v_minor, v_ccy) = account_value_at(
            asset.id,
            &asset.currency_code,
            as_of,
            &tx_by_acct,
            &val_by_acct,
        );
        // Equity is a subtraction, so it cannot survive a dropped term: an asset counted and
        // a secured debt silently omitted reads as a house owned outright. Either every leg
        // converts or this report refuses — unlike net worth, there is no partial answer here
        // worth showing.
        let Some(value_base) = fx.try_to_base_major(v_minor, &v_ccy) else {
            return Err(fx.missing_rate_error());
        };
        let value_base = value_base.max(0.0);

        let mut total_debt = 0.0;
        let mut liabilities = Vec::new();
        for l in &liabs {
            let (lm, lccy) =
                account_value_at(l.id, &l.currency_code, as_of, &tx_by_acct, &val_by_acct);
            // Liabilities carry a negative balance; the debt is its magnitude.
            let Some(lm_base) = fx.try_to_base_major(lm, &lccy) else {
                return Err(fx.missing_rate_error());
            };
            let debt = lm_base.min(0.0).abs();
            total_debt += debt;
            liabilities.push(SecuredLiability {
                account_id: l.id,
                name: l.name.clone(),
                kind: l.kind,
                balance_minor: fx.base_minor(debt),
            });
        }

        let equity = value_base - total_debt;
        let paid_off_pct = if value_base > 0.0 {
            ((equity / value_base) * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };

        Ok(EquityPosition {
            account_id: asset.id,
            name: asset.name,
            currency: base,
            as_of: as_of.to_string(),
            value_minor: fx.base_minor(value_base),
            total_debt_minor: fx.base_minor(total_debt),
            equity_minor: fx.base_minor(equity),
            paid_off_pct,
            liabilities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A provider-synced mortgage: only *today's* balance is known as a valuation, and its
    /// own transactions (a large drawdown that alone sums to the wrong sign, plus small
    /// repayments) don't reconcile to that balance from zero. Historically it must still
    /// read as its true negative liability, not a positive transaction-sum — the bug that
    /// spiked net worth by ~$1.1M when a house was bought against it.
    #[test]
    fn mortgage_reconstructs_its_negative_balance_backwards_from_todays_valuation() {
        let mut tx = HashMap::new();
        tx.insert(
            7i64,
            vec![
                (d("2025-12-11"), 485_000_00), // drawdown (Akahu signs it +, unlike the balance)
                (d("2026-01-12"), 313_81),     // principal repayment (reduces the debt)
                (d("2026-02-09"), 434_70),
            ],
        );
        // Provider reports today's balance: -$484,251.49 (= -485000 + 313.81 + 434.70).
        let mut val = HashMap::new();
        val.insert(
            7i64,
            vec![(d("2026-07-19"), -484_251_49, "NZD".to_string())],
        );

        // Before the drawdown: the mortgage doesn't exist yet.
        assert_eq!(account_value_at(7, "NZD", d("2025-12-01"), &tx, &val).0, 0);
        // Right after the drawdown, before any repayments: the full -$485,000.
        assert_eq!(
            account_value_at(7, "NZD", d("2025-12-15"), &tx, &val).0,
            -485_000_00
        );
        // After the first repayment: -$484,686.19.
        assert_eq!(
            account_value_at(7, "NZD", d("2026-01-20"), &tx, &val).0,
            -484_686_19
        );
        // On/after the valuation date: exactly the provider's figure.
        assert_eq!(
            account_value_at(7, "NZD", d("2026-07-19"), &tx, &val).0,
            -484_251_49
        );
    }

    /// A property carries its manual valuation forward from the purchase date, and reads 0
    /// before it (it wasn't owned yet) rather than being back-projected.
    #[test]
    fn property_is_zero_before_purchase_and_valued_after() {
        let tx = HashMap::new(); // a house has no transactions
        let mut val = HashMap::new();
        val.insert(9i64, vec![(d("2025-12-12"), 770_000_00, "NZD".to_string())]);

        assert_eq!(account_value_at(9, "NZD", d("2025-12-01"), &tx, &val).0, 0);
        assert_eq!(
            account_value_at(9, "NZD", d("2025-12-12"), &tx, &val).0,
            770_000_00
        );
        assert_eq!(
            account_value_at(9, "NZD", d("2026-06-01"), &tx, &val).0,
            770_000_00
        );
    }

    /// A plain cash account with no valuations still uses the running transaction balance.
    #[test]
    fn cash_account_without_valuations_sums_transactions() {
        let mut tx = HashMap::new();
        tx.insert(
            3i64,
            vec![(d("2026-01-05"), 500_00), (d("2026-01-10"), -120_00)],
        );
        let val = HashMap::new();

        assert_eq!(account_value_at(3, "NZD", d("2026-01-01"), &tx, &val).0, 0);
        assert_eq!(
            account_value_at(3, "NZD", d("2026-01-07"), &tx, &val).0,
            500_00
        );
        assert_eq!(
            account_value_at(3, "NZD", d("2026-01-31"), &tx, &val).0,
            380_00
        );
    }

    /// Two rows at the wire ceiling — the largest amount `sure_core::Money` will accept — must
    /// sum *exactly*, not saturate. This is the guard against over-tightening: the second layer
    /// only exists for figures a legal ledger cannot reach, and a false clamp here would show a
    /// wrong balance for data that was accepted correctly.
    ///
    /// The tuples are raw `i64`s, which is precisely how the DAL hands these rows over — so
    /// this exercises the aggregation without the wire type in the picture at all.
    #[test]
    fn two_rows_at_the_wire_ceiling_still_sum_exactly() {
        let ceiling = sure_core::MAX_MONEY_MINOR;
        let mut tx = HashMap::new();
        tx.insert(
            11i64,
            vec![(d("2026-01-05"), ceiling), (d("2026-01-06"), ceiling)],
        );
        let val = HashMap::new();

        assert_eq!(
            account_value_at(11, "NZD", d("2026-02-01"), &tx, &val).0,
            2 * ceiling,
            "the ceiling is chosen so this fits in an i64 with room to spare"
        );
    }

    /// The failure this whole change exists for, at the layer that covers rows **already on
    /// disk**: two `i64::MAX` transactions — which the wire type now refuses but which a row
    /// written before it existed can still hold — used to panic here in debug (a scrubbed 500
    /// on the balance sheet, net worth, equity and forecast at once) and wrap to a small
    /// negative in release (a plausible, wrong balance with no error anywhere).
    ///
    /// Now it saturates: obviously-wrong on screen, and a WARN naming the account to repair.
    /// Nothing about `Money` is involved — these tuples are raw `i64`s straight off a DAL row,
    /// which is exactly the point of testing layer 2 on its own.
    #[test]
    fn a_pre_existing_over_ceiling_row_saturates_instead_of_panicking_or_wrapping() {
        let mut tx = HashMap::new();
        tx.insert(
            12i64,
            vec![(d("2026-01-05"), i64::MAX), (d("2026-01-06"), i64::MAX)],
        );
        let val = HashMap::new();

        assert_eq!(
            account_value_at(12, "NZD", d("2026-02-01"), &tx, &val).0,
            i64::MAX,
            "must clamp at the ceiling of the type, not wrap negative"
        );

        // The same in the other direction: `i64::MIN + i64::MIN` wraps to 0 in release, which
        // reads as "this account is empty".
        let mut tx = HashMap::new();
        tx.insert(
            13i64,
            vec![(d("2026-01-05"), i64::MIN), (d("2026-01-06"), i64::MIN)],
        );
        assert_eq!(
            account_value_at(13, "NZD", d("2026-02-01"), &tx, &val).0,
            i64::MIN
        );
    }

    /// Case 2 — the valuation-anchor reconstruction — subtracts, so it overflows on *opposite*
    /// signs: an `i64::MIN` anchor less a positive movement. A guard on the forward sum alone
    /// would have left this one panicking.
    #[test]
    fn the_valuation_anchor_subtraction_saturates_too() {
        let mut tx = HashMap::new();
        tx.insert(
            14i64,
            vec![(d("2026-01-05"), i64::MAX), (d("2026-01-06"), i64::MAX)],
        );
        let mut val = HashMap::new();
        // The anchor is later than both movements, so `date` sits between the first
        // transaction and the valuation: case 2 walks backwards from it.
        val.insert(14i64, vec![(d("2026-03-01"), i64::MIN, "NZD".to_string())]);

        assert_eq!(
            account_value_at(14, "NZD", d("2026-01-05"), &tx, &val).0,
            i64::MIN,
            "anchor − later movements must clamp, not wrap"
        );
    }

    /// The narrowing helper itself: exact in range, clamped and *signed correctly* outside it.
    #[test]
    fn narrow_minor_is_exact_in_range_and_clamps_outside_it() {
        assert_eq!(narrow_minor("t", None, 0), 0);
        assert_eq!(narrow_minor("t", None, -4250), -4250);
        assert_eq!(narrow_minor("t", None, i128::from(i64::MAX)), i64::MAX);
        assert_eq!(narrow_minor("t", None, i128::from(i64::MIN)), i64::MIN);
        assert_eq!(narrow_minor("t", None, i128::from(i64::MAX) + 1), i64::MAX);
        assert_eq!(narrow_minor("t", None, i128::from(i64::MIN) - 1), i64::MIN);

        // `sum_minor` over an empty iterator is 0, not a clamp — an account with no
        // transactions in the window is worth nothing, and must not warn.
        assert_eq!(sum_minor("t", None, std::iter::empty()), 0);
        assert_eq!(sum_minor("t", None, [1_00, -2_50, 3_00].into_iter()), 1_50);
        // Enough ceiling-magnitude rows to leave `i64` behind entirely: the `i128`
        // accumulator absorbs them all and only the narrowing clamps.
        assert_eq!(
            sum_minor("t", None, std::iter::repeat_n(i64::MAX, 1000)),
            i64::MAX
        );
    }

    /// Seven years of bank history imported behind a feed that only knows today's balance:
    /// the figures are right from the first transaction onward, and 0 before it, because
    /// case 2 reads an account as not-yet-opened there rather than back-projecting.
    ///
    /// That is correct for an account whose ledger really does start at its first row, and
    /// wrong for an imported window that starts mid-life — the account appears out of thin
    /// air at whatever the first day's movements leave behind (here $1,000, after two
    /// transfers out of an $18,694.18 balance). `sure_providers::asb` closes the gap by
    /// writing an opening-balance transaction; the second half of this test is why that
    /// works. Don't "fix" the cliff here: extrapolating a balance backwards past the
    /// earliest thing on record would invent history for every account in the database.
    #[test]
    fn an_imported_history_reads_zero_before_its_earliest_transaction() {
        // The ASB export's rows: $17,694.18 out on day one, $900 spent later. They sum to
        // -$18,594.18, and the account is recorded at $100 today.
        let movements = vec![
            (d("2020-01-01"), -15_694_18),
            (d("2020-01-01"), -2_000_00),
            (d("2021-05-05"), -900_00),
        ];
        let mut val = HashMap::new();
        val.insert(8i64, vec![(d("2026-08-03"), 100_00, "NZD".to_string())]);

        let mut tx = HashMap::new();
        tx.insert(8i64, movements.clone());

        // From the first row on, every figure is already right.
        assert_eq!(
            account_value_at(8, "NZD", d("2020-01-01"), &tx, &val).0,
            1_000_00
        );
        assert_eq!(
            account_value_at(8, "NZD", d("2021-05-05"), &tx, &val).0,
            100_00
        );
        assert_eq!(
            account_value_at(8, "NZD", d("2026-08-03"), &tx, &val).0,
            100_00
        );
        // But the day before the import starts reads 0, not the $18,694.18 held then.
        assert_eq!(account_value_at(8, "NZD", d("2019-12-31"), &tx, &val).0, 0);

        // With an opening-balance transaction dated the day before, the whole series is
        // right — and the ledger reconciles: -18,594.18 + 18,694.18 == the $100 recorded.
        let mut with_opening = movements;
        with_opening.push((d("2019-12-31"), 18_694_18));
        let mut tx = HashMap::new();
        tx.insert(8i64, with_opening);

        assert_eq!(account_value_at(8, "NZD", d("2019-12-30"), &tx, &val).0, 0);
        assert_eq!(
            account_value_at(8, "NZD", d("2019-12-31"), &tx, &val).0,
            18_694_18
        );
        // The dates that were already correct stay correct.
        assert_eq!(
            account_value_at(8, "NZD", d("2020-01-01"), &tx, &val).0,
            1_000_00
        );
        assert_eq!(
            account_value_at(8, "NZD", d("2026-08-03"), &tx, &val).0,
            100_00
        );
    }

    /// Why an opening balance must be a *transaction* and never a valuation: case 1 wins over
    /// case 2, and it returns the most recent valuation on or before the date **directly**,
    /// without applying any transaction since. A valuation placed at the start of an imported
    /// history therefore freezes the account at that figure for every date after it, hiding
    /// the very movements the import was for.
    #[test]
    fn an_early_valuation_freezes_the_account_and_hides_later_movements() {
        let mut tx = HashMap::new();
        tx.insert(
            8i64,
            vec![
                (d("2020-01-01"), -15_694_18),
                (d("2020-01-01"), -2_000_00),
                (d("2021-05-05"), -900_00),
            ],
        );
        let mut val = HashMap::new();
        val.insert(
            8i64,
            vec![
                // The opening balance, wrongly recorded as a valuation …
                (d("2019-12-31"), 18_694_18, "NZD".to_string()),
                (d("2026-08-03"), 100_00, "NZD".to_string()),
            ],
        );

        // … and now 2021 reports the opening figure rather than the $1,000 actually held:
        // the two transfers out have vanished from the history.
        assert_eq!(
            account_value_at(8, "NZD", d("2021-01-01"), &tx, &val).0,
            18_694_18
        );
        assert_eq!(
            account_value_at(8, "NZD", d("2025-01-01"), &tx, &val).0,
            18_694_18
        );
        // Only from the later valuation does it come right again.
        assert_eq!(
            account_value_at(8, "NZD", d("2026-08-03"), &tx, &val).0,
            100_00
        );
    }

    /// The instrument-bookkeeping kinds stay out of the income/expense reports, while the
    /// everyday transaction accounts — including the two *liability* ones — stay in. A
    /// student loan's repayments are positive amounts on a liability, so dropping it from
    /// this list would report them as household income.
    #[test]
    fn only_instrument_bookkeeping_kinds_are_excluded_from_spend() {
        for kind in [
            AccountKind::Mortgage,
            AccountKind::Loan,
            AccountKind::StudentLoan,
            AccountKind::Brokerage,
        ] {
            assert!(is_excluded_from_spend(kind), "{kind:?} should be excluded");
        }
        for kind in [
            AccountKind::Bank,
            AccountKind::Savings,
            AccountKind::Cash,
            AccountKind::CreditCard,
            AccountKind::RevolvingCredit,
            // A generic "other liability" has no instrument bookkeeping of its own — it's
            // whatever the user says it is, so its transactions stay in the report.
            AccountKind::Liability,
            AccountKind::RealEstate,
            AccountKind::SharesNz,
        ] {
            assert!(
                !is_excluded_from_spend(kind),
                "{kind:?} should not be excluded"
            );
        }
    }

    // ---- `?currency=` validation (fake ports) --------------------------------------

    mod currency {
        use super::*;
        use crate::ports::{
            AccountCurrency, ActiveAccount, AssetAccount, CurrencyDecimals, ExchangeRateRow,
            LedgerTx, LedgerValuation, ReportCategory, SecuredLiabilityAccount,
        };
        use async_trait::async_trait;

        /// Only the handful of reads a report of an empty ledger performs; everything else
        /// would mean the test drifted into exercising aggregation, which the tests above
        /// already cover directly.
        struct FakeReports;
        #[async_trait]
        impl ReportRepo for FakeReports {
            async fn base_currency(&self) -> AppResult<String> {
                Ok("NZD".to_string())
            }
            async fn account_currencies(&self) -> AppResult<Vec<AccountCurrency>> {
                Ok(Vec::new())
            }
            async fn transactions(&self, _from: Option<NaiveDate>) -> AppResult<Vec<LedgerTx>> {
                Ok(Vec::new())
            }
            async fn valuations(
                &self,
                _from: Option<NaiveDate>,
            ) -> AppResult<Vec<LedgerValuation>> {
                Ok(Vec::new())
            }
            async fn categories(&self) -> AppResult<Vec<ReportCategory>> {
                Ok(Vec::new())
            }
            async fn spend_transactions(
                &self,
                _from: NaiveDate,
                _to: NaiveDate,
            ) -> AppResult<Vec<SpendTransaction>> {
                Ok(Vec::new())
            }
            async fn earliest_transaction_date(&self) -> AppResult<Option<String>> {
                Ok(None)
            }
            async fn earliest_valuation_date(&self) -> AppResult<Option<String>> {
                Ok(None)
            }
            async fn active_accounts(&self) -> AppResult<Vec<ActiveAccount>> {
                Ok(Vec::new())
            }
            async fn account(&self, _id: i64) -> AppResult<AssetAccount> {
                unreachable!()
            }
            async fn secured_liabilities(
                &self,
                _asset_id: i64,
            ) -> AppResult<Vec<SecuredLiabilityAccount>> {
                unreachable!()
            }
        }

        /// NZD and USD are real, ZZZ is not — exactly the shape of the `currencies` table,
        /// which is what makes an unknown code detectable without a second query.
        struct FakeFx;
        #[async_trait]
        impl FxRatesRepo for FakeFx {
            async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>> {
                Ok(["NZD", "USD"]
                    .into_iter()
                    .map(|code| CurrencyDecimals {
                        code: code.to_string(),
                        decimal_places: 2,
                    })
                    .collect())
            }
            async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>> {
                Ok(vec![ExchangeRateRow {
                    base_code: "NZD".into(),
                    quote_code: "USD".into(),
                    rate: "0.6".into(),
                    as_of: "2026-08-01".into(),
                }])
            }
        }

        fn service() -> ReportService {
            ReportService::new(
                Arc::new(FakeReports),
                Arc::new(FakeFx),
                Arc::new(crate::test_clock::FixedClock(d("2026-08-03"))),
            )
        }

        fn query(currency: Option<&str>) -> ReportQuery {
            ReportQuery {
                currency: currency.map(str::to_string),
                ..Default::default()
            }
        }

        /// The W-16 guard. `?currency=ZZZ` used to return 200 with every account named in
        /// `unconverted` and every total zero — a report that reads as "the household is
        /// worth nothing" rather than "that isn't a currency". It has to be a 400 naming the
        /// code, like an unrecognised `interval`.
        #[test]
        fn an_unknown_currency_is_a_bad_request_on_every_report() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = service();
            let q = query(Some("ZZZ"));

            let errors: Vec<AppError> = vec![
                rt.block_on(svc.balances(&q)).expect_err("balances"),
                rt.block_on(svc.category_breakdown(&q))
                    .expect_err("category_breakdown"),
                rt.block_on(svc.sankey(&q)).expect_err("sankey"),
                rt.block_on(svc.net_worth(&NetWorthQuery {
                    currency: Some("ZZZ".into()),
                    ..Default::default()
                }))
                .expect_err("net_worth"),
                rt.block_on(svc.equity_position(1, &q))
                    .expect_err("equity_position"),
            ];
            for err in errors {
                assert_eq!(err.code(), "bad_request", "got {err:?}");
                assert!(
                    err.to_string().contains("ZZZ"),
                    "the message must name the offending code: {err}"
                );
            }
        }

        /// Lower case is normalised, not rejected — the check is on the currency's existence,
        /// not on how the caller typed it.
        #[test]
        fn a_known_currency_still_works_in_any_case() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let svc = service();
            for code in ["USD", "usd"] {
                let report = rt.block_on(svc.balances(&query(Some(code)))).unwrap();
                assert_eq!(report.currency, "USD");
            }
            // …and omitting it falls back to the configured base as before.
            assert_eq!(
                rt.block_on(svc.balances(&query(None))).unwrap().currency,
                "NZD"
            );
            // An empty `?currency=` is "not supplied", which is how it has always behaved —
            // rejecting it would break a client that renders the param unconditionally.
            assert_eq!(
                rt.block_on(svc.balances(&query(Some(""))))
                    .unwrap()
                    .currency,
                "NZD"
            );
        }
    }

    // ---- W-14: a windowed ledger read must not change a single figure ----------------

    /// The proof obligation behind pushing the report window into SQL: for the same query, a
    /// repo that returns *everything* (literally what these reads did before) and one that
    /// returns only the window plus a per-account seed must produce byte-identical reports.
    ///
    /// The fixture is the shape of a real household, not its identifiers: a bank account whose
    /// entire history predates the window, a provider-synced mortgage whose only valuation is
    /// months *after* the window's end (the case-2 backward reconstruction — the reason there is
    /// no upper bound on the ledger read), and a house valued once, years before the window
    /// (case 1 reaching back past `from`). Each of the three is a different way a naive window
    /// would silently produce a wrong balance.
    mod windowing {
        use super::*;
        use crate::ports::{
            AccountCurrency, ActiveAccount, AssetAccount, CurrencyDecimals, ExchangeRateRow,
            LedgerTx, LedgerValuation, ReportCategory, SecuredLiabilityAccount,
        };
        use async_trait::async_trait;

        const BANK: i64 = 1;
        const MORTGAGE: i64 = 2;
        const HOUSE: i64 = 3;

        /// How the fake answers a windowed read.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        enum Mode {
            /// Ignore the window and hand back every row — exactly what `SqliteStore` did before
            /// W-14, so a report computed over this repo *is* the "before" figure.
            Everything,
            /// Apply the window and collapse the pre-history into one seed row per account, the
            /// way `sure_dal::reports` now does. Modelling the contract here rather than calling
            /// the SQL keeps this a test of the *aggregation* (that a seeded ledger is enough);
            /// `sure_dal::reports`' own tests are what pin the SQL to the same contract.
            Windowed,
        }

        struct FakeReports {
            mode: Mode,
            txns: Vec<LedgerTx>,
            vals: Vec<LedgerValuation>,
            spend: Vec<SpendTransaction>,
        }

        /// The whole fixture ledger, in both tables.
        fn ledger() -> (Vec<LedgerTx>, Vec<LedgerValuation>) {
            let tx = |account_id, posted_at: &str, amount_minor| LedgerTx {
                account_id,
                posted_at: posted_at.to_string(),
                amount_minor,
            };
            let val = |account_id, as_of: &str, value_minor| LedgerValuation {
                account_id,
                as_of: as_of.to_string(),
                value_minor,
                currency_code: "NZD".to_string(),
            };
            (
                vec![
                    // Six years of bank history, all of it before the window.
                    tx(BANK, "2020-01-01", 1_000_00),
                    tx(BANK, "2021-06-15", -300_00),
                    tx(BANK, "2026-03-10", 50_00),
                    // The mortgage: a drawdown Akahu signs positive, then repayments.
                    tx(MORTGAGE, "2025-12-11", 485_000_00),
                    tx(MORTGAGE, "2026-01-12", 313_81),
                    tx(MORTGAGE, "2026-02-09", 434_70),
                ],
                vec![
                    // The house, valued once at purchase and never since.
                    val(HOUSE, "2020-01-01", 770_000_00),
                    // The mortgage's only valuation: what the feed reports *today*, which is
                    // five months past the end of the window asked about below.
                    val(MORTGAGE, "2026-07-19", -484_251_49),
                ],
            )
        }

        fn spend_rows() -> Vec<SpendTransaction> {
            let row = |posted_at: &str, amount_minor, category_id| SpendTransaction {
                posted_at: posted_at.to_string(),
                amount_minor,
                currency_code: "NZD".to_string(),
                category_id: Some(category_id),
                is_one_off: false,
                linked_transaction_id: None,
                account_kind: AccountKind::Bank,
                attribution: Ownership::Joint,
            };
            vec![
                row("2019-12-31", 9_999_00, 10),  // long before the window
                row("2026-01-05", 5_000_00, 10),  // income, inside
                row("2026-01-20", -1_200_00, 20), // expense, inside
                row("2026-02-28", -300_00, 20),   // expense, inside (last day counts)
                row("2026-06-01", -777_00, 20),   // after the window
            ]
        }

        impl FakeReports {
            fn new(mode: Mode) -> Self {
                let (txns, vals) = ledger();
                Self {
                    mode,
                    txns,
                    vals,
                    spend: spend_rows(),
                }
            }
        }

        /// The seeding contract, in Rust: every row on/after `from`, plus one synthetic row per
        /// account holding the sum of everything before it, dated at the latest of them.
        fn seeded_transactions(rows: &[LedgerTx], from: NaiveDate) -> Vec<LedgerTx> {
            let mut seeds: HashMap<i64, (String, i64)> = HashMap::new();
            let mut out = Vec::new();
            for t in rows {
                let d = parse_date(&t.posted_at).expect("fixture dates parse");
                if d >= from {
                    out.push(t.clone());
                    continue;
                }
                let seed = seeds
                    .entry(t.account_id)
                    .or_insert_with(|| (t.posted_at.clone(), 0));
                if t.posted_at > seed.0 {
                    seed.0 = t.posted_at.clone();
                }
                seed.1 += t.amount_minor;
            }
            let mut seeds: Vec<_> = seeds.into_iter().collect();
            seeds.sort_by_key(|(id, _)| *id);
            let mut all: Vec<LedgerTx> = seeds
                .into_iter()
                .map(|(account_id, (posted_at, amount_minor))| LedgerTx {
                    account_id,
                    posted_at,
                    amount_minor,
                })
                .collect();
            all.append(&mut out);
            all
        }

        /// Same, for valuations: every row as of `from` or later, plus the latest earlier one
        /// per account (a level, not a movement, so it is a real row rather than a total).
        fn seeded_valuations(rows: &[LedgerValuation], from: NaiveDate) -> Vec<LedgerValuation> {
            let mut seeds: HashMap<i64, LedgerValuation> = HashMap::new();
            let mut out = Vec::new();
            for v in rows {
                let d = parse_date(&v.as_of).expect("fixture dates parse");
                if d >= from {
                    out.push(v.clone());
                    continue;
                }
                match seeds.get(&v.account_id) {
                    Some(prev) if prev.as_of >= v.as_of => {}
                    Some(_) | None => {
                        seeds.insert(v.account_id, v.clone());
                    }
                }
            }
            let mut seeds: Vec<_> = seeds.into_iter().collect();
            seeds.sort_by_key(|(id, _)| *id);
            let mut all: Vec<LedgerValuation> = seeds.into_iter().map(|(_, v)| v).collect();
            all.append(&mut out);
            all
        }

        #[async_trait]
        impl ReportRepo for FakeReports {
            async fn base_currency(&self) -> AppResult<String> {
                Ok("NZD".to_string())
            }
            async fn account_currencies(&self) -> AppResult<Vec<AccountCurrency>> {
                Ok([BANK, MORTGAGE, HOUSE]
                    .into_iter()
                    .map(|id| AccountCurrency {
                        id,
                        currency_code: "NZD".to_string(),
                        ownership: Ownership::Joint,
                    })
                    .collect())
            }
            async fn transactions(&self, from: Option<NaiveDate>) -> AppResult<Vec<LedgerTx>> {
                Ok(match (self.mode, from) {
                    (Mode::Everything, _) | (Mode::Windowed, None) => self.txns.clone(),
                    (Mode::Windowed, Some(from)) => seeded_transactions(&self.txns, from),
                })
            }
            async fn valuations(&self, from: Option<NaiveDate>) -> AppResult<Vec<LedgerValuation>> {
                Ok(match (self.mode, from) {
                    (Mode::Everything, _) | (Mode::Windowed, None) => self.vals.clone(),
                    (Mode::Windowed, Some(from)) => seeded_valuations(&self.vals, from),
                })
            }
            async fn categories(&self) -> AppResult<Vec<ReportCategory>> {
                Ok(vec![
                    ReportCategory {
                        id: 10,
                        parent_id: None,
                        name: "Salary".to_string(),
                        color: None,
                        kind: CategoryKind::Income,
                    },
                    ReportCategory {
                        id: 20,
                        parent_id: None,
                        name: "Groceries".to_string(),
                        color: None,
                        kind: CategoryKind::Expense,
                    },
                ])
            }
            async fn spend_transactions(
                &self,
                from: NaiveDate,
                to: NaiveDate,
            ) -> AppResult<Vec<SpendTransaction>> {
                Ok(match self.mode {
                    Mode::Everything => self.spend.clone(),
                    Mode::Windowed => self
                        .spend
                        .iter()
                        .filter(|t| parse_date(&t.posted_at).is_some_and(|d| d >= from && d <= to))
                        .cloned()
                        .collect(),
                })
            }
            async fn earliest_transaction_date(&self) -> AppResult<Option<String>> {
                Ok(self.txns.iter().map(|t| t.posted_at.clone()).min())
            }
            async fn earliest_valuation_date(&self) -> AppResult<Option<String>> {
                Ok(self.vals.iter().map(|v| v.as_of.clone()).min())
            }
            async fn active_accounts(&self) -> AppResult<Vec<ActiveAccount>> {
                Ok(vec![
                    ActiveAccount {
                        id: BANK,
                        name: "Everyday".to_string(),
                        kind: AccountKind::Bank,
                        currency_code: "NZD".to_string(),
                        ownership: Ownership::Joint,
                    },
                    ActiveAccount {
                        id: MORTGAGE,
                        name: "Home loan".to_string(),
                        kind: AccountKind::Mortgage,
                        currency_code: "NZD".to_string(),
                        ownership: Ownership::Joint,
                    },
                    ActiveAccount {
                        id: HOUSE,
                        name: "The house".to_string(),
                        kind: AccountKind::RealEstate,
                        currency_code: "NZD".to_string(),
                        ownership: Ownership::Joint,
                    },
                ])
            }
            async fn account(&self, id: i64) -> AppResult<AssetAccount> {
                assert_eq!(id, HOUSE, "only the house is asked for an equity position");
                Ok(AssetAccount {
                    id: HOUSE,
                    name: "The house".to_string(),
                    currency_code: "NZD".to_string(),
                })
            }
            async fn secured_liabilities(
                &self,
                asset_id: i64,
            ) -> AppResult<Vec<SecuredLiabilityAccount>> {
                assert_eq!(asset_id, HOUSE);
                Ok(vec![SecuredLiabilityAccount {
                    id: MORTGAGE,
                    name: "Home loan".to_string(),
                    kind: AccountKind::Mortgage,
                    currency_code: "NZD".to_string(),
                }])
            }
        }

        /// NZD only, no rates needed: the report currency is the base, so nothing converts and
        /// the figures below are the raw minor units.
        struct FakeFx;
        #[async_trait]
        impl FxRatesRepo for FakeFx {
            async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>> {
                Ok(vec![CurrencyDecimals {
                    code: "NZD".to_string(),
                    decimal_places: 2,
                }])
            }
            async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>> {
                Ok(Vec::new())
            }
        }

        fn service(mode: Mode) -> ReportService {
            ReportService::new(
                Arc::new(FakeReports::new(mode)),
                Arc::new(FakeFx),
                Arc::new(crate::test_clock::FixedClock(d("2026-08-04"))),
            )
        }

        fn window(from: &str, to: &str) -> ReportQuery {
            ReportQuery {
                from: Some(from.to_string()),
                to: Some(to.to_string()),
                ..Default::default()
            }
        }

        /// A balance sheet mid-window: the seeded read must give the same three balances, and
        /// they must be these ones. Each is a different reach past the window's edges — the
        /// bank's whole history is behind it, the house's only valuation is six years behind it,
        /// and the mortgage's anchor valuation is five months ahead of it.
        #[test]
        fn a_balance_sheet_is_identical_windowed_and_unwindowed() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let q = window("2026-01-01", "2026-02-01");

            let before = rt.block_on(service(Mode::Everything).balances(&q)).unwrap();
            let after = rt.block_on(service(Mode::Windowed).balances(&q)).unwrap();

            let value = |r: &BalancesReport, id: i64| {
                r.accounts
                    .iter()
                    .find(|a| a.account_id == id)
                    .unwrap_or_else(|| panic!("account {id} missing"))
                    .value_minor
            };
            // Pinned figures, so this test fails if either side moves rather than only if they
            // move apart. $1,000 in less $300 out; the house at its purchase valuation; the
            // mortgage reconstructed backwards from July's -$484,251.49 less the $434.70 repaid
            // after 1 February.
            assert_eq!(value(&after, BANK), 700_00);
            assert_eq!(value(&after, HOUSE), 770_000_00);
            assert_eq!(value(&after, MORTGAGE), -484_686_19);
            assert_eq!(after.total_minor, 286_013_81);

            assert_eq!(value(&before, BANK), value(&after, BANK));
            assert_eq!(value(&before, HOUSE), value(&after, HOUSE));
            assert_eq!(value(&before, MORTGAGE), value(&after, MORTGAGE));
            assert_eq!(before.total_minor, after.total_minor);
            assert_eq!(before.as_of, after.as_of);
        }

        /// The net-worth series, point for point — including the default (`from` omitted)
        /// window, which is now resolved from two `MIN` aggregates instead of from the loaded
        /// ledger and must land on the same first sample.
        #[test]
        fn a_net_worth_series_is_identical_windowed_and_unwindowed() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            for q in [
                NetWorthQuery {
                    from: Some("2026-01-01".to_string()),
                    to: Some("2026-03-31".to_string()),
                    ..Default::default()
                },
                // Defaulted window: earliest of the two tables (2020-01-01) through today.
                NetWorthQuery::default(),
                // Weekly sampling, so several points land between the ledger's rows.
                NetWorthQuery {
                    from: Some("2026-01-01".to_string()),
                    to: Some("2026-02-15".to_string()),
                    interval: Some(Interval::Week),
                    ..Default::default()
                },
            ] {
                let before = rt
                    .block_on(service(Mode::Everything).net_worth(&q))
                    .unwrap();
                let after = rt.block_on(service(Mode::Windowed).net_worth(&q)).unwrap();
                let points = |s: &NetWorthSeries| {
                    s.points
                        .iter()
                        .map(|p| {
                            (
                                p.as_of.clone(),
                                p.net_worth_minor,
                                p.assets_minor,
                                p.liabilities_minor,
                            )
                        })
                        .collect::<Vec<_>>()
                };
                assert_eq!(points(&before), points(&after), "query: {q:?}");
                assert!(!after.points.is_empty());
            }
        }

        /// The equity position is a *subtraction*, so a dropped term reads as a house owned
        /// outright — the one report where a wrong seed would be least visible.
        #[test]
        fn an_equity_position_is_identical_windowed_and_unwindowed() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let q = window("2026-01-01", "2026-02-01");

            let before = rt
                .block_on(service(Mode::Everything).equity_position(HOUSE, &q))
                .unwrap();
            let after = rt
                .block_on(service(Mode::Windowed).equity_position(HOUSE, &q))
                .unwrap();

            assert_eq!(after.value_minor, 770_000_00);
            assert_eq!(after.total_debt_minor, 484_686_19);
            assert_eq!(after.equity_minor, 285_313_81);
            assert_eq!(before.value_minor, after.value_minor);
            assert_eq!(before.total_debt_minor, after.total_debt_minor);
            assert_eq!(before.equity_minor, after.equity_minor);
            assert_eq!(before.paid_off_pct, after.paid_off_pct);
        }

        /// The spend reports: the SQL window is a superset of the filter that always ran here,
        /// so the totals are unchanged — and the rows either side of the window (2019, June)
        /// stay out of them.
        #[test]
        fn the_spend_reports_are_identical_windowed_and_unwindowed() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let q = window("2026-01-01", "2026-02-28");

            let before = rt
                .block_on(service(Mode::Everything).category_breakdown(&q))
                .unwrap();
            let after = rt
                .block_on(service(Mode::Windowed).category_breakdown(&q))
                .unwrap();
            let totals = |b: &CategoryBreakdown| {
                (
                    b.income
                        .iter()
                        .map(|t| (t.category_id, t.total_minor))
                        .collect::<Vec<_>>(),
                    b.expense
                        .iter()
                        .map(|t| (t.category_id, t.total_minor))
                        .collect::<Vec<_>>(),
                )
            };
            assert_eq!(totals(&before), totals(&after));
            assert_eq!(
                totals(&after),
                (vec![(Some(10), 5_000_00)], vec![(Some(20), 1_500_00)]),
                "only the three in-window rows count: $5,000 in, $1,200 + $300 out"
            );

            let before = rt.block_on(service(Mode::Everything).sankey(&q)).unwrap();
            let after = rt.block_on(service(Mode::Windowed).sankey(&q)).unwrap();
            let links = |g: &SankeyGraph| {
                g.links
                    .iter()
                    .map(|l| (l.source.clone(), l.target.clone(), l.value_minor))
                    .collect::<Vec<_>>()
            };
            assert_eq!(links(&before), links(&after));
        }
    }
}
