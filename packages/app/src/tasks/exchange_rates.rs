//! The exchange-rate [`ScheduledTask`]: pull fresh rates from the configured
//! [`ExchangeRateProvider`] and persist them to `exchange_rate_cache`. Persistence goes
//! through the [`ExchangeRateRepo`] port, not `sure-providers` — matching the split used
//! for transaction providers: the provider only fetches and normalizes. Scheduling
//! (including surviving process restarts without re-fetching early) is handled
//! generically by `sure-scheduler`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_scheduler::ScheduledTask;

use crate::ports::{ExchangeRateProvider, ExchangeRateRepo};

/// Free upstream sources refresh at most daily, and exact intraday accuracy isn't
/// needed here, so there's no value in polling more often than this.
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

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
        "exchange_rate_poll"
    }

    fn interval(&self) -> Duration {
        POLL_INTERVAL
    }

    async fn run(&self) -> anyhow::Result<()> {
        let base_code = self.rates.base_currency().await?;
        let known_codes = self.rates.known_currency_codes().await?;

        let quotes = self.provider.fetch_rates(&base_code).await?;
        let mut stored = 0;
        for quote in quotes {
            // The upstream knows far more currencies than we track; only cache the
            // ones we actually have a `currencies` row for (the table's FK requires
            // it).
            if !known_codes.contains(&quote.quote_code) {
                continue;
            }
            self.rates
                .upsert_rate(
                    &base_code,
                    &quote.quote_code,
                    &quote.rate.to_string(),
                    &quote.as_of,
                )
                .await?;
            stored += 1;
        }
        tracing::info!(base = %base_code, stored, "refreshed exchange rates");
        Ok(())
    }
}
