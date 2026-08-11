//! The aggregates.
//!
//! `summarize_spending` is the most important tool in the server. Without it a model asked
//! "what did I spend on groceries this year" fetches rows and adds them up — slowly,
//! expensively, and wrongly the moment two currencies are involved.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use sure_app::reports::{ReportQuery, SpendGroup};
use sure_core::{GroupBy, Interval, Ownership};

use crate::convert::{Range, money_to_string, resolve_window, table};
use crate::error::{ToolResult, invalid_params, to_mcp};
use crate::server::SureMcp;

/// The axis a summary groups along. A twin of [`sure_core::GroupBy`] so the schema shown to
/// a caller carries these doc comments; parsed into the domain enum immediately below.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupByArg {
    /// The category on each transaction, labelled with its full path (`Food > Groceries`).
    Category,
    /// The merchant, or the raw payee text where a transaction has no merchant record.
    Merchant,
    Account,
    /// Calendar month — use this for "how did X change over time".
    Month,
}

impl From<GroupByArg> for GroupBy {
    fn from(a: GroupByArg) -> Self {
        match a {
            GroupByArg::Category => GroupBy::Category,
            GroupByArg::Merchant => GroupBy::Merchant,
            GroupByArg::Account => GroupBy::Account,
            GroupByArg::Month => GroupBy::Month,
        }
    }
}

/// Which side of the ledger to report.
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Money out. The default: "what did I spend" is the usual question.
    #[default]
    Expense,
    /// Money in.
    Income,
    Both,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SummarizeSpendingParams {
    /// What the buckets are.
    pub group_by: GroupByArg,
    #[serde(default)]
    pub range: Option<Range>,
    /// Inclusive start date, YYYY-MM-DD. Overrides `range` on this side.
    #[serde(default)]
    pub from: Option<String>,
    /// Inclusive end date, YYYY-MM-DD. Overrides `range` on this side.
    #[serde(default)]
    pub to: Option<String>,
    /// Expense (default), income, or both.
    #[serde(default)]
    pub direction: Option<Direction>,
    /// Report currency. Defaults to the household's base currency.
    #[serde(default)]
    pub currency: Option<String>,
    /// Include one-off transactions. Default false — a house purchase in the middle of a
    /// spending summary swamps everything else.
    #[serde(default)]
    pub include_one_off: Option<bool>,
    /// Whose spending: "joint", or a household member's person id.
    #[serde(default)]
    pub attributed_to: Option<String>,
    /// Keep only the largest N buckets. Ignored when grouping by month, where dropping the
    /// small ones would put a hole in the middle of a time series.
    #[serde(default)]
    pub top_n: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct NetWorthParams {
    #[serde(default)]
    pub range: Option<Range>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    /// Sampling granularity: day, week or month (default).
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    /// Restrict to the accounts one household member owns: "joint", or a person id.
    #[serde(default)]
    pub attributed_to: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct MoneyFlowParams {
    #[serde(default)]
    pub range: Option<Range>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub include_one_off: Option<bool>,
    #[serde(default)]
    pub attributed_to: Option<String>,
}

#[tool_router(router = reports_router, vis = "pub")]
impl SureMcp {
    /// Totals along one axis.
    #[tool(
        name = "summarize_spending",
        description = "Total income or expense over a window, grouped by category, merchant, \
                       account or month, normalised to one currency. Use this for any \
                       question ending in a number — how much, which is biggest, how it \
                       changed month to month.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn summarize_spending(
        &self,
        Parameters(params): Parameters<SummarizeSpendingParams>,
    ) -> ToolResult<CallToolResult> {
        let group_by: GroupBy = params.group_by.into();
        let direction = params.direction.unwrap_or_default();
        let query = self.report_query(
            params.range,
            params.from,
            params.to,
            params.currency,
            params.include_one_off,
            params.attributed_to,
        )?;

        let report = self
            .state
            .reports
            .spend_by(&query, group_by)
            .await
            .map_err(to_mcp)?;

        let decimals = self.currency_decimals().await?;
        let scale = Self::scale_of(&decimals, &report.currency);
        // Ranked axes get a top-N; a month axis never does. Truncating a time series drops
        // months out of the middle of it, which reads as "nothing happened then".
        let cap = match group_by {
            GroupBy::Month => None,
            GroupBy::Category | GroupBy::Merchant | GroupBy::Account => {
                Some(params.top_n.unwrap_or(self.config.max_rows).max(1))
            }
        };

        let mut out = String::new();
        let section = |label: &str, groups: &[SpendGroup], out: &mut String| {
            let total: i64 = groups.iter().map(|g| g.total_minor).sum();
            let kept: &[SpendGroup] = match cap {
                Some(n) if groups.len() > n => &groups[..n],
                Some(_) | None => groups,
            };
            let rows: Vec<Vec<String>> = kept
                .iter()
                .map(|g| {
                    vec![
                        g.id.map(|i| i.to_string()).unwrap_or_default(),
                        g.label.clone(),
                        money_to_string(g.total_minor, scale),
                        // The share is what makes a list of numbers a finding. Computed off
                        // the *full* total, not the truncated one, so a top-10 still says
                        // what fraction of everything it accounts for.
                        if total > 0 {
                            format!("{:.1}%", 100.0 * g.total_minor as f64 / total as f64)
                        } else {
                            String::new()
                        },
                    ]
                })
                .collect();
            out.push_str(&format!(
                "\n\n{label} ({} total):\n",
                money_to_string(total, scale)
            ));
            out.push_str(&table(&["id", "group", "amount", "share"], &rows));
            if kept.len() < groups.len() {
                out.push_str(&format!(
                    "\n({} more groups not shown; raise top_n to see them)",
                    groups.len() - kept.len()
                ));
            }
        };

        out.push_str(&format!(
            "{} to {}, in {}, grouped by {}.",
            report.from,
            report.to,
            report.currency,
            group_by.as_str()
        ));
        match direction {
            Direction::Expense => section("Expense", &report.expense, &mut out),
            Direction::Income => section("Income", &report.income, &mut out),
            Direction::Both => {
                section("Income", &report.income, &mut out);
                section("Expense", &report.expense, &mut out);
            }
        }
        out.push_str(&unconverted_note(&report.unconverted, &report.currency));
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// Net worth over time.
    #[tool(
        name = "net_worth",
        description = "Net worth over time, split into assets and liabilities, normalised to \
                       one currency.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn net_worth(
        &self,
        Parameters(params): Parameters<NetWorthParams>,
    ) -> ToolResult<CallToolResult> {
        let today = chrono::Utc::now().date_naive();
        let (from, to) = resolve_window(params.range, params.from, params.to, today)?;
        let interval = match params.interval.as_deref() {
            Some(s) => Some(
                s.parse::<Interval>()
                    .map_err(|e| invalid_params(format!("{e}; expected day, week or month")))?,
            ),
            None => None,
        };
        let query = sure_app::reports::NetWorthQuery {
            from,
            to,
            interval,
            currency: params.currency,
            attributed_to: parse_attribution(params.attributed_to.as_deref())?,
        };

        let series = self.state.reports.net_worth(&query).await.map_err(to_mcp)?;
        let decimals = self.currency_decimals().await?;
        let scale = Self::scale_of(&decimals, &series.currency);

        // Capped from the *end*: a daily series over ten years is thousands of points, and
        // the recent ones are the ones a question is about. Says so, rather than silently
        // starting the chart late.
        let total = series.points.len();
        let points = if total > self.config.max_rows {
            &series.points[total - self.config.max_rows..]
        } else {
            &series.points[..]
        };
        let rows: Vec<Vec<String>> = points
            .iter()
            .map(|p| {
                vec![
                    p.as_of.clone(),
                    money_to_string(p.net_worth_minor, scale),
                    money_to_string(p.assets_minor, scale),
                    money_to_string(p.liabilities_minor, scale),
                ]
            })
            .collect();

        let mut out = format!("Net worth in {}.\n", series.currency);
        if points.len() < total {
            out.push_str(&format!(
                "(showing the most recent {} of {total} points; use a coarser interval or a \
                 narrower window for the rest)\n",
                points.len()
            ));
        }
        out.push_str(&table(
            &["as_of", "net_worth", "assets", "liabilities"],
            &rows,
        ));
        out.push_str(&unconverted_note(&series.unconverted, &series.currency));
        if let Some(rates) = &series.rates_as_of {
            out.push_str(&format!("\nExchange rates as of {rates}."));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// Where the money went, as flows.
    #[tool(
        name = "money_flow",
        description = "The money-flow graph for a window: income sources into the household, \
                       and out again into expense categories and savings. Returns one line \
                       per flow.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn money_flow(
        &self,
        Parameters(params): Parameters<MoneyFlowParams>,
    ) -> ToolResult<CallToolResult> {
        let query = self.report_query(
            params.range,
            params.from,
            params.to,
            params.currency,
            params.include_one_off,
            params.attributed_to,
        )?;
        let graph = self.state.reports.sankey(&query).await.map_err(to_mcp)?;
        let decimals = self.currency_decimals().await?;
        let scale = Self::scale_of(&decimals, &graph.currency);

        let labels: std::collections::HashMap<&str, &str> = graph
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.label.as_str()))
            .collect();
        let name = |id: &str| labels.get(id).copied().unwrap_or(id).to_string();

        let rows: Vec<Vec<String>> = graph
            .links
            .iter()
            .map(|l| {
                vec![
                    name(&l.source),
                    name(&l.target),
                    money_to_string(l.value_minor, scale),
                ]
            })
            .collect();
        let mut out = format!("Money flow in {}.\n", graph.currency);
        out.push_str(&table(&["from", "to", "amount"], &rows));
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }
}

impl SureMcp {
    /// The window + currency + attribution arguments three of these tools share.
    pub(crate) fn report_query(
        &self,
        range: Option<Range>,
        from: Option<String>,
        to: Option<String>,
        currency: Option<String>,
        include_one_off: Option<bool>,
        attributed_to: Option<String>,
    ) -> ToolResult<ReportQuery> {
        let today = chrono::Utc::now().date_naive();
        let (from, to) = resolve_window(range, from, to, today)?;
        Ok(ReportQuery {
            from,
            to,
            include_one_off,
            currency,
            attributed_to: parse_attribution(attributed_to.as_deref())?,
        })
    }
}

/// `"joint"` or a person id — the one legal place this value is text.
pub(crate) fn parse_attribution(raw: Option<&str>) -> ToolResult<Option<Ownership>> {
    match raw {
        None => Ok(None),
        Some(s) => s
            .parse::<Ownership>()
            .map(Some)
            .map_err(|e| invalid_params(format!("{e}; expected \"joint\" or a person id"))),
    }
}

/// Names the currencies a report had to leave out.
///
/// Always rendered when non-empty, and worded as a warning rather than a footnote: the
/// totals above are wrong-by-omission without it, and a model summarising them will not
/// think to mention a field it was not told mattered.
pub(crate) fn unconverted_note(unconverted: &[String], currency: &str) -> String {
    if unconverted.is_empty() {
        return String::new();
    }
    format!(
        "\n\nINCOMPLETE: no exchange rate links {} to {currency}, so those transactions are \
         excluded from every total above. Say so when reporting these figures.",
        unconverted.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconverted_currency_produces_a_warning_a_summary_cannot_miss() {
        let note = unconverted_note(&["JPY".to_string(), "AUD".to_string()], "NZD");
        assert!(note.contains("INCOMPLETE"), "{note}");
        assert!(note.contains("JPY, AUD"), "{note}");
        assert!(note.contains("excluded"), "{note}");
    }

    #[test]
    fn a_fully_converted_report_says_nothing() {
        assert_eq!(unconverted_note(&[], "NZD"), "");
    }

    #[test]
    fn the_group_by_argument_maps_onto_the_domain_enum() {
        for (arg, expected) in [
            (GroupByArg::Category, GroupBy::Category),
            (GroupByArg::Merchant, GroupBy::Merchant),
            (GroupByArg::Account, GroupBy::Account),
            (GroupByArg::Month, GroupBy::Month),
        ] {
            assert_eq!(GroupBy::from(arg), expected);
        }
    }

    #[test]
    fn attribution_is_parsed_or_refused_never_widened() {
        assert_eq!(parse_attribution(None).unwrap(), None);
        assert_eq!(
            parse_attribution(Some("joint")).unwrap(),
            Some(Ownership::Joint)
        );
        assert!(parse_attribution(Some("everyone")).is_err());
    }
}
