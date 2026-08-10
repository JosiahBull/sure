//! The read half of the rules engine.
//!
//! `preview_rule` is here rather than with the writes on purpose. Trying an expression is
//! a read: it evaluates against the ledger and changes nothing. That means a read-only
//! server can still draft a rule and show exactly what it would catch — which is most of the
//! value, and all of it if the household would rather press the button themselves.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use sure_core::PreviewRequest;

use crate::convert::{money_to_string, table};
use crate::error::{to_mcp, ToolResult};
use crate::server::SureMcp;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewRuleParams {
    /// The expression to try.
    pub expression: String,
    /// How many matching transactions to show. The match *count* is always exact regardless.
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ListRulesParams {}

#[tool_router(router = rules_read_router, vis = "pub")]
impl SureMcp {
    /// Try an expression without saving anything.
    #[tool(
        name = "preview_rule",
        // The field list lives here rather than in a constant because this description is
        // the only place a model reads before writing an expression — left to itself it
        // invents plausible names (`payee`, `amount_cents`) and the rule matches nothing.
        description = "Evaluate a rule expression against the whole ledger and report how \
                       many transactions it matches, with a sample. Changes nothing. Always \
                       do this before saving a rule. \
                       The expression is a Zen expression (gorules.io) evaluated per \
                       transaction; truthy means match. Fields: amount, amount_minor, \
                       abs_amount, is_income, is_expense, description, merchant, \
                       merchant_id, notes, currency, account, account_kind, account_id, \
                       category_id, is_one_off, date, year, month, day. Example: \
                       is_expense and contains(lower(description), 'countdown')",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn preview_rule(
        &self,
        Parameters(params): Parameters<PreviewRuleParams>,
    ) -> ToolResult<CallToolResult> {
        let cap = self.config.max_rows as i64;
        let limit = params.limit.unwrap_or(20).clamp(1, cap);
        let preview = self
            .state
            .rules
            .preview(&PreviewRequest {
                expression: params.expression,
                limit: Some(limit),
            })
            .await
            .map_err(to_mcp)?;

        let decimals = self.currency_decimals().await?;
        let rows: Vec<Vec<String>> = preview
            .sample
            .iter()
            .map(|m| {
                vec![
                    m.transaction_id.to_string(),
                    m.posted_at.clone(),
                    money_to_string(m.amount_minor, Self::scale_of(&decimals, &m.currency_code)),
                    m.currency_code.clone(),
                    m.description.clone(),
                    m.category_id
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "(uncategorised)".to_string()),
                ]
            })
            .collect();

        // The count leads, and it is the exact total rather than the sample size — the
        // question a preview answers is "how much would this touch", and a sample of 20 out
        // of 4,000 read as "20" is how a rule gets saved that recategorises the ledger.
        let mut out = format!(
            "{} transaction(s) match. Showing {}.\n",
            preview.matched,
            rows.len()
        );
        out.push_str(&table(
            &[
                "id",
                "date",
                "amount",
                "currency",
                "description",
                "current_category",
            ],
            &rows,
        ));
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// The rules already in place.
    #[tool(
        name = "list_rules",
        description = "List the categorisation rules, in the order they are applied. Shows \
                       each rule's expression and what it sets.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn list_rules(
        &self,
        Parameters(_): Parameters<ListRulesParams>,
    ) -> ToolResult<CallToolResult> {
        let rules = self.state.rules.list().await.map_err(to_mcp)?;
        let rows: Vec<Vec<String>> = rules
            .iter()
            .map(|r| {
                vec![
                    r.id.to_string(),
                    r.priority.to_string(),
                    r.name.clone(),
                    r.expression.clone(),
                    r.set_category_id.map(|c| c.to_string()).unwrap_or_default(),
                    r.set_merchant_id.map(|m| m.to_string()).unwrap_or_default(),
                    r.set_one_off.map(|b| b.to_string()).unwrap_or_default(),
                    // Two flags that change what a run does to rows somebody already filed
                    // by hand; worth seeing before adding a rule beside them.
                    if r.overwrite_manual { "overwrite" } else { "" }.to_string(),
                    if r.stop_on_match { "stop" } else { "" }.to_string(),
                    if r.enabled { "" } else { "disabled" }.to_string(),
                ]
            })
            .collect();
        Ok(CallToolResult::success(vec![ContentBlock::text(table(
            &[
                "id",
                "priority",
                "name",
                "expression",
                "set_category_id",
                "set_merchant_id",
                "set_one_off",
                "manual",
                "chain",
                "state",
            ],
            &rows,
        ))]))
    }
}
