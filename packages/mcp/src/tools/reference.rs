//! The three lists everything else takes ids from.
//!
//! Cheap, small, and worth calling first: a model that guesses a `category_id` writes to the
//! wrong category, and nothing downstream can tell that it did.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use sure_core::{CategoryKind, CategoryNode};

use crate::convert::table;
use crate::error::{to_mcp, ToolResult};
use crate::server::SureMcp;

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ListAccountsParams {
    /// Include accounts that have been archived. Off by default: an archived account is one
    /// the household has finished with, and including them makes every listing longer
    /// without making it more useful.
    #[serde(default)]
    pub include_archived: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct NoParams {}

#[tool_router(router = reference_router, vis = "pub")]
impl SureMcp {
    /// Every account with its current balance, currency and kind.
    ///
    /// The first call worth making: it yields the account ids every other tool takes, and
    /// the balances answer "how much is in X" without a second round trip.
    #[tool(
        name = "list_accounts",
        description = "List accounts with their current balance, currency, kind and owner. \
                       Returns the account ids other tools take. Start here.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn list_accounts(
        &self,
        Parameters(params): Parameters<ListAccountsParams>,
    ) -> ToolResult<CallToolResult> {
        let include_archived = params.include_archived.unwrap_or(false);
        let accounts = self
            .state
            .accounts
            .list(include_archived)
            .await
            .map_err(to_mcp)?;

        // Balances come from the report layer rather than being summed here: an account's
        // value is a latest-valuation question for an asset and a sum-of-transactions one
        // for cash, and `ReportService` is where that distinction already lives.
        let balances = self
            .state
            .reports
            .balances(&Default::default())
            .await
            .map_err(to_mcp)?;
        let by_id: std::collections::HashMap<i64, &sure_app::reports::AccountBalance> = balances
            .accounts
            .iter()
            .map(|a| (a.account_id, a))
            .collect();

        let decimals = self.currency_decimals().await?;
        let rows: Vec<Vec<String>> = accounts
            .iter()
            .map(|a| {
                let balance = by_id
                    .get(&a.id)
                    .map(|b| {
                        crate::convert::money_to_string(
                            b.value_minor,
                            decimals.get(&b.currency_code).copied().unwrap_or(2),
                        )
                    })
                    // An account the balances report does not carry (archived, or with no
                    // ledger at all) gets an empty cell rather than a misleading "0.00".
                    .unwrap_or_default();
                vec![
                    a.id.to_string(),
                    a.name.clone(),
                    a.kind.as_str().to_string(),
                    a.class.as_str().to_string(),
                    a.currency_code.clone(),
                    balance,
                    a.institution.clone().unwrap_or_default(),
                    if a.archived { "archived" } else { "" }.to_string(),
                ]
            })
            .collect();

        let mut out = table(
            &[
                "id",
                "name",
                "kind",
                "class",
                "currency",
                "balance",
                "institution",
                "state",
            ],
            &rows,
        );
        out.push_str(&format!(
            "\n\nBalances are as of {} in {}.",
            balances.as_of, balances.currency
        ));
        if !balances.unconverted.is_empty() {
            out.push_str(&format!(
                " No exchange rate for {} — those accounts are excluded from any total.",
                balances.unconverted.join(", ")
            ));
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }

    /// The category tree, flattened to paths.
    #[tool(
        name = "list_categories",
        description = "List every category as a full path (e.g. 'Food > Groceries') with its \
                       id and kind (income/expense/transfer).",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn list_categories(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> ToolResult<CallToolResult> {
        let tree = self.state.categories.tree().await.map_err(to_mcp)?;
        let mut rows = Vec::new();
        // Flattened rather than nested: a path in one cell is what a reader needs to pick
        // the right id, and an indented tree costs tokens to convey the same thing.
        for node in &tree {
            flatten(node, "", &mut rows);
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(table(
            &["id", "path", "kind"],
            &rows,
        ))]))
    }

    /// Custom merchants (payees) and the category each defaults to.
    #[tool(
        name = "list_merchants",
        description = "List custom merchants (payees) with their id and default category. \
                       These are the merchant_ids that categorising tools take.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn list_merchants(
        &self,
        Parameters(_): Parameters<NoParams>,
    ) -> ToolResult<CallToolResult> {
        let merchants = self.state.merchants.list().await.map_err(to_mcp)?;
        let categories = self.state.categories.list().await.map_err(to_mcp)?;
        let names: std::collections::HashMap<i64, &str> =
            categories.iter().map(|c| (c.id, c.name.as_str())).collect();

        let rows: Vec<Vec<String>> = merchants
            .iter()
            .map(|m| {
                vec![
                    m.id.to_string(),
                    m.name.clone(),
                    m.category_id
                        .and_then(|c| names.get(&c).map(|n| (*n).to_string()))
                        .unwrap_or_default(),
                    m.note.clone().unwrap_or_default(),
                ]
            })
            .collect();
        Ok(CallToolResult::success(vec![ContentBlock::text(table(
            &["id", "name", "default_category", "note"],
            &rows,
        ))]))
    }
}

/// Depth-first, building each node's `Parent > Child` path as it descends.
fn flatten(node: &CategoryNode, prefix: &str, out: &mut Vec<Vec<String>>) {
    let path = if prefix.is_empty() {
        node.category.name.clone()
    } else {
        format!("{prefix} > {}", node.category.name)
    };
    out.push(vec![
        node.category.id.to_string(),
        path.clone(),
        kind_str(node.category.kind).to_string(),
    ]);
    for child in &node.children {
        flatten(child, &path, out);
    }
}

/// `CategoryKind` has no `as_str` of its own; this is the one place it is rendered.
fn kind_str(kind: CategoryKind) -> &'static str {
    match kind {
        CategoryKind::Income => "income",
        CategoryKind::Expense => "expense",
        CategoryKind::Transfer => "transfer",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::Category;

    fn cat(id: i64, name: &str, kind: CategoryKind) -> Category {
        Category {
            id,
            name: name.to_string(),
            parent_id: None,
            kind,
            color: None,
            icon: None,
            sort_order: 0,
            created_at: String::new(),
        }
    }

    fn node(id: i64, name: &str, children: Vec<CategoryNode>) -> CategoryNode {
        CategoryNode {
            category: cat(id, name, CategoryKind::Expense),
            children,
        }
    }

    #[test]
    fn a_nested_category_is_listed_under_its_full_path() {
        let tree = vec![node(
            10,
            "Food",
            vec![node(11, "Groceries", vec![node(12, "Fresh", vec![])])],
        )];
        let mut rows = Vec::new();
        for n in &tree {
            flatten(n, "", &mut rows);
        }
        let paths: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
        assert_eq!(
            paths,
            vec!["Food", "Food > Groceries", "Food > Groceries > Fresh"]
        );
        // Parent first, so a reader meets a path only after the path it extends.
        assert_eq!(rows[0][0], "10");
    }

    #[test]
    fn every_category_kind_renders() {
        assert_eq!(kind_str(CategoryKind::Income), "income");
        assert_eq!(kind_str(CategoryKind::Expense), "expense");
        assert_eq!(kind_str(CategoryKind::Transfer), "transfer");
    }
}
