//! Backend-computed report data. All heavy aggregation (running balances, currency
//! normalisation, category roll-ups, flow graphs) happens here so the SPA only ever
//! renders ready-made numbers. Row loading is delegated to `sure_dal::reports`; this
//! module never touches `sqlx` directly.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::{Json, Router};
use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::error::AppResult;
use crate::state::AppState;
use sure_dal::reports::SpendTransaction;

// ---- query params --------------------------------------------------------

// OTEL span names for this module's handlers.
const REPORTS_NET_WORTH: &str = "reports.net_worth";
const REPORTS_CATEGORY_BREAKDOWN: &str = "reports.category_breakdown";
const REPORTS_SANKEY: &str = "reports.sankey";
const REPORTS_BALANCES: &str = "reports.balances";
const REPORTS_EQUITY_POSITION: &str = "reports.equity_position";

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct ReportQuery {
    /// Inclusive start date (ISO-8601). Defaults to the earliest data.
    pub from: Option<String>,
    /// Inclusive end date (ISO-8601). Defaults to today.
    pub to: Option<String>,
    /// Include one-off transactions (default false).
    pub include_one_off: Option<bool>,
    /// Report currency; defaults to the configured base currency.
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct NetWorthQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    /// Sampling interval: `month` (default), `week`, or `day`.
    pub interval: Option<String>,
    pub currency: Option<String>,
}

// ---- response shapes -----------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct NetWorthPoint {
    pub as_of: String,
    pub net_worth_minor: i64,
    pub assets_minor: i64,
    pub liabilities_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NetWorthSeries {
    pub currency: String,
    pub points: Vec<NetWorthPoint>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryTotal {
    /// `null` for the uncategorised bucket.
    pub category_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub total_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryBreakdown {
    pub currency: String,
    pub from: String,
    pub to: String,
    pub income: Vec<CategoryTotal>,
    pub expense: Vec<CategoryTotal>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SankeyNode {
    pub id: String,
    pub label: String,
    /// `income` | `center` | `expense` | `savings`.
    pub kind: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SankeyLink {
    pub source: String,
    pub target: String,
    pub value_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SankeyGraph {
    pub currency: String,
    pub nodes: Vec<SankeyNode>,
    pub links: Vec<SankeyLink>,
}

// ---- currency conversion -------------------------------------------------

use crate::fx::Fx;

#[tracing::instrument(level = "debug", skip_all)]
async fn base_currency(db: &sure_dal::Db, override_: Option<&str>) -> AppResult<String> {
    if let Some(c) = override_.filter(|s| !s.is_empty()) {
        return Ok(c.to_uppercase());
    }
    sure_dal::settings::base_currency(db).await
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(0..10).unwrap_or(s), "%Y-%m-%d").ok()
}

fn last_day_of_month(y: i32, m: u32) -> NaiveDate {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
}

// ---- net worth -----------------------------------------------------------

/// Net worth over time, sampled at the requested interval.
#[utoipa::path(get, path = "/api/reports/net-worth", tag = "reports", params(NetWorthQuery),
    responses((status = 200, body = NetWorthSeries)))]
#[tracing::instrument(
    name = REPORTS_NET_WORTH,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn net_worth(
    State(st): State<AppState>,
    Query(q): Query<NetWorthQuery>,
) -> AppResult<Json<NetWorthSeries>> {
    let base = base_currency(&st.db, q.currency.as_deref()).await?;
    let fx = Fx::load(&st.db, base.clone()).await?;

    let accounts = sure_dal::reports::account_currencies(&st.db).await?;
    let txns = sure_dal::reports::transactions(&st.db).await?;
    let vals = sure_dal::reports::valuations(&st.db).await?;

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
    let today = Utc::now().date_naive();
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

    let sample_dates = sample_dates(from, to, q.interval.as_deref().unwrap_or("month"));

    let mut points = Vec::with_capacity(sample_dates.len());
    for date in sample_dates {
        let mut assets = 0.0f64;
        let mut liabilities = 0.0f64;
        for a in &accounts {
            let (value_minor, ccy) =
                account_value_at(a.id, &a.currency_code, date, &tx_by_acct, &val_by_acct);
            let base_major = fx.to_base_major(value_minor, &ccy);
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

    Ok(Json(NetWorthSeries {
        currency: base,
        points,
    }))
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
fn account_value_at(
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

fn sample_dates(from: NaiveDate, to: NaiveDate, interval: &str) -> Vec<NaiveDate> {
    if to < from {
        return vec![to];
    }
    let mut out = Vec::new();
    match interval {
        "day" | "week" => {
            let step = if interval == "week" { 7 } else { 1 };
            let mut d = from;
            while d < to && out.len() < 400 {
                out.push(d);
                d += chrono::Duration::days(step);
            }
        }
        _ => {
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

// ---- spend loading (shared by pie + sankey) ------------------------------

struct Categories {
    parents: HashMap<i64, Option<i64>>,
    names: HashMap<i64, String>,
    colors: HashMap<i64, Option<String>>,
    kinds: HashMap<i64, String>,
}

impl Categories {
    async fn load(db: &sure_dal::Db) -> AppResult<Self> {
        let cats = sure_dal::reports::categories(db).await?;
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

    fn top_ancestor(&self, id: i64) -> i64 {
        let mut cur = id;
        for _ in 0..64 {
            match self.parents.get(&cur) {
                Some(Some(p)) => cur = *p,
                _ => break,
            }
        }
        cur
    }

    fn is_transfer(&self, id: i64) -> bool {
        self.kinds
            .get(&id)
            .map(|k| k == "transfer")
            .unwrap_or(false)
    }
}

/// Load transactions in the window, excluding transfers (either linked, or in a
/// transfer-kind category) and — optionally — one-offs.
#[tracing::instrument(level = "debug", skip_all)]
async fn load_spend(
    db: &sure_dal::Db,
    cats: &Categories,
    from: NaiveDate,
    to: NaiveDate,
    include_one_off: bool,
) -> AppResult<Vec<SpendTransaction>> {
    let rows = sure_dal::reports::spend_transactions(db).await?;
    Ok(rows
        .into_iter()
        .filter(|t| {
            // Linked transactions are the two legs of a transfer — internal movement.
            if t.linked_transaction_id.is_some() {
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

/// Resolve the report window with data-driven defaults.
fn window(from: Option<&str>, to: Option<&str>) -> (NaiveDate, NaiveDate) {
    let today = Utc::now().date_naive();
    let to = to.and_then(parse_date).unwrap_or(today);
    let from = from.and_then(parse_date).unwrap_or_else(|| {
        // default to the start of the month 12 months back
        let d = to - chrono::Duration::days(365);
        NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap_or(d)
    });
    (from, to)
}

// ---- category breakdown (pie) --------------------------------------------

/// Income/expense totals per top-level category for the period.
#[utoipa::path(get, path = "/api/reports/category-breakdown", tag = "reports", params(ReportQuery),
    responses((status = 200, body = CategoryBreakdown)))]
#[tracing::instrument(
    name = REPORTS_CATEGORY_BREAKDOWN,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn category_breakdown(
    State(st): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Json<CategoryBreakdown>> {
    let base = base_currency(&st.db, q.currency.as_deref()).await?;
    let fx = Fx::load(&st.db, base.clone()).await?;
    let cats = Categories::load(&st.db).await?;
    let (from, to) = window(q.from.as_deref(), q.to.as_deref());
    let spend = load_spend(&st.db, &cats, from, to, q.include_one_off.unwrap_or(false)).await?;

    // key 0 => uncategorised.
    let mut income: HashMap<i64, f64> = HashMap::new();
    let mut expense: HashMap<i64, f64> = HashMap::new();
    for t in &spend {
        let key = t.category_id.map(|c| cats.top_ancestor(c)).unwrap_or(0);
        let base_major = fx.to_base_major(t.amount_minor.abs(), &t.currency_code);
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

    Ok(Json(CategoryBreakdown {
        currency: base,
        from: from.to_string(),
        to: to.to_string(),
        income: to_totals(income),
        expense: to_totals(expense),
    }))
}

// ---- sankey --------------------------------------------------------------

/// Money-flow graph: income categories -> cash flow -> expense categories (+ savings).
#[utoipa::path(get, path = "/api/reports/sankey", tag = "reports", params(ReportQuery),
    responses((status = 200, body = SankeyGraph)))]
#[tracing::instrument(
    name = REPORTS_SANKEY,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn sankey(
    State(st): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Json<SankeyGraph>> {
    let base = base_currency(&st.db, q.currency.as_deref()).await?;
    let fx = Fx::load(&st.db, base.clone()).await?;
    let cats = Categories::load(&st.db).await?;
    let (from, to) = window(q.from.as_deref(), q.to.as_deref());
    let spend = load_spend(&st.db, &cats, from, to, q.include_one_off.unwrap_or(false)).await?;

    let mut income: HashMap<i64, f64> = HashMap::new();
    let mut expense: HashMap<i64, f64> = HashMap::new();
    for t in &spend {
        let key = t.category_id.map(|c| cats.top_ancestor(c)).unwrap_or(0);
        let base_major = fx.to_base_major(t.amount_minor.abs(), &t.currency_code);
        if t.amount_minor >= 0 {
            *income.entry(key).or_default() += base_major;
        } else {
            *expense.entry(key).or_default() += base_major;
        }
    }

    let label = |key: i64| -> String {
        if key == 0 {
            "Uncategorised".to_string()
        } else {
            cats.names.get(&key).cloned().unwrap_or_else(|| "?".into())
        }
    };

    let mut nodes = vec![SankeyNode {
        id: "center".into(),
        label: "Cash flow".into(),
        kind: "center".into(),
    }];
    let mut links = Vec::new();

    let mut total_income = 0.0;
    for (key, total) in &income {
        total_income += *total;
        let id = format!("in:{key}");
        nodes.push(SankeyNode {
            id: id.clone(),
            label: label(*key),
            kind: "income".into(),
        });
        links.push(SankeyLink {
            source: id,
            target: "center".into(),
            value_minor: fx.base_minor(*total),
        });
    }

    let mut total_expense = 0.0;
    for (key, total) in &expense {
        total_expense += *total;
        let id = format!("out:{key}");
        nodes.push(SankeyNode {
            id: id.clone(),
            label: label(*key),
            kind: "expense".into(),
        });
        links.push(SankeyLink {
            source: "center".into(),
            target: id,
            value_minor: fx.base_minor(*total),
        });
    }

    // Surplus flows to savings.
    if total_income > total_expense {
        nodes.push(SankeyNode {
            id: "savings".into(),
            label: "Savings".into(),
            kind: "savings".into(),
        });
        links.push(SankeyLink {
            source: "center".into(),
            target: "savings".into(),
            value_minor: fx.base_minor(total_income - total_expense),
        });
    }

    Ok(Json(SankeyGraph {
        currency: base,
        nodes,
        links,
    }))
}

// ---- account balances ----------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AccountBalance {
    pub account_id: i64,
    pub name: String,
    pub kind: String,
    /// cash | asset | investment | liability
    pub class: String,
    pub currency_code: String,
    pub value_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BalancesReport {
    pub currency: String,
    pub as_of: String,
    pub total_minor: i64,
    pub accounts: Vec<AccountBalance>,
}

fn class_of(kind: &str) -> &'static str {
    match kind {
        "cash" | "bank" | "savings" => "cash",
        "credit_card" | "revolving_credit" | "mortgage" | "student_loan" | "loan" | "liability" => {
            "liability"
        }
        "shares_nz" | "shares_us" | "shares_private" => "investment",
        _ => "asset",
    }
}

/// Current value of each (non-archived) account plus a base-currency total.
#[utoipa::path(get, path = "/api/reports/balances", tag = "reports", params(ReportQuery),
    responses((status = 200, body = BalancesReport)))]
#[tracing::instrument(
    name = REPORTS_BALANCES,
    level = "debug",
    skip_all,
    fields(query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn balances(
    State(st): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Json<BalancesReport>> {
    let base = base_currency(&st.db, q.currency.as_deref()).await?;
    let fx = Fx::load(&st.db, base.clone()).await?;
    let as_of =
        q.to.as_deref()
            .and_then(parse_date)
            .unwrap_or_else(|| Utc::now().date_naive());

    let accounts = sure_dal::reports::active_accounts(&st.db).await?;
    let (tx_by_acct, val_by_acct) = load_ledger(&st.db).await?;

    let mut out = Vec::new();
    let mut total = 0.0;
    for a in &accounts {
        let (value_minor, ccy) =
            account_value_at(a.id, &a.currency_code, as_of, &tx_by_acct, &val_by_acct);
        total += fx.to_base_major(value_minor, &ccy);
        out.push(AccountBalance {
            account_id: a.id,
            name: a.name.clone(),
            kind: a.kind.clone(),
            class: class_of(&a.kind).to_string(),
            currency_code: ccy,
            value_minor,
        });
    }

    Ok(Json(BalancesReport {
        currency: base,
        as_of: as_of.to_string(),
        total_minor: fx.base_minor(total),
        accounts: out,
    }))
}

// ---- asset equity position (property paid-off %) -------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct SecuredLiability {
    pub account_id: i64,
    pub name: String,
    pub kind: String,
    /// Amount owed, in the report currency (positive).
    pub balance_minor: i64,
}

#[derive(Debug, Serialize, ToSchema)]
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

type Ledger = (
    HashMap<i64, Vec<(NaiveDate, i64)>>,
    HashMap<i64, Vec<(NaiveDate, i64, String)>>,
);

/// Load transactions + valuations indexed per account, for point-in-time balances.
#[tracing::instrument(level = "debug", skip_all)]
async fn load_ledger(db: &sure_dal::Db) -> AppResult<Ledger> {
    let txns = sure_dal::reports::transactions(db).await?;
    let vals = sure_dal::reports::valuations(db).await?;
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

/// The equity position of an asset: its value, the liabilities secured against it,
/// total debt, equity, and the paid-off percentage.
#[utoipa::path(get, path = "/api/accounts/{id}/equity-position", tag = "reports",
    params(("id" = i64, Path,), ReportQuery),
    responses((status = 200, body = EquityPosition), (status = 404, body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = REPORTS_EQUITY_POSITION,
    level = "debug",
    skip_all,
    fields(account_id = %id, query = ?q),
    ret(level = tracing::Level::DEBUG),
    err(level = tracing::Level::WARN),
)]
pub async fn equity_position(
    State(st): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Json<EquityPosition>> {
    let base = base_currency(&st.db, q.currency.as_deref()).await?;
    let fx = Fx::load(&st.db, base.clone()).await?;
    let as_of =
        q.to.as_deref()
            .and_then(parse_date)
            .unwrap_or_else(|| Utc::now().date_naive());

    let asset = sure_dal::reports::account(&st.db, id).await?;
    let liabs = sure_dal::reports::secured_liabilities(&st.db, id).await?;

    let (tx_by_acct, val_by_acct) = load_ledger(&st.db).await?;

    let (v_minor, v_ccy) = account_value_at(
        asset.id,
        &asset.currency_code,
        as_of,
        &tx_by_acct,
        &val_by_acct,
    );
    let value_base = fx.to_base_major(v_minor, &v_ccy).max(0.0);

    let mut total_debt = 0.0;
    let mut liabilities = Vec::new();
    for l in &liabs {
        let (lm, lccy) = account_value_at(l.id, &l.currency_code, as_of, &tx_by_acct, &val_by_acct);
        // Liabilities carry a negative balance; the debt is its magnitude.
        let debt = fx.to_base_major(lm, &lccy).min(0.0).abs();
        total_debt += debt;
        liabilities.push(SecuredLiability {
            account_id: l.id,
            name: l.name.clone(),
            kind: l.kind.clone(),
            balance_minor: fx.base_minor(debt),
        });
    }

    let equity = value_base - total_debt;
    let paid_off_pct = if value_base > 0.0 {
        ((equity / value_base) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    Ok(Json(EquityPosition {
        account_id: asset.id,
        name: asset.name,
        currency: base,
        as_of: as_of.to_string(),
        value_minor: fx.base_minor(value_base),
        total_debt_minor: fx.base_minor(total_debt),
        equity_minor: fx.base_minor(equity),
        paid_off_pct,
        liabilities,
    }))
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route("/reports/net-worth", get(net_worth))
        .route("/reports/category-breakdown", get(category_breakdown))
        .route("/reports/sankey", get(sankey))
        .route("/reports/balances", get(balances))
        .route("/accounts/{id}/equity-position", get(equity_position))
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
}
