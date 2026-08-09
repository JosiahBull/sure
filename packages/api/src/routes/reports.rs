//! Report HTTP handlers. All the heavy aggregation (running balances, currency
//! normalisation, category roll-ups, flow graphs) lives in `sure_app::reports`; these
//! handlers extract query params, forward to it, and convert the plain result into the
//! wire-facing (`ToSchema`) response — the shapes here are computed/flattened enough
//! that a DTO twin is worth it even in this early, no-trait-inversion phase (see
//! `docs/architecture-refactor.md`).

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde::{Deserialize, Serialize};
use sure_core::{AccountClass, AccountKind, Ownership};
use utoipa::{IntoParams, ToSchema};

use crate::compute;
use crate::error::{AppError, AppResult};
use crate::extract::Json;
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
    /// Report currency; defaults to the configured base currency. A code that isn't in the
    /// `currencies` table is a 400, not a report denominated in a currency that doesn't exist
    /// with every account listed as `unconverted` — see `sure_app::reports`'s
    /// `currency_and_fx`.
    pub currency: Option<String>,
    /// Whose spending to report: `joint`, or a household member's id. Omitted reports the
    /// whole household.
    pub attributed_to: Option<String>,
}

impl TryFrom<&ReportQuery> for sure_app::reports::ReportQuery {
    type Error = AppError;

    fn try_from(q: &ReportQuery) -> Result<Self, Self::Error> {
        Ok(sure_app::reports::ReportQuery {
            attributed_to: parse_attribution(q.attributed_to.as_deref())?,
            from: q.from.clone(),
            to: q.to.clone(),
            include_one_off: q.include_one_off,
            currency: q.currency.clone(),
        })
    }
}

/// Parse the `attributed_to` query param into the domain enum. Same rule as everywhere
/// else this value crosses the wire: unrecognised is a 400, never a filter that quietly
/// widens to the whole household.
fn parse_attribution(raw: Option<&str>) -> AppResult<Option<Ownership>> {
    raw.map(str::parse::<Ownership>)
        .transpose()
        .map_err(AppError::bad_request)
}

#[derive(Debug, Deserialize, IntoParams, Default)]
#[into_params(parameter_in = Query)]
pub struct NetWorthQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    /// Sampling interval: `month` (default), `week`, or `day`. An unrecognised value is a
    /// 400, not a silent fall back to `month`.
    pub interval: Option<String>,
    /// Report currency; defaults to the configured base currency. An unknown code is a 400,
    /// on the same rule as `interval` above.
    pub currency: Option<String>,
    /// Whose accounts to include: `joint`, or a household member's id. Omitted is the whole
    /// household. Filters accounts, not transactions — see the domain type's doc comment.
    pub attributed_to: Option<String>,
}

impl TryFrom<&NetWorthQuery> for sure_app::reports::NetWorthQuery {
    type Error = AppError;

    /// The HTTP edge `interval` is parsed into the domain enum right here: a query string
    /// is the "one legal place a domain value is text" (CLAUDE.md rule 1), and an
    /// unparseable value is rejected outright rather than defaulting.
    fn try_from(q: &NetWorthQuery) -> Result<Self, Self::Error> {
        let interval = q
            .interval
            .as_deref()
            .map(str::parse::<sure_core::Interval>)
            .transpose()
            .map_err(|e: String| AppError::bad_request(e))?;
        Ok(sure_app::reports::NetWorthQuery {
            attributed_to: parse_attribution(q.attributed_to.as_deref())?,
            from: q.from.clone(),
            to: q.to.clone(),
            interval,
            currency: q.currency.clone(),
        })
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
    /// Currency codes whose accounts are **missing** from every point, because no exchange
    /// rate links them to `currency`. They are excluded rather than counted at parity — an
    /// unconverted foreign balance is a wrong number, not a missing one — so a non-empty list
    /// means this series describes part of the household's net worth. Render it.
    pub unconverted: Vec<String>,
    /// Newest date across the exchange rates used (ISO-8601), `null` if none are on record.
    /// The poller only writes on success, so a stale date is the only sign that a feed has
    /// been down and these conversions are running on old rates.
    pub rates_as_of: Option<String>,
}

impl From<sure_app::reports::NetWorthSeries> for NetWorthSeries {
    fn from(s: sure_app::reports::NetWorthSeries) -> Self {
        NetWorthSeries {
            currency: s.currency,
            points: s.points.into_iter().map(Into::into).collect(),
            unconverted: s.unconverted,
            rates_as_of: s.rates_as_of,
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
    /// `center`, `savings`, or `in:<category_id>` / `out:<category_id>` at any level of
    /// the hierarchy (`0` being the uncategorised bucket). Treat it as an opaque key and
    /// read the fields below rather than parsing it.
    pub id: String,
    pub label: String,
    /// `income` | `center` | `expense` | `savings`.
    pub kind: String,
    /// The category this node stands for; null for the hub, savings and uncategorised.
    pub category_id: Option<i64>,
    /// 0-based level within its own side (0 = top-level, adjacent to the hub); null for
    /// the hub and savings.
    pub depth: Option<u8>,
    /// Top-level ancestor, for colouring a whole branch from one key.
    pub root_id: Option<i64>,
    /// That top-level ancestor's own colour, if set — the branch's base shade.
    pub root_color: Option<String>,
}

impl From<sure_app::reports::SankeyNode> for SankeyNode {
    fn from(n: sure_app::reports::SankeyNode) -> Self {
        SankeyNode {
            id: n.id,
            label: n.label,
            kind: n.kind.as_str().to_string(),
            category_id: n.category_id,
            depth: n.depth,
            root_id: n.root_id,
            root_color: n.root_color,
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
    pub kind: AccountKind,
    pub class: AccountClass,
    pub currency_code: String,
    pub value_minor: i64,
    pub ownership: Ownership,
    /// Listed, but outside `BalancesReport::total_minor` — the household has said this
    /// account is not part of what it is worth.
    pub excluded_from_net_worth: bool,
}

impl From<sure_app::reports::AccountBalance> for AccountBalance {
    fn from(a: sure_app::reports::AccountBalance) -> Self {
        AccountBalance {
            ownership: a.ownership,
            account_id: a.account_id,
            name: a.name,
            kind: a.kind,
            class: a.class,
            currency_code: a.currency_code,
            value_minor: a.value_minor,
            excluded_from_net_worth: a.excluded_from_net_worth,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BalancesReport {
    pub currency: String,
    pub as_of: String,
    /// Total of the accounts that could be converted into `currency`. An account whose
    /// currency is in `unconverted` is still in `accounts` (its own-currency balance is
    /// true) but is **not** inside this figure.
    pub total_minor: i64,
    pub accounts: Vec<AccountBalance>,
    /// Currency codes excluded from `total_minor` for want of an exchange rate.
    pub unconverted: Vec<String>,
    /// Newest date across the exchange rates used (ISO-8601), `null` if none are on record.
    pub rates_as_of: Option<String>,
}

impl From<sure_app::reports::BalancesReport> for BalancesReport {
    fn from(r: sure_app::reports::BalancesReport) -> Self {
        BalancesReport {
            currency: r.currency,
            as_of: r.as_of,
            total_minor: r.total_minor,
            accounts: r.accounts.into_iter().map(Into::into).collect(),
            unconverted: r.unconverted,
            rates_as_of: r.rates_as_of,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SecuredLiability {
    pub account_id: i64,
    pub name: String,
    pub kind: AccountKind,
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

// Kept out of the doc comments below (utoipa publishes those verbatim as the endpoint
// description, and an internal scheduling detail tells an API consumer nothing):
//
// Three of the five handlers here run their aggregation on the blocking pool rather than on the
// runtime worker that accepted the request, under `crate::compute`'s process-wide slot. Both
// halves of the reason are in `sure_app::reports::NetWorthInputs`: there is no `.await` inside a
// report aggregation, so (a) `crate::cache::timeout`'s deadline could not fire until the work had
// already finished — a completed response thrown away and a 408 returned for CPU already spent —
// and (b) the worker was held throughout, so on a four-worker box four concurrent report requests
// meant no connections accepted, `/api/health` silent, no scheduler tick and no shutdown watcher,
// with no external failure required (one SPA dashboard load fans out several of these).
//
// `balances` and `equity_position` stay inline: their arithmetic is one `account_value_at` call
// per account on a *single* date, bounded by how many accounts a household has rather than by the
// ledger or a date range. See those two service methods' doc comments.

/// Net worth over time, sampled at the requested interval.
#[utoipa::path(get, path = "/api/reports/net-worth", tag = "reports", params(NetWorthQuery),
    responses((status = 200, body = NetWorthSeries),
        (status = 400, description = "unrecognised `interval` or unknown `currency`",
            body = crate::error::ErrorBody),
        // Declared, because the refusal arrives in the standard `{ error: { code, message } }`
        // envelope with code `overloaded` like every other "busy" answer — a client that
        // expected an empty 503 body would fail to read it.
        (status = 503, description = "every compute slot is busy; retry after `Retry-After`",
            body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = REPORTS_NET_WORTH,
    level = "debug",
    skip_all,
    fields(query = ?q),
    // No `ret`: the response is now a `Response`, which logs as opaque bytes rather than the
    // series a reader would want. `err` still carries the interesting case.
    err(level = tracing::Level::WARN),
)]
pub async fn net_worth(
    State(st): State<AppState>,
    Query(q): Query<NetWorthQuery>,
) -> AppResult<Response> {
    let query: sure_app::reports::NetWorthQuery = (&q).try_into()?;
    let inputs = st.reports.net_worth_inputs(&query).await?;

    // Acquired *after* the loads and released when the handler returns, so a slot is only held
    // while a core is actually being used. Shed rather than queued: a client waiting behind a
    // pile of full-ledger walks has given up long before its turn arrives.
    let Some(_slot) = compute::try_slot() else {
        return Ok(compute::shed(REPORTS_NET_WORTH));
    };

    // One `?`, not two: `net_worth_from` is infallible, so the only failure is the join itself.
    // A `JoinError` means the closure panicked — mapped to the same scrubbed 500
    // `CatchPanicLayer` produces for an inline panic, never unwrapped (that would re-panic here,
    // on the runtime worker awaiting the join). See `crate::compute::joined`.
    let series = st
        .shutdown
        .spawn_blocking(move || sure_app::reports::ReportService::net_worth_from(inputs))
        .await
        .map_err(|e| compute::joined(e, REPORTS_NET_WORTH))?;

    Ok(Json(NetWorthSeries::from(series)).into_response())
}

/// Income/expense totals per top-level category for the period.
#[utoipa::path(get, path = "/api/reports/category-breakdown", tag = "reports", params(ReportQuery),
    responses((status = 200, body = CategoryBreakdown),
        (status = 400, description = "unknown `currency` or `attributed_to`",
            body = crate::error::ErrorBody),
        // Same `overloaded` envelope as every other "busy" answer — see `net_worth` above.
        (status = 503, description = "every compute slot is busy; retry after `Retry-After`",
            body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = REPORTS_CATEGORY_BREAKDOWN,
    level = "debug",
    skip_all,
    fields(query = ?q),
    // No `ret`: an opaque `Response` now, as in `net_worth`.
    err(level = tracing::Level::WARN),
)]
pub async fn category_breakdown(
    State(st): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Response> {
    let inputs = st
        .reports
        .category_breakdown_inputs(&(&q).try_into()?)
        .await?;

    let Some(_slot) = compute::try_slot() else {
        return Ok(compute::shed(REPORTS_CATEGORY_BREAKDOWN));
    };

    let breakdown = st
        .shutdown
        .spawn_blocking(move || sure_app::reports::ReportService::category_breakdown_from(inputs))
        .await
        .map_err(|e| compute::joined(e, REPORTS_CATEGORY_BREAKDOWN))?;

    Ok(Json(CategoryBreakdown::from(breakdown)).into_response())
}

/// Money-flow graph: income categories -> cash flow -> expense categories (+ savings).
#[utoipa::path(get, path = "/api/reports/sankey", tag = "reports", params(ReportQuery),
    responses((status = 200, body = SankeyGraph),
        (status = 400, description = "unknown `currency` or `attributed_to`",
            body = crate::error::ErrorBody),
        // Same `overloaded` envelope as every other "busy" answer — see `net_worth` above.
        (status = 503, description = "every compute slot is busy; retry after `Retry-After`",
            body = crate::error::ErrorBody)))]
#[tracing::instrument(
    name = REPORTS_SANKEY,
    level = "debug",
    skip_all,
    fields(query = ?q),
    // No `ret`: an opaque `Response` now, as in `net_worth`.
    err(level = tracing::Level::WARN),
)]
pub async fn sankey(
    State(st): State<AppState>,
    Query(q): Query<ReportQuery>,
) -> AppResult<Response> {
    let inputs = st.reports.sankey_inputs(&(&q).try_into()?).await?;

    let Some(_slot) = compute::try_slot() else {
        return Ok(compute::shed(REPORTS_SANKEY));
    };

    let graph = st
        .shutdown
        .spawn_blocking(move || sure_app::reports::ReportService::sankey_from(inputs))
        .await
        .map_err(|e| compute::joined(e, REPORTS_SANKEY))?;

    Ok(Json(SankeyGraph::from(graph)).into_response())
}

/// Current value of each (non-archived) account plus a base-currency total.
#[utoipa::path(get, path = "/api/reports/balances", tag = "reports", params(ReportQuery),
    responses((status = 200, body = BalancesReport),
        (status = 400, description = "unknown `currency` or `attributed_to`",
            body = crate::error::ErrorBody)))]
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
    Ok(Json(st.reports.balances(&(&q).try_into()?).await?.into()))
}

/// The equity position of an asset: its value, the liabilities secured against it,
/// total debt, equity, and the paid-off percentage.
///
/// 422 when the asset or one of its secured debts has no exchange rate to the report
/// currency: equity is a subtraction, so a silently dropped debt reads as an asset owned
/// outright. There is no partial answer worth returning here.
#[utoipa::path(get, path = "/api/accounts/{id}/equity-position", tag = "reports",
    params(("id" = i64, Path,), ReportQuery),
    responses((status = 200, body = EquityPosition), (status = 400, body = crate::error::ErrorBody),
        (status = 404, body = crate::error::ErrorBody),
        (status = 422, body = crate::error::ErrorBody)))]
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
        st.reports
            .equity_position(id, &(&q).try_into()?)
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
