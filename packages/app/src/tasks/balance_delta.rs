//! Derive a ledger for an account whose upstream only reports a balance.
//!
//! Some upstream accounts are balance-only: Akahu exposes an IR student loan's balance but
//! no transactions for it, so [`crate::sync::SyncService`] records a daily
//! `source='provider'` valuation and the account ends up with an accurate balance and an
//! empty transaction list. Net worth is right — [`crate::reports::account_value_at`] reads
//! the valuations — but nothing shows *what moved*, so a fortnightly repayment or a
//! living-costs drawdown is invisible.
//!
//! This task fills that in by differencing consecutive provider valuations and importing
//! each non-zero change as a transaction, through the same idempotent
//! [`ProviderRepo::import_transactions`] path a real provider's sync uses. Three properties
//! make it safe to run unattended:
//!
//! * **The sign is right by construction.** Valuations and transactions share one signed
//!   convention (liabilities negative), so a $400.00 repayment moving the balance from
//!   −30,000.00 to −29,600.00 differences to `+400_00` — already what Sure means by a
//!   repayment on a liability. Nothing has to be flipped, unlike a myIR CSV import.
//! * **Only closed days are derived.** `upsert_from_provider` *rewrites* today's valuation
//!   on every poll, so a delta computed against today would be stale the moment the next
//!   poll landed — and `INSERT OR IGNORE` would never correct it. Excluding today costs a
//!   day of lag and removes the whole class of bug.
//! * **Failure is cosmetic.** The balance comes from the valuations, so a missed or
//!   duplicated derived transaction cannot move net worth; it only makes the ledger less
//!   tidy.
//!
//! Opt in per connection, because a provider that *does* return real transactions (Akahu
//! for a mortgage or an everyday account) would otherwise have every movement counted
//! twice:
//!
//! ```json
//! { "external_account_id": "acc_…",
//!   "derive_transactions_from_balance": true,
//!   "derive_from": "2026-07-31" }
//! ```
//!
//! `derive_from` is the seam against a historical import: rows before it belong to
//! whatever backfilled the account (e.g. a myIR export through the `csv` provider), rows
//! from it onward belong to this task. Leave it unset to derive the whole series.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::Value;
use sure_core::{AccountClass, AppResult, Provider, Valuation, ValuationSource};
use sure_scheduler::ScheduledTask;

use crate::ports::{AccountRepo, Clock, ImportRow, ProviderRepo, ValuationRepo};
use crate::reports::parse_date;

/// A balance-only upstream is polled a few times a day at most, and only *closed* days are
/// derived, so there is nothing to gain from running more often than daily.
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// `providers.config` key that opts a connection into deriving.
const OPT_IN_KEY: &str = "derive_transactions_from_balance";
/// `providers.config` key holding the first date to derive (exclusive of anything before).
const FROM_KEY: &str = "derive_from";

pub struct BalanceDeltaTask {
    providers: Arc<dyn ProviderRepo>,
    accounts: Arc<dyn AccountRepo>,
    valuations: Arc<dyn ValuationRepo>,
    clock: Arc<dyn Clock>,
}

impl BalanceDeltaTask {
    pub fn new(
        providers: Arc<dyn ProviderRepo>,
        accounts: Arc<dyn AccountRepo>,
        valuations: Arc<dyn ValuationRepo>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            providers,
            accounts,
            valuations,
            clock,
        }
    }

    async fn derive_for(&self, provider: &Provider, today: NaiveDate) -> AppResult<()> {
        let account = self.accounts.get(provider.account_id).await?;
        let valuations = self
            .valuations
            .list_for_account(provider.account_id)
            .await?;
        let rows = derive_rows(
            &valuations,
            derive_from(&provider.config),
            today,
            account.kind.class(),
        );
        if rows.is_empty() {
            return Ok(());
        }

        let currency = self.providers.account_currency(provider.account_id).await?;
        let tag = format!("balance-delta#{}", provider.id);
        let (imported, skipped) = self
            .providers
            .import_transactions(provider.account_id, &currency, &tag, &rows)
            .await?;
        if imported > 0 {
            tracing::info!(
                provider = %provider.name,
                imported,
                skipped,
                "derived transactions from balance movements"
            );
        }
        Ok(())
    }
}

#[async_trait]
impl ScheduledTask for BalanceDeltaTask {
    fn name(&self) -> &'static str {
        "balance_delta"
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self) -> anyhow::Result<()> {
        let today = self.clock.today();
        for provider in self.providers.list().await? {
            if !provider.enabled || !is_opted_in(&provider.config) {
                continue;
            }
            // One misconfigured connection shouldn't stop the others being derived; the
            // work is idempotent, so the next run retries it anyway.
            if let Err(e) = self.derive_for(&provider, today).await {
                tracing::warn!(
                    provider = %provider.name,
                    error = %e,
                    "could not derive balance movements"
                );
            }
        }
        Ok(())
    }
}

fn is_opted_in(config: &Value) -> bool {
    config
        .get(OPT_IN_KEY)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn derive_from(config: &Value) -> Option<NaiveDate> {
    config
        .get(FROM_KEY)
        .and_then(Value::as_str)
        .and_then(parse_date)
}

/// Which way a movement reads in the ledger.
fn describe(delta_minor: i64, class: AccountClass) -> &'static str {
    match class {
        // A liability's stored balance is negative, so a *positive* movement is the debt
        // shrinking — the opposite of what the same sign means on anything else.
        AccountClass::Liability => {
            if delta_minor > 0 {
                "Repayment (derived)"
            } else {
                "Drawdown or fee (derived)"
            }
        }
        AccountClass::Cash | AccountClass::Asset | AccountClass::Investment => {
            if delta_minor > 0 {
                "Balance increase (derived)"
            } else {
                "Balance decrease (derived)"
            }
        }
    }
}

/// Difference consecutive provider valuations into importable rows.
///
/// Split out from the task so the rules that matter — sign, which days are eligible, and
/// how ids are formed — are testable without a database.
///
/// The `external_id` names the pair it came from (`2026-07-30..2026-07-31`), so re-deriving
/// the same series yields byte-identical ids and the unique `(provider, external_id)` index
/// absorbs the repeat. That holds because provider valuations are only ever written for
/// "today" (see `SyncService::sync_provider`), so the series only ever grows forward and an
/// already-derived pair can't be split by a later arrival.
pub(crate) fn derive_rows(
    valuations: &[Valuation],
    derive_from: Option<NaiveDate>,
    today: NaiveDate,
    class: AccountClass,
) -> Vec<ImportRow> {
    // Only the provider's own series: a manual or cron valuation on the same account
    // describes a different opinion of the balance, and differencing across the two would
    // invent movements that never happened.
    let mut series: Vec<(NaiveDate, i64, &str)> = valuations
        .iter()
        .filter(|v| v.source == ValuationSource::Provider)
        .filter_map(|v| parse_date(&v.as_of).map(|d| (d, v.value_minor, v.currency_code.as_str())))
        .collect();
    series.sort_by_key(|(day, _, _)| *day);

    let mut rows = Vec::new();
    for pair in series.windows(2) {
        let (previous_day, previous_minor, _) = pair[0];
        let (day, minor, currency) = pair[1];

        // Today's valuation is still being rewritten by the poll; leave it until it closes.
        if day >= today {
            continue;
        }
        if derive_from.is_some_and(|from| day < from) {
            continue;
        }
        let delta_minor = minor - previous_minor;
        if delta_minor == 0 {
            continue;
        }

        rows.push(ImportRow {
            external_id: format!("{previous_day}..{day}"),
            // Midday UTC to match the rest of this database's `posted_at` values, so a
            // derived row sorts sensibly against imported ones on the same day.
            posted_at: format!("{day}T12:00:00+00:00"),
            amount_minor: delta_minor,
            currency_code: Some(currency.to_string()),
            description: describe(delta_minor, class).to_string(),
            merchant: None,
            category_name: None,
            category_group: None,
            category_kind: None,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn valuation(as_of: &str, value_minor: i64, source: ValuationSource) -> Valuation {
        Valuation {
            id: 0,
            account_id: 6,
            as_of: as_of.to_string(),
            value_minor,
            currency_code: "NZD".to_string(),
            source,
            note: None,
            created_at: String::new(),
        }
    }

    fn provider_valuation(as_of: &str, value_minor: i64) -> Valuation {
        valuation(as_of, value_minor, ValuationSource::Provider)
    }

    fn liability(valuations: &[Valuation], today: &str) -> Vec<ImportRow> {
        derive_rows(valuations, None, d(today), AccountClass::Liability)
    }

    /// The headline case: a fortnightly PAYE repayment shrinks the debt, and on a liability
    /// that has to come out positive — the same sign a myIR import has to be flipped into.
    #[test]
    fn a_repayment_on_a_liability_derives_a_positive_transaction() {
        let rows = liability(
            &[
                provider_valuation("2026-08-01", -30_000_00),
                provider_valuation("2026-08-02", -29_600_00),
            ],
            "2026-08-03",
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount_minor, 400_00);
        assert_eq!(rows[0].description, "Repayment (derived)");
        assert_eq!(rows[0].posted_at, "2026-08-02T12:00:00+00:00");
        assert_eq!(rows[0].external_id, "2026-08-01..2026-08-02");
    }

    /// A living-costs drawdown or an administration fee grows the debt, which is a negative
    /// movement on a liability.
    #[test]
    fn a_drawdown_on_a_liability_derives_a_negative_transaction() {
        let rows = liability(
            &[
                provider_valuation("2026-08-01", -30_000_00),
                provider_valuation("2026-08-02", -30_350_00),
            ],
            "2026-08-03",
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount_minor, -350_00);
        assert_eq!(rows[0].description, "Drawdown or fee (derived)");
    }

    /// Most days nothing happens, and a $0 transaction is noise.
    #[test]
    fn an_unchanged_balance_derives_nothing() {
        let rows = liability(
            &[
                provider_valuation("2026-08-01", -30_000_00),
                provider_valuation("2026-08-02", -30_000_00),
                provider_valuation("2026-08-03", -30_000_00),
            ],
            "2026-08-04",
        );

        assert!(rows.is_empty());
    }

    /// Today's valuation is rewritten by every poll, so the pair ending on it must wait —
    /// but the pair before it is closed and derives now.
    #[test]
    fn the_pair_ending_today_is_left_until_it_closes() {
        let valuations = [
            provider_valuation("2026-08-01", -30_000_00),
            provider_valuation("2026-08-02", -29_600_00),
            provider_valuation("2026-08-03", -29_000_00),
        ];

        let rows = liability(&valuations, "2026-08-03");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].external_id, "2026-08-01..2026-08-02");

        // A day later the same series yields the second pair as well, with the first
        // unchanged — so the deferred movement is never lost.
        let rows = liability(&valuations, "2026-08-04");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].external_id, "2026-08-02..2026-08-03");
        assert_eq!(rows[1].amount_minor, 600_00);
    }

    /// If the poll misses days (process down, upstream flaking), the movement is still
    /// captured — as one delta spanning the gap, posted on the day the balance reappeared.
    #[test]
    fn a_gap_in_the_series_yields_one_spanning_delta() {
        let rows = liability(
            &[
                provider_valuation("2026-08-01", -30_000_00),
                provider_valuation("2026-08-09", -29_000_00),
            ],
            "2026-08-10",
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount_minor, 1_000_00);
        assert_eq!(rows[0].external_id, "2026-08-01..2026-08-09");
        assert_eq!(rows[0].posted_at, "2026-08-09T12:00:00+00:00");
    }

    /// The seam against a historical import: days the backfill already covers are its
    /// business, not this task's, or the two would both post the same movement.
    #[test]
    fn days_before_the_cutover_are_left_to_the_importer() {
        let valuations = [
            provider_valuation("2026-07-29", -31_000_00),
            provider_valuation("2026-07-30", -30_000_00),
            provider_valuation("2026-07-31", -29_600_00),
        ];

        let rows = derive_rows(
            &valuations,
            Some(d("2026-07-31")),
            d("2026-08-01"),
            AccountClass::Liability,
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].external_id, "2026-07-30..2026-07-31");
    }

    /// A manual or cron valuation is a different opinion of the balance; differencing
    /// across the two sources would invent movements that never happened.
    #[test]
    fn only_provider_valuations_are_differenced() {
        let rows = liability(
            &[
                provider_valuation("2026-08-01", -30_000_00),
                valuation("2026-08-02", -12_345_00, ValuationSource::Manual),
                valuation("2026-08-02", -99_999_00, ValuationSource::Cron),
                provider_valuation("2026-08-03", -29_600_00),
            ],
            "2026-08-04",
        );

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount_minor, 400_00);
        assert_eq!(rows[0].external_id, "2026-08-01..2026-08-03");
    }

    /// `list_for_account` returns newest-first, and a restore or snapshot import can land
    /// rows in any order; the pairing must follow the calendar, not the row order.
    #[test]
    fn an_out_of_order_series_is_differenced_by_date() {
        let rows = liability(
            &[
                provider_valuation("2026-08-03", -29_000_00),
                provider_valuation("2026-08-01", -30_000_00),
                provider_valuation("2026-08-02", -29_600_00),
            ],
            "2026-08-04",
        );

        assert_eq!(
            rows.iter().map(|r| r.amount_minor).collect::<Vec<_>>(),
            [400_00, 600_00]
        );
    }

    /// Re-deriving must produce the same ids, or the unique `(provider, external_id)` index
    /// can't absorb the repeat and every run would duplicate the whole ledger.
    #[test]
    fn re_deriving_the_same_series_produces_identical_ids() {
        let valuations = [
            provider_valuation("2026-08-01", -30_000_00),
            provider_valuation("2026-08-02", -29_600_00),
            provider_valuation("2026-08-05", -29_000_00),
        ];

        let first = liability(&valuations, "2026-08-06");
        let second = liability(&valuations, "2026-08-06");

        assert_eq!(
            first.iter().map(|r| &r.external_id).collect::<Vec<_>>(),
            second.iter().map(|r| &r.external_id).collect::<Vec<_>>()
        );
        assert_eq!(first.len(), 2);
    }

    /// An asset's balance moving up isn't a "repayment"; only liabilities get the debt
    /// wording.
    #[test]
    fn non_liability_accounts_get_neutral_wording() {
        let valuations = [
            provider_valuation("2026-08-01", 10_000_00),
            provider_valuation("2026-08-02", 10_500_00),
        ];

        let rows = derive_rows(&valuations, None, d("2026-08-03"), AccountClass::Cash);
        assert_eq!(rows[0].description, "Balance increase (derived)");
    }

    #[test]
    fn opting_in_requires_the_explicit_flag() {
        assert!(!is_opted_in(&serde_json::json!({})));
        assert!(!is_opted_in(
            &serde_json::json!({ "external_account_id": "acc_1" })
        ));
        assert!(!is_opted_in(
            &serde_json::json!({ "derive_transactions_from_balance": false })
        ));
        assert!(is_opted_in(
            &serde_json::json!({ "derive_transactions_from_balance": true })
        ));
    }

    #[test]
    fn a_missing_or_unreadable_cutover_means_derive_everything() {
        assert_eq!(derive_from(&serde_json::json!({})), None);
        assert_eq!(derive_from(&serde_json::json!({ "derive_from": "" })), None);
        assert_eq!(
            derive_from(&serde_json::json!({ "derive_from": "2026-07-31" })),
            Some(d("2026-07-31"))
        );
    }
}
