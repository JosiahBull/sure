//! Reference data a client can pull in as context rather than call for.
//!
//! Deliberately a thin layer over three tools that already exist. Client support for MCP
//! resources is uneven — plenty of clients only ever call tools — so the tools stay the
//! canonical path and these are the bonus for clients that can attach them to a conversation
//! without spending a turn on it.
//!
//! `sure://conventions` has no tool twin. It is the one thing worth handing a model before
//! it does anything: what the numbers mean.

use rmcp::model::{
    ListResourcesResult, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents,
};

use crate::error::{ToolResult, invalid_params};
use crate::server::SureMcp;

pub const ACCOUNTS_URI: &str = "sure://accounts";
pub const CATEGORIES_URI: &str = "sure://categories";
pub const MERCHANTS_URI: &str = "sure://merchants";
pub const CONVENTIONS_URI: &str = "sure://conventions";

/// What a reader has to know before any figure here means anything.
///
/// Every claim in it is one a model gets wrong unprompted: dividing an already-decimal
/// amount by 100, reading a negative as an error, treating a base-currency total as though
/// every account were in that currency.
pub const CONVENTIONS: &str = "\
# Reading Sure's data

## Money
Amounts arrive as decimal strings paired with a currency code: `\"-42.50\"`, `\"NZD\"`.
They are already in major units. Do not divide or multiply them.
A negative amount is money leaving the account; positive is money arriving.
Not every currency has two decimal places, so do not assume the scale.

## Dates
ISO-8601, `YYYY-MM-DD`. Tools that take a window also accept a named `range`
(`last_month`, `last_90_days`, `ytd`, `last_12_months`, `all_time`) — prefer it over
computing dates, and note that `last_month` means the previous *calendar* month.

## Currency conversion
Accounts may be held in different currencies. Reports normalise into the household's base
currency using the latest exchange rates on record. Where no rate links a currency to the
base, those transactions are **left out** of the totals rather than counted at parity, and
the currency is named in an `unconverted` note. A total carrying such a note is incomplete;
say so when reporting it.

## Accounts
Every account has a `kind` (bank, mortgage, real_estate, shares_us, …) and a derived
`class`. The class decides how its value is worked out: `cash` balances are the sum of
their transactions, while `asset` and `investment` accounts are valued by their most recent
valuation. `liability` accounts count negatively toward net worth.

## Categories
A tree, up to three levels. Tools report a category by its full path (`Food > Groceries`).
A transaction may have no category at all — that is a real state, shown as
`(uncategorised)`, and `search_transactions` can filter for exactly those rows.

## One-offs
A transaction flagged one-off (a house purchase, a tax refund) is excluded from spending
summaries by default, because leaving it in swamps every ordinary pattern.
";

impl SureMcp {
    /// The four resources, always the same four.
    pub fn resource_list(&self) -> ListResourcesResult {
        let resource = |uri: &str, name: &str, description: &str, mime: &str| {
            Resource::new(uri, name)
                .with_description(description)
                .with_mime_type(mime)
        };
        ListResourcesResult::with_all_items(vec![
            resource(
                CONVENTIONS_URI,
                "Conventions",
                "How to read Sure's amounts, dates, currencies and categories. Worth \
                     reading before interpreting any figure.",
                "text/markdown",
            ),
            resource(
                ACCOUNTS_URI,
                "Accounts",
                "Every account with its current balance, currency and kind.",
                "text/plain",
            ),
            resource(
                CATEGORIES_URI,
                "Categories",
                "The category tree, as full paths with ids.",
                "text/plain",
            ),
            resource(
                MERCHANTS_URI,
                "Merchants",
                "Custom merchants and their default categories.",
                "text/plain",
            ),
        ])
    }

    /// Read one.
    ///
    /// The three data resources delegate to the tools rather than re-querying, so a resource
    /// and its tool can never disagree about what an account list looks like.
    pub async fn resource_read(&self, uri: &str) -> ToolResult<ReadResourceResponse> {
        let text = match uri {
            CONVENTIONS_URI => CONVENTIONS.to_string(),
            ACCOUNTS_URI => self.tool_text(
                self.list_accounts(rmcp::handler::server::wrapper::Parameters(
                    crate::tools::reference::ListAccountsParams::default(),
                ))
                .await?,
            ),
            CATEGORIES_URI => self.tool_text(
                self.list_categories(rmcp::handler::server::wrapper::Parameters(
                    crate::tools::reference::NoParams::default(),
                ))
                .await?,
            ),
            MERCHANTS_URI => self.tool_text(
                self.list_merchants(rmcp::handler::server::wrapper::Parameters(
                    crate::tools::reference::NoParams::default(),
                ))
                .await?,
            ),
            // A genuinely open string — anything at all can arrive as a URI — so naming the
            // four is the only thing to do, and listing them beats "not found".
            other => {
                return Err(invalid_params(format!(
                    "unknown resource '{other}'; this server has {ACCOUNTS_URI}, \
                     {CATEGORIES_URI}, {MERCHANTS_URI} and {CONVENTIONS_URI}"
                )));
            }
        };
        Ok(
            ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
                uri: uri.to_string(),
                mime_type: Some("text/plain".to_string()),
                text,
                meta: None,
            }])
            .into(),
        )
    }

    /// Flatten a tool result back to the text a resource carries.
    fn tool_text(&self, result: rmcp::model::CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conventions text is the only defence against a model confidently reporting a
    /// figure a hundred times too large. These are the specific claims it has to carry.
    #[test]
    fn the_conventions_state_the_things_a_reader_otherwise_gets_wrong() {
        for claim in [
            "Do not divide or multiply",
            "negative amount is money leaving",
            "left out",
            "unconverted",
            "uncategorised",
            "previous *calendar* month",
        ] {
            assert!(
                CONVENTIONS.contains(claim),
                "conventions no longer say: {claim}"
            );
        }
    }

    /// A resource whose uri is not one of the four says which four there are — a bare
    /// "not found" leaves a client guessing at spellings.
    #[test]
    fn an_unknown_resource_names_the_ones_that_exist() {
        let err = invalid_params(format!(
            "unknown resource 'sure://budgets'; this server has {ACCOUNTS_URI}, \
             {CATEGORIES_URI}, {MERCHANTS_URI} and {CONVENTIONS_URI}"
        ));
        assert!(err.message.contains("sure://accounts"), "{}", err.message);
        assert!(
            err.message.contains("sure://conventions"),
            "{}",
            err.message
        );
    }
}
