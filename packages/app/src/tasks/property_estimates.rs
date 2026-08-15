//! The property-estimate [`ScheduledTask`]: once a month, ask the configured
//! [`PropertyEstimateProvider`] what each opted-in property is worth and record the answer as a
//! `source='estimate'` valuation — the same series a property account's net worth is read from,
//! so a poll actually moves the reported figure.
//!
//! Persistence goes through the [`ValuationRepo`] / [`AccountRepo`] ports, not
//! `sure-providers` — matching the split used for exchange rates and stock prices: the provider
//! only fetches and normalises.
//!
//! **Opt-in, one account at a time.** The subscription lives on the account
//! ([`sure_core::HousePricerLink`]) and is written by the API's pre-flight flow, never by this
//! task: an address is personal data, and nothing here may send one to a third party that the
//! person has not explicitly confirmed against that exact upstream match. An account with no
//! link is not polled, and archiving one stops the polling (see
//! `sure_dal::accounts::list_house_pricer_subscriptions`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_scheduler::{ScheduledTask, TaskRun};
use tokio_util::sync::CancellationToken;

use crate::ports::{
    AccountRepo, Clock, HousePricerSubscription, PropertyEstimate, PropertyEstimateProvider,
    ValuationRepo,
};

/// Once a month, as asked for — and as much as the upstream can justify: an automated valuation
/// model is rebuilt from council sale records and territorial-authority revaluations, which move
/// on a scale of months, not days. Polling weekly would be four times the traffic for the same
/// number.
///
/// 30 days rather than a calendar month because [`ScheduledTask::interval`] is a [`Duration`]:
/// the scheduler measures elapsed time since the last *completed* run, so this lands on a slowly
/// drifting day of the month rather than a fixed one. Harmless here — the valuation is stamped
/// with the day it was fetched, and nothing downstream expects one row per calendar month.
const POLL_INTERVAL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The scheduler's key for this task in `scheduled_task_runs`.
///
/// Public for the same reason [`crate::tasks::exchange_rates::TASK_NAME`] is: a config-snapshot
/// import can wipe the valuations this task writes, and clearing its last-run row is what makes
/// the next check refill them instead of waiting out a month. A hand-copied string there would
/// drift the day this task is renamed.
pub const TASK_NAME: &str = "property_estimate_poll";

/// Prefix on the note every estimate valuation carries, so where a figure came from is legible
/// in the account's valuation list without cross-referencing the provider by name.
pub const NOTE_PREFIX: &str = "House Pricer estimate";

pub struct PropertyEstimateTask {
    accounts: Arc<dyn AccountRepo>,
    valuations: Arc<dyn ValuationRepo>,
    clock: Arc<dyn Clock>,
    provider: Arc<dyn PropertyEstimateProvider>,
}

impl PropertyEstimateTask {
    pub fn new(
        accounts: Arc<dyn AccountRepo>,
        valuations: Arc<dyn ValuationRepo>,
        clock: Arc<dyn Clock>,
        provider: Arc<dyn PropertyEstimateProvider>,
    ) -> Self {
        Self {
            accounts,
            valuations,
            clock,
            provider,
        }
    }
}

/// What the poll does with one answer the upstream gave, decided before anything is written.
///
/// A returned value rather than a branch inside the sweep so the two refusals below are testable
/// without a database, a socket, or a fake for either ~40-method repo port — the same reason
/// `tasks::balance_delta` puts its decisions in `derive_rows`.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// The match is the property this subscription pinned and the currency agrees: write it.
    Record { value_minor: i64, note: String },
    /// Leave whatever is already recorded alone, for this reason.
    Skip(SkipReason),
}

/// Why an answer was not recorded. Named variants rather than a bare `bool`/`None` so
/// [`PropertyEstimateTask::run`] must say something specific in the log about each — an operator
/// reading "skipped" learns nothing, and these two mean very different things.
#[derive(Debug, PartialEq, Eq)]
enum SkipReason {
    /// The saved query now resolves to a different property than the one confirmed at opt-in.
    DifferentProperty {
        matched: String,
        matched_address: String,
    },
    /// The upstream quoted a currency the account isn't denominated in.
    CurrencyMismatch { estimate_currency: String },
}

/// Decide what to do with `estimate` for `sub`.
///
/// Both refusals are deliberate and neither is theoretical-only:
///
/// * **A different property.** `q` is a fuzzy address match, so an upstream re-index can quietly
///   start resolving the saved query to the *neighbouring* house. Writing that would restate
///   this property's worth as that one's, and nothing downstream could tell. Refusing costs one
///   month's figure; the person re-confirms through the pre-flight. This is the whole reason the
///   subscription pins an id rather than just a query.
/// * **A different currency.** This task has no FX in reach, and the alternative — booking an
///   NZD estimate against, say, an AUD-denominated account — is a wrong number that looks
///   entirely plausible. Unreachable in practice while the feed covers one New Zealand city,
///   which is exactly how a parity bug gets written.
fn decide(sub: &HousePricerSubscription, estimate: &PropertyEstimate) -> Decision {
    if estimate.property_id != sub.link.property_id {
        return Decision::Skip(SkipReason::DifferentProperty {
            matched: estimate.property_id.clone(),
            matched_address: estimate.matched_address.clone(),
        });
    }
    if !estimate
        .currency_code
        .eq_ignore_ascii_case(&sub.currency_code)
    {
        return Decision::Skip(SkipReason::CurrencyMismatch {
            estimate_currency: estimate.currency_code.clone(),
        });
    }
    Decision::Record {
        value_minor: estimate.value_minor,
        note: format!("{NOTE_PREFIX} ({})", estimate.model_note),
    }
}

#[async_trait]
impl ScheduledTask for PropertyEstimateTask {
    fn name(&self) -> &'static str {
        TASK_NAME
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
        let subscriptions = self.accounts.list_house_pricer_subscriptions().await?;
        // Cheap, and it keeps the common case — nobody has opted in — from logging a sweep that
        // did nothing every month.
        if subscriptions.is_empty() {
            return Ok(TaskRun::Completed);
        }
        let today = self.clock.today().to_string();

        let mut recorded = 0;
        let mut skipped = 0;
        for sub in &subscriptions {
            // Between whole accounts, never between a fetch and its write: stopping here leaves
            // the valuations already written intact, and an interrupted run isn't recorded, so
            // the next start sweeps the lot again.
            if cancel.is_cancelled() {
                tracing::debug!(
                    recorded,
                    skipped,
                    "property estimate refresh stopped early for shutdown"
                );
                return Ok(TaskRun::Interrupted);
            }

            let estimate = match self.provider.fetch_estimate(&sub.link.query).await {
                Ok(Some(estimate)) => estimate,
                // The upstream no longer matches an address it matched at opt-in time — a
                // re-index, or a property taken out of the data set. Warned rather than
                // errored: one unmatchable address must not stop the accounts after it, and
                // there is nothing a retry would fix.
                Ok(None) => {
                    tracing::warn!(
                        account = %sub.account_name,
                        "no property matched this account's saved House Pricer address; \
                         re-check it on the account to re-subscribe"
                    );
                    skipped += 1;
                    continue;
                }
                // One unreachable or unintelligible fetch shouldn't block the rest — the same
                // per-item resilience the stock-price poll needs and the exchange-rate poll
                // (one call for every currency) does not.
                Err(err) => {
                    tracing::warn!(
                        account = %sub.account_name,
                        error = %err,
                        "failed to fetch a property estimate"
                    );
                    skipped += 1;
                    continue;
                }
            };

            match decide(sub, &estimate) {
                Decision::Record { value_minor, note } => {
                    self.valuations
                        .upsert_from_estimate(
                            sub.account_id,
                            &today,
                            value_minor,
                            &estimate.currency_code,
                            &note,
                        )
                        .await?;
                    recorded += 1;
                }
                Decision::Skip(SkipReason::DifferentProperty {
                    matched,
                    matched_address,
                }) => {
                    tracing::warn!(
                        account = %sub.account_name,
                        expected = %sub.link.property_id,
                        %matched,
                        %matched_address,
                        "the saved House Pricer address now matches a different property; \
                         skipping rather than recording an estimate for the wrong house"
                    );
                    skipped += 1;
                }
                Decision::Skip(SkipReason::CurrencyMismatch { estimate_currency }) => {
                    tracing::warn!(
                        account = %sub.account_name,
                        account_currency = %sub.currency_code,
                        %estimate_currency,
                        "House Pricer quoted a property in a different currency than the \
                         account; skipping rather than recording it at parity"
                    );
                    skipped += 1;
                }
            }
        }

        tracing::info!(recorded, skipped, "refreshed property estimates");
        Ok(TaskRun::Completed)
    }
}

#[cfg(test)]
mod tests {
    use sure_core::HousePricerLink;

    use super::*;

    /// A property id that could not collide with a real `unitOfPropertyId`, and an invented
    /// address — no fixture in this repo carries a real one (CLAUDE.md rule 3).
    const PROPERTY_ID: &str = "00000000-0000-4000-8000-000000000001";
    const MATCHED: &str = "123 kowhai street, riccarton";

    fn subscription(currency: &str) -> HousePricerSubscription {
        HousePricerSubscription {
            account_id: 7,
            account_name: "Home".into(),
            currency_code: currency.into(),
            link: HousePricerLink {
                query: "123 kowhai street riccarton".into(),
                property_id: PROPERTY_ID.into(),
                matched_address: MATCHED.into(),
            },
        }
    }

    fn estimate(property_id: &str, currency: &str) -> PropertyEstimate {
        PropertyEstimate {
            property_id: property_id.into(),
            matched_address: MATCHED.into(),
            value_minor: 650_000_00,
            currency_code: currency.into(),
            model_note: "model A 650000, model B 598000".into(),
        }
    }

    #[test]
    fn records_a_matching_estimate_with_both_models_in_the_note() {
        let decision = decide(&subscription("NZD"), &estimate(PROPERTY_ID, "NZD"));
        assert_eq!(
            decision,
            Decision::Record {
                value_minor: 650_000_00,
                // Model A is the recorded figure; model B rides along so the ~8% spread between
                // two undocumented models stays visible on the valuation itself.
                note: "House Pricer estimate (model A 650000, model B 598000)".into(),
            }
        );
    }

    /// The guard that matters most: a fuzzy `q` re-resolving to the neighbouring house must not
    /// silently restate this property's worth as that one's.
    #[test]
    fn refuses_an_estimate_for_a_different_property() {
        let other = "00000000-0000-4000-8000-0000000000ff";
        let decision = decide(&subscription("NZD"), &estimate(other, "NZD"));
        assert_eq!(
            decision,
            Decision::Skip(SkipReason::DifferentProperty {
                matched: other.into(),
                matched_address: MATCHED.into(),
            })
        );
    }

    #[test]
    fn refuses_an_estimate_quoted_in_another_currency() {
        // An AUD-denominated property account: recording an NZD figure against it at parity is
        // a wrong number that looks right.
        let decision = decide(&subscription("AUD"), &estimate(PROPERTY_ID, "NZD"));
        assert_eq!(
            decision,
            Decision::Skip(SkipReason::CurrencyMismatch {
                estimate_currency: "NZD".into(),
            })
        );
    }

    #[test]
    fn currency_matching_ignores_case() {
        // `currencies.code` is upper-case by convention, but the comparison is the one thing
        // standing between a real mismatch and a parity bug — it should not turn on casing.
        assert!(matches!(
            decide(&subscription("nzd"), &estimate(PROPERTY_ID, "NZD")),
            Decision::Record { .. }
        ));
    }

    #[test]
    fn polls_monthly() {
        // The interval the feature was asked for. Pinned because it is the one number a
        // refactor could quietly turn into "daily" with nothing else noticing.
        assert_eq!(POLL_INTERVAL, Duration::from_secs(30 * 24 * 60 * 60));
    }
}
