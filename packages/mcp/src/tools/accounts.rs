//! One account, and whatever that kind of account actually means.
//!
//! A house's interesting facts are its debt and its paid-off percentage; a brokerage
//! account's are its positions; a private-shares account's are its vesting grants. The HTTP
//! API has a separate endpoint per case and expects the caller to know which to call. This
//! is one tool that dispatches on the kind, so a model that only knows an account id can ask
//! the obvious question and get the right answer.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router};
use serde::Deserialize;
use sure_core::{AccountKind, Valuation};

use crate::convert::{money_to_string, table};
use crate::error::{invalid_params, to_mcp, ToolResult};
use crate::server::SureMcp;

/// How many of an account's valuations to show. Enough to see a trend; the full series is
/// what `net_worth` is for.
const RECENT_VALUATIONS: usize = 10;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AccountDetailParams {
    /// From list_accounts.
    pub account_id: i64,
    /// Value the account as at this date (YYYY-MM-DD). Defaults to today.
    #[serde(default)]
    pub as_of: Option<String>,
}

#[tool_router(router = accounts_router, vis = "pub")]
impl SureMcp {
    /// Everything worth knowing about one account.
    #[tool(
        name = "account_detail",
        description = "Full detail for one account: its settings and recent valuations, plus \
                       whatever its kind adds — holdings and their market value for a \
                       brokerage or shares account, secured debt and paid-off percentage for \
                       a property, vesting grants for private equity.",
        annotations(read_only_hint = true, idempotent_hint = true)
    )]
    pub async fn account_detail(
        &self,
        Parameters(params): Parameters<AccountDetailParams>,
    ) -> ToolResult<CallToolResult> {
        let as_of = match params.as_of.as_deref() {
            Some(s) => chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| {
                invalid_params(format!("'{s}' is not an ISO-8601 date (YYYY-MM-DD)"))
            })?,
            None => chrono::Utc::now().date_naive(),
        };

        let account = self
            .state
            .accounts
            .get(params.account_id)
            .await
            .map_err(to_mcp)?;
        let decimals = self.currency_decimals().await?;
        let scale = Self::scale_of(&decimals, &account.currency_code);

        let mut out = format!(
            "{} (id {}) — {} / {}, {}{}{}\n",
            account.name,
            account.id,
            account.kind.as_str(),
            account.class.as_str(),
            account.currency_code,
            account
                .institution
                .as_deref()
                .map(|i| format!(", {i}"))
                .unwrap_or_default(),
            if account.archived { ", archived" } else { "" },
        );

        // The typed metadata, rendered as JSON: it is a discriminated union with a different
        // shape per kind, and flattening it into columns would lose the discriminator that
        // says which shape it is.
        match serde_json::to_string_pretty(&account.metadata) {
            Ok(json) => out.push_str(&format!("\nSettings:\n{json}\n")),
            // Metadata that will not serialise is not worth failing the whole call over —
            // the caller still gets the account, the valuations, and the kind-specific half.
            Err(e) => {
                tracing::warn!(error = %e, account_id = account.id, "account metadata did not serialise");
                out.push_str("\nSettings: (unavailable)\n");
            }
        }

        // One more than will be shown, so "there are older ones" is known rather than
        // assumed — and so a brokerage account revalued daily for five years does not load
        // two thousand rows to print ten.
        let valuations = self
            .state
            .valuations
            .list_for_account(
                account.id,
                sure_core::ValuationQuery {
                    source: None,
                    limit: Some(RECENT_VALUATIONS as i64 + 1),
                },
            )
            .await
            .map_err(to_mcp)?;
        out.push_str(&valuation_section(&valuations, scale));

        // The kind-specific half. Exhaustive over `AccountKind`: adding a kind should be a
        // compile error here, so somebody decides what its detail view is rather than it
        // silently falling into "nothing extra".
        match account.kind {
            AccountKind::Brokerage
            | AccountKind::SharesNz
            | AccountKind::SharesUs
            | AccountKind::Crypto => {
                out.push_str(&self.holdings_section(account.id, as_of, &decimals).await?);
            }
            AccountKind::SharesPrivate => {
                out.push_str(&self.vesting_section(account.id, as_of, &decimals).await?);
            }
            AccountKind::RealEstate | AccountKind::Vehicle | AccountKind::Asset => {
                out.push_str(&self.equity_section(account.id, &decimals).await?);
            }
            // Nothing a balance and a valuation history do not already say. A liability's
            // terms live in the metadata printed above.
            AccountKind::Cash
            | AccountKind::Bank
            | AccountKind::Savings
            | AccountKind::CreditCard
            | AccountKind::RevolvingCredit
            | AccountKind::Mortgage
            | AccountKind::StudentLoan
            | AccountKind::Loan
            | AccountKind::Liability => {}
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(out)]))
    }
}

impl SureMcp {
    /// Positions and their market value, priced as of `as_of`.
    async fn holdings_section(
        &self,
        account_id: i64,
        as_of: chrono::NaiveDate,
        decimals: &std::collections::HashMap<String, u32>,
    ) -> ToolResult<String> {
        let snapshot = self
            .state
            .brokerage
            .snapshot(
                Some(self.state.stock_price_provider.as_ref()),
                account_id,
                as_of,
            )
            .await
            .map_err(to_mcp)?;
        let scale = Self::scale_of(decimals, &snapshot.currency_code);

        let rows: Vec<Vec<String>> = snapshot
            .positions
            .iter()
            .map(|p| {
                vec![
                    p.ticker.clone(),
                    p.exchange.clone(),
                    // Quantities are share counts, not money — printed as they are rather
                    // than run through the minor-unit conversion.
                    p.quantity.to_string(),
                    p.price.clone().unwrap_or_default(),
                    p.currency_code.clone(),
                    p.market_value_minor
                        .map(|m| money_to_string(m, Self::scale_of(decimals, &p.currency_code)))
                        .unwrap_or_default(),
                    p.cost_basis_minor
                        .map(|m| money_to_string(m, Self::scale_of(decimals, &p.currency_code)))
                        .unwrap_or_default(),
                ]
            })
            .collect();

        let mut out = format!("\nHoldings as of {}:\n", snapshot.as_of);
        out.push_str(&table(
            &[
                "ticker",
                "exchange",
                "quantity",
                "price",
                "currency",
                "market_value",
                "cost_basis",
            ],
            &rows,
        ));
        if !snapshot.wallets.is_empty() {
            let wallet_rows: Vec<Vec<String>> = snapshot
                .wallets
                .iter()
                .map(|w| {
                    vec![
                        w.currency_code.clone(),
                        money_to_string(w.amount_minor, Self::scale_of(decimals, &w.currency_code)),
                    ]
                })
                .collect();
            out.push_str("\n\nCash wallets:\n");
            out.push_str(&table(&["currency", "amount"], &wallet_rows));
        }
        out.push_str(&format!(
            "\n\nTotal {} {}.",
            money_to_string(snapshot.total_value_minor, scale),
            snapshot.currency_code
        ));
        out.push_str(&crate::tools::reports::unconverted_note(
            &snapshot.unconverted,
            &snapshot.currency_code,
        ));
        Ok(out)
    }

    /// Vesting grants: how much has vested, how much is exercisable, what it is worth.
    async fn vesting_section(
        &self,
        account_id: i64,
        as_of: chrono::NaiveDate,
        decimals: &std::collections::HashMap<String, u32>,
    ) -> ToolResult<String> {
        let equity = self
            .state
            .equity
            .account_equity(account_id, Some(&as_of.to_string()))
            .await
            .map_err(to_mcp)?;
        let scale = Self::scale_of(decimals, &equity.currency_code);

        let rows: Vec<Vec<String>> = equity
            .grants
            .iter()
            .map(|g| {
                vec![
                    g.grant_id.to_string(),
                    g.company.clone(),
                    g.quantity.to_string(),
                    g.vested.to_string(),
                    g.unvested.to_string(),
                    g.exercised.to_string(),
                    g.vested_unexercised.to_string(),
                    money_to_string(g.strike_minor, Self::scale_of(decimals, &g.currency_code)),
                    money_to_string(
                        g.intrinsic_value_minor,
                        Self::scale_of(decimals, &g.currency_code),
                    ),
                ]
            })
            .collect();

        let mut out = format!("\nVesting as of {}:\n", equity.as_of);
        out.push_str(&table(
            &[
                "grant_id",
                "company",
                "granted",
                "vested",
                "unvested",
                "exercised",
                "exercisable",
                "strike",
                "intrinsic_value",
            ],
            &rows,
        ));
        out.push_str(&format!(
            "\n\nTotal intrinsic value {} {}.",
            money_to_string(equity.total_intrinsic_minor, scale),
            equity.currency_code
        ));
        Ok(out)
    }

    /// Secured debt against an asset, and how much of it is owned outright.
    async fn equity_section(
        &self,
        account_id: i64,
        decimals: &std::collections::HashMap<String, u32>,
    ) -> ToolResult<String> {
        let position = self
            .state
            .reports
            .equity_position(account_id, &Default::default())
            .await
            .map_err(to_mcp)?;
        let scale = Self::scale_of(decimals, &position.currency);

        // No secured debt is a fact, not an empty table: the asset is owned outright, and
        // saying so is shorter and clearer than a header with no rows under it.
        if position.liabilities.is_empty() {
            return Ok(format!(
                "\nWorth {} {} as of {}, with no debt secured against it (100% owned).",
                money_to_string(position.value_minor, scale),
                position.currency,
                position.as_of
            ));
        }

        let rows: Vec<Vec<String>> = position
            .liabilities
            .iter()
            .map(|l| {
                vec![
                    l.account_id.to_string(),
                    l.name.clone(),
                    l.kind.as_str().to_string(),
                    money_to_string(l.balance_minor, scale),
                ]
            })
            .collect();
        let mut out = format!("\nSecured debt as of {}:\n", position.as_of);
        out.push_str(&table(&["account_id", "name", "kind", "balance"], &rows));
        out.push_str(&format!(
            "\n\nValue {} − debt {} = equity {} {} ({:.1}% owned).",
            money_to_string(position.value_minor, scale),
            money_to_string(position.total_debt_minor, scale),
            money_to_string(position.equity_minor, scale),
            position.currency,
            position.paid_off_pct
        ));
        Ok(out)
    }
}

/// The most recent valuations, newest first.
///
/// `valuations` is expected to hold one row more than [`RECENT_VALUATIONS`], which is how
/// "there are older ones" is stated rather than guessed at.
fn valuation_section(valuations: &[Valuation], scale: u32) -> String {
    if valuations.is_empty() {
        return "\nNo valuations recorded.\n".to_string();
    }
    let more = valuations.len() > RECENT_VALUATIONS;
    let rows: Vec<Vec<String>> = valuations
        .iter()
        .take(RECENT_VALUATIONS)
        .map(|v| {
            vec![
                v.as_of.clone(),
                money_to_string(v.value_minor, scale),
                v.source.as_str().to_string(),
                v.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    let mut out = format!(
        "\nValuations ({} most recent{}):\n",
        rows.len(),
        if more { ", older ones exist" } else { "" }
    );
    out.push_str(&table(&["as_of", "value", "source", "note"], &rows));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::ValuationSource;

    fn valuation(as_of: &str, value_minor: i64) -> Valuation {
        Valuation {
            id: 1,
            account_id: 1,
            as_of: as_of.to_string(),
            value_minor,
            currency_code: "NZD".to_string(),
            source: ValuationSource::Manual,
            note: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn an_account_with_no_valuations_says_so_rather_than_showing_an_empty_table() {
        let out = valuation_section(&[], 2);
        assert!(out.contains("No valuations recorded"), "{out}");
        assert!(!out.contains("as_of |"), "{out}");
    }

    /// A house valued every month for a decade would otherwise print 120 rows into the
    /// middle of an answer about one account.
    #[test]
    fn a_long_valuation_history_is_capped_and_says_that_it_was() {
        let vals: Vec<Valuation> = (1..=RECENT_VALUATIONS + 1)
            .map(|i| valuation(&format!("2026-01-{i:02}"), i as i64 * 100))
            .collect();
        let out = valuation_section(&vals, 2);
        assert!(out.contains("10 most recent"), "{out}");
        assert!(out.contains("older ones exist"), "{out}");
        assert_eq!(out.matches("manual").count(), RECENT_VALUATIONS);
    }

    /// Exactly the cap is not "more than the cap": claiming older rows exist when they do
    /// not would send a caller paging after nothing.
    #[test]
    fn a_history_that_exactly_fills_the_cap_does_not_claim_more() {
        let vals: Vec<Valuation> = (1..=RECENT_VALUATIONS)
            .map(|i| valuation(&format!("2026-01-{i:02}"), i as i64 * 100))
            .collect();
        let out = valuation_section(&vals, 2);
        assert!(!out.contains("older ones exist"), "{out}");
    }

    #[test]
    fn a_valuation_is_rendered_as_a_decimal() {
        let out = valuation_section(&[valuation("2026-01-01", 770_000_00)], 2);
        assert!(out.contains("770000.00"), "{out}");
        assert!(!out.contains("77000000"), "{out}");
    }
}
