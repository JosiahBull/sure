//! The tools that change the ledger. Registered only when `SURE_MCP=write`.
//!
//! Everything here is either single-row, or previewed. The one tool that can touch many rows
//! at once — [`SureMcp::bulk_categorize`] — refuses to write until it has told the caller how
//! many rows it found and been told the same number back. That is not ceremony: an
//! off-by-one filter is the most likely mistake a model makes here, and it is the one
//! mistake whose blast radius is the whole ledger.
//!
//! Absent by design, at any mode: deleting anything, editing an account, importing or
//! exporting the configuration, and linking a bank feed. See `docs/MCP.md`.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use sure_core::transactions::BulkIds;
use sure_core::{
    BulkUpdate, CategoryKind, IsoDate, Money, NewValuation, RuleRunKind, SaveCategory,
    SaveMerchant, SaveRule, SaveTransaction, TxQuery,
};

use crate::convert::{Range, money_from_string, money_to_string, resolve_window};
use crate::error::{ToolResult, invalid_params, to_mcp};
use crate::server::SureMcp;
use crate::tools::reports::parse_attribution;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateTransactionParams {
    pub id: i64,
    /// Set the category. Pass null to clear it. Omit to leave it alone.
    #[serde(default, deserialize_with = "double_option")]
    pub category_id: Option<Option<i64>>,
    /// Set the merchant. Pass null to clear it. Omit to leave it alone.
    #[serde(default, deserialize_with = "double_option")]
    pub merchant_id: Option<Option<i64>>,
    /// Mark (or unmark) this as a one-off — something that should not count toward normal
    /// spending patterns.
    #[serde(default)]
    pub is_one_off: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BulkCategorizeParams {
    /// The transactions to change. Either this or a filter, not both.
    #[serde(default)]
    pub ids: Option<Vec<i64>>,
    /// Select rows to change by the same filters search_transactions takes.
    #[serde(default)]
    pub account_id: Option<i64>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub range: Option<Range>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub uncategorized: Option<bool>,
    #[serde(default)]
    pub attributed_to: Option<String>,

    /// The category to file them under.
    #[serde(default)]
    pub category_id: Option<i64>,
    /// The merchant to attach.
    #[serde(default)]
    pub merchant_id: Option<i64>,
    #[serde(default)]
    pub is_one_off: Option<bool>,

    /// Leave unset (or true) to see what would change without changing it. To actually
    /// write, pass false AND pass expect_count with the number the dry run reported.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// The row count a dry run reported. Required when dry_run is false; the write is
    /// refused if the filter now matches a different number of rows.
    #[serde(default)]
    pub expect_count: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateTransactionParams {
    pub account_id: i64,
    /// YYYY-MM-DD.
    pub posted_at: String,
    /// A decimal string. Negative is money out, e.g. "-42.50".
    pub amount: String,
    pub description: String,
    /// Defaults to the account's currency.
    #[serde(default)]
    pub currency_code: Option<String>,
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub merchant_id: Option<i64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub is_one_off: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecordValuationParams {
    pub account_id: i64,
    /// YYYY-MM-DD.
    pub as_of: String,
    /// What the account is worth on that date, as a decimal string. For a liability this is
    /// negative.
    pub value: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateMerchantParams {
    pub name: String,
    /// The category transactions from this merchant should default to.
    #[serde(default)]
    pub category_id: Option<i64>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateCategoryParams {
    pub name: String,
    /// Nest under this category. Omit for a top-level one.
    #[serde(default)]
    pub parent_id: Option<i64>,
    /// income, expense (default) or transfer.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SaveRuleParams {
    /// Omit to create a new rule; pass an existing id to replace it.
    #[serde(default)]
    pub id: Option<i64>,
    pub name: String,
    /// The match expression. Run preview_rule on it first.
    pub expression: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub set_category_id: Option<i64>,
    #[serde(default)]
    pub set_merchant_id: Option<i64>,
    #[serde(default)]
    pub set_one_off: Option<bool>,
    /// Also re-file transactions somebody categorised by hand. Default false.
    #[serde(default)]
    pub overwrite_manual: Option<bool>,
    /// Stop evaluating later rules for a transaction this one matches. Default false.
    #[serde(default)]
    pub stop_on_match: Option<bool>,
    /// Lower runs first. Default 0.
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct RunRuleParams {
    /// Run just this rule. Omit to run every enabled rule in priority order.
    #[serde(default)]
    pub rule_id: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UndoRuleRunParams {
    /// The run_id that run_rule reported.
    pub run_id: i64,
}

#[tool_router(router = writes_router, vis = "pub")]
impl SureMcp {
    /// Patch one transaction.
    #[tool(
        name = "update_transaction",
        description = "Change one transaction's category, merchant, or one-off flag. Only the \
                       fields you pass are touched; pass null to clear a category or \
                       merchant.",
        annotations(read_only_hint = false, idempotent_hint = true)
    )]
    pub async fn update_transaction(
        &self,
        Parameters(params): Parameters<UpdateTransactionParams>,
    ) -> ToolResult<CallToolResult> {
        if params.category_id.is_none()
            && params.merchant_id.is_none()
            && params.is_one_off.is_none()
        {
            return Err(invalid_params(
                "nothing to change: pass at least one of category_id, merchant_id or is_one_off",
            ));
        }
        // A patch through `bulk_update` rather than `update`, which takes a whole
        // `SaveTransaction` and would make the caller re-send every field it is not
        // changing — a read-modify-write a model gets wrong by dropping the notes.
        let affected = self
            .state
            .transactions
            .bulk_update(BulkUpdate {
                ids: bulk_ids(vec![params.id])?,
                category_id: params.category_id,
                merchant_id: params.merchant_id,
                is_one_off: params.is_one_off,
                ownership: None,
            })
            .await
            .map_err(to_mcp)?;
        Ok(text(format!("Updated {affected} transaction.")))
    }

    /// File many transactions at once — after saying how many.
    #[tool(
        name = "bulk_categorize",
        description = "Set the category, merchant or one-off flag on many transactions at \
                       once, chosen by id or by filter. Runs as a dry run unless you pass \
                       dry_run=false together with expect_count matching what the dry run \
                       reported.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    pub async fn bulk_categorize(
        &self,
        Parameters(params): Parameters<BulkCategorizeParams>,
    ) -> ToolResult<CallToolResult> {
        if params.category_id.is_none()
            && params.merchant_id.is_none()
            && params.is_one_off.is_none()
        {
            return Err(invalid_params(
                "nothing to set: pass at least one of category_id, merchant_id or is_one_off",
            ));
        }

        let has_filter = params.account_id.is_some()
            || params.search.is_some()
            || params.range.is_some()
            || params.from.is_some()
            || params.to.is_some()
            || params.uncategorized.is_some()
            || params.attributed_to.is_some();

        // Ids and a filter together would leave "which won?" to be inferred from the row
        // count — and inferred wrongly in exactly the case that matters, where the filter is
        // broader than the list.
        let ids: Vec<i64> = match (&params.ids, has_filter) {
            (Some(_), true) => {
                return Err(invalid_params(
                    "pass either ids or a filter, not both — they would select different rows",
                ));
            }
            (Some(ids), false) => ids.clone(),
            (None, true) => self.matching_ids(&params).await?,
            (None, false) => {
                return Err(invalid_params(
                    "no rows selected: pass ids, or at least one filter (account_id, search, \
                     range/from/to, uncategorized, attributed_to)",
                ));
            }
        };

        if ids.is_empty() {
            return Ok(text("Nothing matched; no changes made.".to_string()));
        }

        // The gate. Default-on, and the confirmation has to carry the *number* rather than
        // just `dry_run: false` — a model that would blindly flip a boolean has to have
        // read the count to get past this, and a filter whose result moved in between is
        // refused rather than applied to a set nobody looked at.
        let dry_run = params.dry_run.unwrap_or(true);
        if dry_run {
            return Ok(text(format!(
                "Dry run: {} transaction(s) would be changed.\nTo apply, call again with \
                 dry_run=false and expect_count={}.\nFirst ids: {}",
                ids.len(),
                ids.len(),
                preview_ids(&ids),
            )));
        }
        match params.expect_count {
            None => {
                return Err(invalid_params(format!(
                    "dry_run=false needs expect_count. This filter currently matches {} \
                     transaction(s); pass expect_count={} to apply.",
                    ids.len(),
                    ids.len()
                )));
            }
            Some(expected) if expected != ids.len() => {
                return Err(invalid_params(format!(
                    "refusing to write: you expected {expected} transaction(s) but this now \
                     matches {}. Re-run the dry run and check the selection.",
                    ids.len()
                )));
            }
            Some(_) => {}
        }

        let affected = self
            .state
            .transactions
            .bulk_update(BulkUpdate {
                ids: bulk_ids(ids)?,
                // A bulk call sets values rather than clearing them: `Some(Some(id))` to
                // write, absent to leave alone. Clearing a category across many rows is not
                // something to reach by accident, and `update_transaction` can still do it
                // one row at a time.
                category_id: params.category_id.map(Some),
                merchant_id: params.merchant_id.map(Some),
                is_one_off: params.is_one_off,
                ownership: None,
            })
            .await
            .map_err(to_mcp)?;
        Ok(text(format!("Updated {affected} transaction(s).")))
    }

    /// Add a transaction by hand.
    #[tool(
        name = "create_transaction",
        description = "Record a transaction. Amount is a decimal string; negative is money \
                       out.",
        annotations(read_only_hint = false)
    )]
    pub async fn create_transaction(
        &self,
        Parameters(params): Parameters<CreateTransactionParams>,
    ) -> ToolResult<CallToolResult> {
        let account = self
            .state
            .accounts
            .get(params.account_id)
            .await
            .map_err(to_mcp)?;
        let currency = params
            .currency_code
            .clone()
            .unwrap_or_else(|| account.currency_code.clone());
        let decimals = self.currency_decimals().await?;
        let amount_minor = money_from_string(&params.amount, Self::scale_of(&decimals, &currency))?;

        let created = self
            .state
            .transactions
            .create(SaveTransaction {
                account_id: params.account_id,
                posted_at: iso_date(&params.posted_at)?,
                amount_minor: money(amount_minor)?,
                currency_code: Some(currency.clone()),
                description: params.description,
                merchant: None,
                merchant_id: params.merchant_id,
                ownership: None,
                notes: params.notes,
                category_id: params.category_id,
                is_one_off: params.is_one_off.unwrap_or(false),
            })
            .await
            .map_err(to_mcp)?;
        Ok(text(format!(
            "Created transaction {} on {}: {} {} — {}",
            created.id,
            created.posted_at,
            money_to_string(created.amount_minor, Self::scale_of(&decimals, &currency)),
            currency,
            created.description
        )))
    }

    /// Record what something is worth today.
    #[tool(
        name = "record_valuation",
        description = "Record a point-in-time value for an account — a house revaluation, a \
                       car's current worth. Feeds net worth from that date on.",
        annotations(read_only_hint = false, idempotent_hint = true)
    )]
    pub async fn record_valuation(
        &self,
        Parameters(params): Parameters<RecordValuationParams>,
    ) -> ToolResult<CallToolResult> {
        let account = self
            .state
            .accounts
            .get(params.account_id)
            .await
            .map_err(to_mcp)?;
        let decimals = self.currency_decimals().await?;
        let scale = Self::scale_of(&decimals, &account.currency_code);
        let value_minor = money_from_string(&params.value, scale)?;

        let created = self
            .state
            .valuations
            .create(
                params.account_id,
                NewValuation {
                    as_of: iso_date(&params.as_of)?,
                    value_minor: money(value_minor)?,
                    currency_code: None,
                    note: params.note,
                },
            )
            .await
            .map_err(to_mcp)?;
        Ok(text(format!(
            "Recorded {} {} for {} as of {}.",
            money_to_string(created.value_minor, scale),
            created.currency_code,
            account.name,
            created.as_of
        )))
    }

    /// Add a merchant.
    #[tool(
        name = "create_merchant",
        description = "Create a merchant (payee), optionally with a default category so \
                       future transactions from it are filed automatically.",
        annotations(read_only_hint = false)
    )]
    pub async fn create_merchant(
        &self,
        Parameters(params): Parameters<CreateMerchantParams>,
    ) -> ToolResult<CallToolResult> {
        let created = self
            .state
            .merchants
            .create(SaveMerchant {
                name: params.name,
                category_id: params.category_id,
                note: params.note,
            })
            .await
            .map_err(to_mcp)?;
        Ok(text(format!(
            "Created merchant {} (id {}).",
            created.name, created.id
        )))
    }

    /// Add a category.
    #[tool(
        name = "create_category",
        description = "Create a category, optionally nested under an existing one.",
        annotations(read_only_hint = false)
    )]
    pub async fn create_category(
        &self,
        Parameters(params): Parameters<CreateCategoryParams>,
    ) -> ToolResult<CallToolResult> {
        let kind = match params.kind.as_deref() {
            None => CategoryKind::Expense,
            Some("income") => CategoryKind::Income,
            Some("expense") => CategoryKind::Expense,
            Some("transfer") => CategoryKind::Transfer,
            Some(other) => {
                return Err(invalid_params(format!(
                    "unknown category kind '{other}' (expected income, expense or transfer)"
                )));
            }
        };
        let created = self
            .state
            .categories
            .create(SaveCategory {
                name: params.name,
                parent_id: params.parent_id,
                kind,
                color: None,
                icon: None,
                sort_order: 0,
            })
            .await
            .map_err(to_mcp)?;
        Ok(text(format!(
            "Created category {} (id {}).",
            created.name, created.id
        )))
    }

    /// Save a rule.
    #[tool(
        name = "save_rule",
        description = "Create or replace a categorisation rule. Saving does not apply it — \
                       call run_rule for that. Preview the expression first.",
        annotations(read_only_hint = false)
    )]
    pub async fn save_rule(
        &self,
        Parameters(params): Parameters<SaveRuleParams>,
    ) -> ToolResult<CallToolResult> {
        let input = SaveRule {
            name: params.name,
            description: params.description,
            expression: params.expression,
            set_category_id: params.set_category_id,
            set_one_off: params.set_one_off,
            set_merchant_id: params.set_merchant_id,
            overwrite_manual: params.overwrite_manual.unwrap_or(false),
            stop_on_match: params.stop_on_match.unwrap_or(false),
            priority: params.priority.unwrap_or(0),
            enabled: params.enabled.unwrap_or(true),
        };
        let (rule, verb) = match params.id {
            Some(id) => (
                self.state.rules.update(id, input).await.map_err(to_mcp)?,
                "Updated",
            ),
            None => (
                self.state.rules.create(input).await.map_err(to_mcp)?,
                "Created",
            ),
        };
        Ok(text(format!(
            "{verb} rule {} (id {}). Nothing has been re-filed yet — call run_rule with \
             rule_id={} to apply it.",
            rule.name, rule.id, rule.id
        )))
    }

    /// Apply rules to the ledger.
    #[tool(
        name = "run_rule",
        description = "Apply one rule, or every enabled rule, to the whole ledger. Reports \
                       how many transactions changed and a run_id that undo_rule_run can \
                       reverse.",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    pub async fn run_rule(
        &self,
        Parameters(params): Parameters<RunRuleParams>,
    ) -> ToolResult<CallToolResult> {
        let (rules, kind) = match params.rule_id {
            Some(id) => {
                let rule = self.state.rules.get(id).await.map_err(to_mcp)?;
                (vec![rule], RuleRunKind::Single)
            }
            None => (
                self.state.rules.enabled_rules().await.map_err(to_mcp)?,
                RuleRunKind::All,
            ),
        };
        let result = self
            .state
            .rules
            .run(&rules, params.rule_id, kind)
            .await
            .map_err(to_mcp)?;
        Ok(text(format!(
            "Run {}: {} transaction(s) matched, {} changed. To reverse this exactly, call \
             undo_rule_run with run_id={}.",
            result.run_id, result.matched, result.changed, result.run_id
        )))
    }

    /// Put a rule run back.
    #[tool(
        name = "undo_rule_run",
        description = "Reverse a rule run, restoring every transaction it changed to what it \
                       was before.",
        annotations(read_only_hint = false)
    )]
    pub async fn undo_rule_run(
        &self,
        Parameters(params): Parameters<UndoRuleRunParams>,
    ) -> ToolResult<CallToolResult> {
        let result = self
            .state
            .rules
            .undo_run(params.run_id)
            .await
            .map_err(to_mcp)?;
        Ok(text(format!(
            "Undid run {}: {} transaction(s) restored.",
            params.run_id, result.changed
        )))
    }
}

impl SureMcp {
    /// The ids a `bulk_categorize` filter currently selects.
    ///
    /// Fetches one past the bulk cap so an over-broad filter is refused with the reason
    /// rather than silently truncated to the first 5,000 rows.
    async fn matching_ids(&self, params: &BulkCategorizeParams) -> ToolResult<Vec<i64>> {
        let today = chrono::Utc::now().date_naive();
        let (from, to) =
            resolve_window(params.range, params.from.clone(), params.to.clone(), today)?;
        let rows = self
            .state
            .transactions
            .list(TxQuery {
                account_id: params.account_id,
                category_id: None,
                from,
                to,
                include_one_off: None,
                search: params.search.clone(),
                uncategorized: params.uncategorized,
                attributed_to: parse_attribution(params.attributed_to.as_deref())?,
                limit: Some(sure_core::transactions::MAX_BULK_IDS as i64 + 1),
                offset: None,
            })
            .await
            .map_err(to_mcp)?;
        if rows.len() > sure_core::transactions::MAX_BULK_IDS {
            return Err(invalid_params(format!(
                "that filter matches more than {} transactions, which is the most one call \
                 may change. Narrow it — by account, by date window, or by search text.",
                sure_core::transactions::MAX_BULK_IDS
            )));
        }
        Ok(rows.into_iter().map(|t| t.id).collect())
    }
}

fn text(body: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(body)])
}

/// The first few ids, so a dry run is checkable rather than merely counted.
fn preview_ids(ids: &[i64]) -> String {
    const SHOWN: usize = 10;
    let head: Vec<String> = ids.iter().take(SHOWN).map(|i| i.to_string()).collect();
    if ids.len() > SHOWN {
        format!("{} … and {} more", head.join(", "), ids.len() - SHOWN)
    } else {
        head.join(", ")
    }
}

fn bulk_ids(ids: Vec<i64>) -> ToolResult<BulkIds> {
    BulkIds::new(ids).map_err(to_mcp)
}

/// A present `null` clears the field; an omitted one leaves it alone. Plain
/// `Option<Option<T>>` cannot tell the two apart — the same helper, and the same reason, as
/// `sure_core::transactions::double_option`, which is private to that module.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

fn iso_date(raw: &str) -> ToolResult<IsoDate> {
    IsoDate::parse(raw.trim()).map_err(|e| invalid_params(format!("{e} (dates are YYYY-MM-DD)")))
}

fn money(minor: i64) -> ToolResult<Money> {
    Money::new(minor).map_err(to_mcp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_run_preview_lists_ids_and_says_how_many_it_hid() {
        assert_eq!(preview_ids(&[1, 2, 3]), "1, 2, 3");
        let many: Vec<i64> = (1..=25).collect();
        let out = preview_ids(&many);
        assert!(out.starts_with("1, 2, 3, 4, 5, 6, 7, 8, 9, 10 "), "{out}");
        assert!(out.contains("15 more"), "{out}");
    }

    #[test]
    fn an_empty_id_list_is_refused_by_the_bulk_cap_rather_than_written() {
        assert!(bulk_ids(vec![]).is_err());
        assert!(bulk_ids(vec![1]).is_ok());
    }

    #[test]
    fn a_date_that_is_not_iso_says_what_one_looks_like() {
        let err = iso_date("10/08/2026").unwrap_err();
        assert!(err.message.contains("YYYY-MM-DD"), "{}", err.message);
    }
}
