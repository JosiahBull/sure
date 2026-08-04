//! The exchange-rate [`ScheduledTask`]: pull fresh rates from the configured
//! [`ExchangeRateProvider`] and persist them to `exchange_rates` — the same table every
//! conversion reads through [`crate::fx::Fx`], so a poll actually moves reported figures.
//! (It used to write a separate latest-only cache that nothing read, leaving foreign-currency
//! amounts silently at parity; see `0018_fx_rates_single_table.sql`.) Persistence goes
//! through the [`ExchangeRateRepo`] port, not `sure-providers` — matching the split used
//! for transaction providers: the provider only fetches and normalizes. Scheduling
//! (including surviving process restarts without re-fetching early) is handled
//! generically by `sure-scheduler`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_scheduler::{ScheduledTask, TaskRun};
use tokio_util::sync::CancellationToken;

use crate::ports::{ExchangeRateProvider, ExchangeRateRepo};

/// Free upstream sources refresh at most daily, and exact intraday accuracy isn't
/// needed here, so there's no value in polling more often than this.
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The scheduler's key for this task in `scheduled_task_runs`. Public because config-snapshot
/// import has to clear this task's last-run row (an import can wipe the rates, and the
/// scheduler would otherwise wait out `POLL_INTERVAL` before refilling them) — and a
/// hand-copied string there would drift the day this task is renamed.
pub const TASK_NAME: &str = "exchange_rate_poll";

pub struct ExchangeRateTask {
    rates: Arc<dyn ExchangeRateRepo>,
    provider: Arc<dyn ExchangeRateProvider>,
}

impl ExchangeRateTask {
    pub fn new(rates: Arc<dyn ExchangeRateRepo>, provider: Arc<dyn ExchangeRateProvider>) -> Self {
        Self { rates, provider }
    }
}

#[async_trait]
impl ScheduledTask for ExchangeRateTask {
    fn name(&self) -> &'static str {
        TASK_NAME
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self, cancel: &CancellationToken) -> anyhow::Result<TaskRun> {
        let base_code = self.rates.base_currency().await?;
        let known_codes = self.rates.known_currency_codes().await?;

        let quotes = self.provider.fetch_rates(&base_code).await?;
        let mut stored = 0;
        for quote in quotes {
            // Between whole rates, never between the two halves of one upsert: stopping here
            // leaves the rates already written intact and the rest untouched, and the run
            // isn't recorded, so the next start refetches the lot.
            if cancel.is_cancelled() {
                tracing::debug!(base = %base_code, stored, "exchange-rate refresh stopped early for shutdown");
                return Ok(TaskRun::Interrupted);
            }
            // The upstream knows far more currencies than we track; only store the
            // ones we actually have a `currencies` row for (the table's FK requires
            // it).
            if !known_codes.contains(&quote.quote_code) {
                continue;
            }
            self.rates
                .upsert_rate(
                    &base_code,
                    &quote.quote_code,
                    &quote.as_of,
                    &quote.rate.to_string(),
                )
                .await?;
            stored += 1;
        }
        tracing::info!(base = %base_code, stored, "refreshed exchange rates");
        Ok(TaskRun::Completed)
    }
}
