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

use sure_core::{AccountClass, AccountKind, AppResult, CategoryKind, Interval, Ownership};

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

pub(crate) fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
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
                    let after_date: i64 = tx_by_acct
                        .get(&id)
                        .map(|txs| {
                            txs.iter()
                                .filter(|(d, _)| *d > date && *d <= *anchor_date)
                                .map(|(_, a)| a)
                                .sum()
                        })
                        .unwrap_or(0);
                    (anchor_value - after_date, ccy.clone())
                }
                // Before the account's first transaction (or it has none) → not yet opened.
                _ => (0, ccy.clone()),
            };
        }
    }
    // Case 3.
    let balance = tx_by_acct
        .get(&id)
        .map(|txs| txs.iter().filter(|(d, _)| *d <= date).map(|(_, a)| a).sum())
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

pub(crate) async fn load_ledger(reports: &dyn ReportRepo) -> AppResult<Ledger> {
    let txns = reports.transactions().await?;
    let vals = reports.valuations().await?;
    let mut tx_by_acct: HashMap<i64, Vec<(NaiveDate, i64)>> = HashMap::new();
    for t in &txns {
        if let Some(d) = parse_date(&t.posted_at) {
            tx_by_acct
                .entry(t.account_id)
                .or_default()
                .push((d, t.amount_minor));
        }
    }
    let mut val_by_acct: HashMap<i64, Vec<(NaiveDate, i64, String)>> = HashMap::new();
    for v in &vals {
        if let Some(d) = parse_date(&v.as_of) {
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
    let rows = reports.spend_transactions().await?;
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
            match parse_date(&t.posted_at) {
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
        let base = self.base_currency(q.currency.as_deref()).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;

        let mut accounts = self.reports.account_currencies().await?;
        if let Some(owner) = q.attributed_to {
            accounts.retain(|a| a.ownership == owner);
        }
        let txns = self.reports.transactions().await?;
        let vals = self.reports.valuations().await?;

        // Index transactions/valuations per account, with parsed dates.
        let mut tx_by_acct: HashMap<i64, Vec<(NaiveDate, i64)>> = HashMap::new();
        for t in &txns {
            if let Some(d) = parse_date(&t.posted_at) {
                tx_by_acct
                    .entry(t.account_id)
                    .or_default()
                    .push((d, t.amount_minor));
            }
        }
        let mut val_by_acct: HashMap<i64, Vec<(NaiveDate, i64, String)>> = HashMap::new();
        for v in &vals {
            if let Some(d) = parse_date(&v.as_of) {
                val_by_acct.entry(v.account_id).or_default().push((
                    d,
                    v.value_minor,
                    v.currency_code.clone(),
                ));
            }
        }

        // Resolve the reporting window.
        let today = self.clock.today();
        let to = q.to.as_deref().and_then(parse_date).unwrap_or(today);
        let earliest = tx_by_acct
            .values()
            .flatten()
            .map(|(d, _)| *d)
            .chain(val_by_acct.values().flatten().map(|(d, _, _)| *d))
            .min();
        let from = q
            .from
            .as_deref()
            .and_then(parse_date)
            .or(earliest)
            .unwrap_or_else(|| to - chrono::Duration::days(365));

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
        let base = self.base_currency(q.currency.as_deref()).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;
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
        let base = self.base_currency(q.currency.as_deref()).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;
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
            flow_order(&forest.roots, forest, &cats, &fx)
                .into_iter()
                .map(|root| emit_flow_node(root, CENTER, side, forest, &cats, &fx, nodes, links))
                .sum()
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
                value_minor: income_minor - expense_minor,
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
        let base = self.base_currency(q.currency.as_deref()).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;
        let as_of =
            q.to.as_deref()
                .and_then(parse_date)
                .unwrap_or_else(|| self.clock.today());

        let accounts = self.reports.active_accounts().await?;
        let (tx_by_acct, val_by_acct) = load_ledger(self.reports.as_ref()).await?;

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
        let base = self.base_currency(q.currency.as_deref()).await?;
        let fx = Fx::load(self.fx.as_ref(), base.clone()).await?;
        let as_of =
            q.to.as_deref()
                .and_then(parse_date)
                .unwrap_or_else(|| self.clock.today());

        let asset = self.reports.account(id).await?;
        let liabs = self.reports.secured_liabilities(id).await?;

        let (tx_by_acct, val_by_acct) = load_ledger(self.reports.as_ref()).await?;

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
}
