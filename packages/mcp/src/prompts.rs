//! Three saved workflows, for the things somebody actually does monthly.
//!
//! A prompt is user-invoked: it puts a phrasing in front of the model that names the right
//! tools in the right order. That matters most for the two jobs where the wrong first move
//! is expensive — pulling four thousand rows instead of asking for a total, and re-filing
//! the ledger without previewing first.

use rmcp::model::{
    GetPromptResponse, GetPromptResult, ListPromptsResult, Prompt, PromptArgument, PromptMessage,
    Role,
};

use crate::error::{invalid_params, ToolResult};
use crate::server::SureMcp;

pub const MONTHLY_REVIEW: &str = "monthly_review";
pub const TIDY_UNCATEGORISED: &str = "tidy_uncategorised";
pub const EXPLAIN_ACCOUNT: &str = "explain_account";

impl SureMcp {
    pub fn prompt_list(&self) -> ListPromptsResult {
        let explain = Prompt::new(
            EXPLAIN_ACCOUNT,
            Some("Explain one account: what it is, what it is worth, and what has moved."),
            Some(vec![{
                let mut arg = PromptArgument::new("account_id")
                    .with_description("The account to explain (from list_accounts).");
                arg.required = Some(true);
                arg
            }]),
        )
        .with_title("Explain an account");

        ListPromptsResult::with_all_items(vec![
            Prompt::new(
                MONTHLY_REVIEW,
                Some("Review last month's spending against the months before it."),
                None,
            ),
            Prompt::new(
                TIDY_UNCATEGORISED,
                Some("Work through the uncategorised transactions and propose rules."),
                None,
            ),
            explain,
        ])
    }

    pub fn prompt_get(
        &self,
        name: &str,
        arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> ToolResult<GetPromptResponse> {
        let text = match name {
            MONTHLY_REVIEW => MONTHLY_REVIEW_TEXT.to_string(),
            TIDY_UNCATEGORISED => TIDY_TEXT.to_string(),
            EXPLAIN_ACCOUNT => {
                let account_id = arguments
                    .and_then(|a| a.get("account_id"))
                    // Accepts either form: a client that types its arguments sends a number,
                    // one that stringifies everything sends `"12"`, and refusing the second
                    // would be refusing a client rather than a mistake.
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    })
                    .ok_or_else(|| {
                        invalid_params("explain_account needs an account_id (see list_accounts)")
                    })?;
                explain_account_text(account_id)
            }
            other => {
                return Err(invalid_params(format!(
                    "unknown prompt '{other}'; this server has {MONTHLY_REVIEW}, \
                     {TIDY_UNCATEGORISED} and {EXPLAIN_ACCOUNT}"
                )));
            }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }
}

const MONTHLY_REVIEW_TEXT: &str = "\
Review last month's money.

1. Call summarize_spending with group_by=\"category\", range=\"last_month\".
2. Call summarize_spending with group_by=\"month\", range=\"last_12_months\" so you have
   something to compare against — one month in isolation says very little.
3. Call summarize_spending with group_by=\"merchant\", range=\"last_month\", top_n=10.

Then tell me:
- What the month cost in total, and how that compares to the twelve-month pattern.
- Which categories moved most against their usual level, and by how much.
- Anything that looks like a one-off wrongly counted as ordinary spending, or the reverse.

Use the figures as given — they are already decimal amounts in the base currency. If any
result carries an INCOMPLETE note about unconverted currencies, say so explicitly.";

const TIDY_TEXT: &str = "\
Help me file the transactions that have no category.

1. Call search_transactions with uncategorized=true, range=\"last_90_days\", limit=100.
2. Group what you see by payee and by pattern, and call list_categories so you are
   proposing real category ids rather than names you have invented.

Then, for the groups worth automating:
- Draft a rule expression and run preview_rule on it. Report how many transactions it
  matches — the exact count, not the size of the sample.
- Show me each proposed rule and what it would catch, and wait for me to say yes.

Do not save or run any rule before I have seen its preview. For one-off rows that no rule
would sensibly cover, just list them with a suggested category and let me decide.";

fn explain_account_text(account_id: i64) -> String {
    format!(
        "\
Explain account {account_id} to me.

1. Call account_detail with account_id={account_id}.
2. Call summarize_spending with group_by=\"month\", range=\"last_12_months\",
   account_id is not a filter there, so instead call search_transactions with
   account_id={account_id}, range=\"last_90_days\", limit=50 to see recent movement.

Then tell me what this account is, what it is worth now, what has been moving through it,
and anything that looks unusual — a payment that stopped, a fee that started, a balance
moving the wrong way. Quote amounts exactly as given."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The monthly review exists to stop the model pulling rows and summing them. If it
    /// stops naming the aggregate tool it has lost its reason to exist.
    #[test]
    fn the_monthly_review_sends_the_model_to_the_aggregate_not_the_ledger() {
        assert!(MONTHLY_REVIEW_TEXT.contains("summarize_spending"));
        assert!(!MONTHLY_REVIEW_TEXT.contains("search_transactions"));
        assert!(MONTHLY_REVIEW_TEXT.contains("INCOMPLETE"));
    }

    /// And the tidying prompt exists to stop it writing before anyone has looked.
    #[test]
    fn the_tidy_prompt_insists_on_a_preview_and_on_asking_first() {
        assert!(TIDY_TEXT.contains("preview_rule"));
        assert!(TIDY_TEXT.contains("wait for me"));
        assert!(TIDY_TEXT.contains("Do not save or run any rule before"));
    }

    #[test]
    fn explaining_an_account_names_the_account_it_was_asked_about() {
        let text = explain_account_text(42);
        assert!(text.contains("account_id=42"), "{text}");
    }
}
