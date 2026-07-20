//! Report HTTP handlers. All the heavy aggregation (running balances, currency
//! normalisation, category roll-ups, flow graphs) lives in `sure_app::reports`; these
//! handlers extract query params, forward to it, and convert the plain result into the
//! wire-facing (`ToSchema`) response — the shapes here are computed/flattened enough
//! that a DTO twin is worth it even in this early, no-trait-inversion phase (see
//! `docs/architecture-refactor.md`).

use axum::extract::{Path, Query, State};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::error::AppResult;
use crate::state::AppState;

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

impl From<&ReportQuery> for sure_app::reports::ReportQuery {
    fn from(q: &ReportQuery) -> Self {
        sure_app::reports::ReportQuery {
            from: q.from.clone(),
            to: q.to.clone(),
            include_one_off: q.include_one_off,
            currency: q.currency.clone(),
        }
    }
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

impl From<&NetWorthQuery> for sure_app::reports::NetWorthQuery {
    fn from(q: &NetWorthQuery) -> Self {
        sure_app::reports::NetWorthQuery {
            from: q.from.clone(),
            to: q.to.clone(),
            interval: q.interval.clone(),
            currency: q.currency.clone(),
        }
    }
}

// ---- response shapes -----------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct NetWorthPoint {
    pub as_of: String,
    pub net_worth_minor: i64,
    pub assets_minor: i64,
    pub liabilities_minor: i64,
}

impl From<sure_app::reports::NetWorthPoint> for NetWorthPoint {
    fn from(p: sure_app::reports::NetWorthPoint) -> Self {
        NetWorthPoint {
            as_of: p.as_of,
            net_worth_minor: p.net_worth_minor,
            assets_minor: p.assets_minor,
            liabilities_minor: p.liabilities_minor,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct NetWorthSeries {
    pub currency: String,
    pub points: Vec<NetWorthPoint>,
}

impl From<sure_app::reports::NetWorthSeries> for NetWorthSeries {
    fn from(s: sure_app::reports::NetWorthSeries) -> Self {
        NetWorthSeries {
            currency: s.currency,
            points: s.points.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryTotal {
    /// `null` for the uncategorised bucket.
    pub category_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub total_minor: i64,
}

impl From<sure_app::reports::CategoryTotal> for CategoryTotal {
    fn from(t: sure_app::reports::CategoryTotal) -> Self {
        CategoryTotal {
            category_id: t.category_id,
            name: t.name,
            color: t.color,
            total_minor: t.total_minor,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryBreakdown {
    pub currency: String,
    pub from: String,
    pub to: String,
    pub income: Vec<CategoryTotal>,
    pub expense: Vec<CategoryTotal>,
}

impl From<sure_app::reports::CategoryBreakdown> for CategoryBreakdown {
    fn from(b: sure_app::reports::CategoryBreakdown) -> Self {
        CategoryBreakdown {
            currency: b.currency,
            from: b.from,
            to: b.to,
            income: b.income.into_iter().map(Into::into).collect(),
            expense: b.expense.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SankeyNode {
    pub id: String,
    pub label: String,
    /// `income` | `center` | `expense` | `savings`.
    pub kind: String,
}

impl From<sure_app::reports::SankeyNode> for SankeyNode {
    fn from(n: sure_app::reports::SankeyNode) -> Self {
        SankeyNode {
            id: n.id,
            label: n.label,
            kind: n.kind,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SankeyLink {
    pub source: String,
    pub target: String,
    pub value_minor: i64,
}

impl From<sure_app::reports::SankeyLink> for SankeyLink {
    fn from(l: sure_app::reports::SankeyLink) -> Self {
        SankeyLink {
            source: l.source,
            target: l.target,
            value_minor: l.value_minor,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SankeyGraph {
    pub currency: String,
    pub nodes: Vec<SankeyNode>,
    pub links: Vec<SankeyLink>,
}

impl From<sure_app::reports::SankeyGraph> for SankeyGraph {
    fn from(g: sure_app::reports::SankeyGraph) -> Self {
        SankeyGraph {
            currency: g.currency,
            nodes: g.nodes.into_iter().map(Into::into).collect(),
            links: g.links.into_iter().map(Into::into).collect(),
        }
    }
}

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

impl From<sure_app::reports::AccountBalance> for AccountBalance {
    fn from(a: sure_app::reports::AccountBalance) -> Self {
        AccountBalance {
            account_id: a.account_id,
            name: a.name,
            kind: a.kind,
            class: a.class,
            currency_code: a.currency_code,
            value_minor: a.value_minor,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BalancesReport {
    pub currency: String,
    pub as_of: String,
    pub total_minor: i64,
    pub accounts: Vec<AccountBalance>,
}

impl From<sure_app::reports::BalancesReport> for BalancesReport {
    fn from(r: sure_app::reports::BalancesReport) -> Self {
        BalancesReport {
            currency: r.currency,
            as_of: r.as_of,
            total_minor: r.total_minor,
            accounts: r.accounts.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SecuredLiability {
    pub account_id: i64,
    pub name: String,
    pub kind: String,
    /// Amount owed, in the report currency (positive).
    pub balance_minor: i64,
}

impl From<sure_app::reports::SecuredLiability> for SecuredLiability {
    fn from(l: sure_app::reports::SecuredLiability) -> Self {
        SecuredLiability {
            account_id: l.account_id,
            name: l.name,
            kind: l.kind,
            balance_minor: l.balance_minor,
        }
    }
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

impl From<sure_app::reports::EquityPosition> for EquityPosition {
    fn from(p: sure_app::reports::EquityPosition) -> Self {
        EquityPosition {
            account_id: p.account_id,
            name: p.name,
            currency: p.currency,
            as_of: p.as_of,
            value_minor: p.value_minor,
            total_debt_minor: p.total_debt_minor,
            equity_minor: p.equity_minor,
            paid_off_pct: p.paid_off_pct,
            liabilities: p.liabilities.into_iter().map(Into::into).collect(),
        }
    }
}

// ---- handlers -------------------------------------------------------------

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
    Ok(Json(
        sure_app::reports::net_worth(&st.db, &(&q).into())
            .await?
            .into(),
    ))
}

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
    Ok(Json(
        sure_app::reports::category_breakdown(&st.db, &(&q).into())
            .await?
            .into(),
    ))
}

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
    Ok(Json(
        sure_app::reports::sankey(&st.db, &(&q).into())
            .await?
            .into(),
    ))
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
    Ok(Json(
        sure_app::reports::balances(&st.db, &(&q).into())
            .await?
            .into(),
    ))
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
    Path(id): Path<i64>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Json<EquityPosition>> {
    Ok(Json(
        sure_app::reports::equity_position(&st.db, id, &(&q).into())
            .await?
            .into(),
    ))
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
