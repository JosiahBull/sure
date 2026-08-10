//! Finding and inspecting individual transactions.
//!
//! The counterweight to these is `summarize_spending`: anything that ends in a total should
//! go there instead, and the truncation note on a capped result says so.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use sure_core::{Ownership, TxQuery};

use crate::convert::{money_to_string, resolve_window, table, truncation_note, Range};
use crate::error::{invalid_params, to_mcp, ToolResult};
use crate::server::SureMcp;

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct SearchTransactionsParams {
    /// Only this account (see list_accounts).
    #[serde(default)]
    pub account_id: Option<i64>,
    /// Only this category (see list_categories). Matches the category itself, not its
    /// children.
    #[serde(default)]
    pub category_id: Option<i64>,
    /// A named window. Ignored on whichever side `from`/`to` also specifies.
    #[serde(default)]
    pub range: Option<Range>,
    /// Inclusive start date, YYYY-MM-DD.
    #[serde(default)]
    pub from: Option<String>,
    /// Inclusive end date, YYYY-MM-DD.
    #[serde(default)]
    pub to: Option<String>,
    /// Case-insensitive substring, matched against description, merchant and notes.
    #[serde(default)]
    pub search: Option<String>,
    /// `true` returns only transactions with no category — the ones worth filing.
    /// `false` returns only those that have one. Omitted returns both.
    #[serde(default)]
    pub uncategorized: Option<bool>,
    /// Include one-off transactions (a house purchase, a tax refund). Default true.
    #[serde(default)]
    pub include_one_off: Option<bool>,
    /// Whose transactions: "joint", or a household member's person id.
    #[serde(default)]
    pub attributed_to: Option<String>,
    /// How many rows to return. Capped by the server; ask for fewer if you only need a
    /// sample.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Rows to skip, for paging through a result the cap truncated.
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetTransactionParams {
    pub id: i64,
}

#[tool_router(router = transactions_router, vis = "pub")]
impl SureMcp {
    /// Find transactions.
    #[tool(
        name = "search_transactions",
        description = "Find transactions by account, category, date window, text, or whether \
                       they are uncategorised. Returns rows, newest first. To total or \
                       compare spending use summarize_spending instead — do not fetch rows \
                       and add them up.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn search_transactions(
        &self,
        Parameters(params): Parameters<SearchTransactionsParams>,
    ) -> ToolResult<CallToolResult> {
        let today = chrono::Utc::now().date_naive();
        let (from, to) = resolve_window(params.range, params.from, params.to, today)?;

        let attributed_to = match params.attributed_to.as_deref() {
            Some(s) => Some(
                s.parse::<Ownership>()
                    // The one legal place this value is text (CLAUDE.md rule 1), so it is
                    // parsed here and an unrecognised one is refused rather than becoming a
                    // filter that silently matches the whole household.
                    .map_err(|e| {
                        invalid_params(format!("{e}; expected \"joint\" or a person id"))
                    })?,
            ),
            None => None,
        };

        // One more than will be shown, so "there is more than this" is something the answer
        // knows rather than implies. `TxQuery` returns no total and adding one would mean a
        // second count query on every call.
        let cap = self.config.max_rows as i64;
        let limit = params.limit.unwrap_or(50).clamp(1, cap);
        let offset = params.offset.unwrap_or(0).max(0);

        let rows = self
            .state
            .transactions
            .list(TxQuery {
                account_id: params.account_id,
                category_id: params.category_id,
                from,
                to,
                include_one_off: params.include_one_off,
                search: params.search,
                uncategorized: params.uncategorized,
                attributed_to,
                limit: Some(limit + 1),
                offset: Some(offset),
            })
            .await
            .map_err(to_mcp)?;

        let truncated = rows.len() as i64 > limit;
        let shown = &rows[..rows.len().min(limit as usize)];

        let decimals = self.currency_decimals().await?;
        let categories = self.state.categories.list().await.map_err(to_mcp)?;
        let category_names: std::collections::HashMap<i64, &str> =
            categories.iter().map(|c| (c.id, c.name.as_str())).collect();
        let accounts = self.state.accounts.list(true).await.map_err(to_mcp)?;
        let account_names: std::collections::HashMap<i64, &str> =
            accounts.iter().map(|a| (a.id, a.name.as_str())).collect();

        let table_rows: Vec<Vec<String>> = shown
            .iter()
            .map(|t| {
                vec![
                    t.id.to_string(),
                    t.posted_at.clone(),
                    account_names
                        .get(&t.account_id)
                        .map(|n| (*n).to_string())
                        .unwrap_or_else(|| t.account_id.to_string()),
                    money_to_string(t.amount_minor, Self::scale_of(&decimals, &t.currency_code)),
                    t.currency_code.clone(),
                    t.description.clone(),
                    t.merchant.clone().unwrap_or_default(),
                    t.category_id
                        .and_then(|c| category_names.get(&c).map(|n| (*n).to_string()))
                        // Named rather than left blank: "uncategorised" is the answer to a
                        // question people actually ask, and an empty cell reads as missing
                        // data instead of as a fact about the row.
                        .unwrap_or_else(|| "(uncategorised)".to_string()),
                    if t.is_one_off { "one-off" } else { "" }.to_string(),
                ]
            })
            .collect();

        let mut out = table(
            &[
                "id",
                "date",
                "account",
                "amount",
                "currency",
                "description",
                "merchant",
                "category",
                "flags",
            ],
            &table_rows,
        );
        if truncated {
            out.push_str(&truncation_note(shown.len(), offset + limit));
        } else if shown.is_empty() {
            out.push_str("\n(no transactions matched)");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// One transaction, in full.
    #[tool(
        name = "get_transaction",
        description = "Fetch one transaction with everything on it: notes, merchant, \
                       transfer link, source feed, and which rule categorised it.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn get_transaction(
        &self,
        Parameters(params): Parameters<GetTransactionParams>,
    ) -> ToolResult<CallToolResult> {
        let t = self
            .state
            .transactions
            .get(params.id)
            .await
            .map_err(to_mcp)?;
        let decimals = self.currency_decimals().await?;

        // A single row is JSON rather than a table: there is no repetition for a table to
        // save, and the caller is more likely to want a named field than a column.
        let body = serde_json::json!({
            "id": t.id,
            "account_id": t.account_id,
            "posted_at": t.posted_at,
            "amount": money_to_string(t.amount_minor, Self::scale_of(&decimals, &t.currency_code)),
            "currency": t.currency_code,
            "description": t.description,
            "merchant": t.merchant,
            "merchant_id": t.merchant_id,
            "category_id": t.category_id,
            "notes": t.notes,
            "is_one_off": t.is_one_off,
            "linked_transaction_id": t.linked_transaction_id,
            "provider": t.provider,
            "external_id": t.external_id,
            "categorized_by_rule_id": t.categorized_by_rule_id,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string()),
        )]))
    }
}
