//! The exchange-rate [`ScheduledTask`]: pull fresh rates from the configured
//! [`ExchangeRateProvider`] and persist them to `exchange_rate_cache`. Persistence is
//! handled here, not in `sure-providers`, matching the split used for transaction
//! providers — the provider only fetches and normalizes. Scheduling (including
//! surviving process restarts without re-fetching early) is handled generically by
//! `sure-scheduler`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use sure_dal::Db;
use sure_providers::ExchangeRateProvider;
use sure_scheduler::ScheduledTask;

/// Free upstream sources refresh at most daily, and exact intraday accuracy isn't
/// needed here, so there's no value in polling more often than this.
const POLL_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub struct ExchangeRateTask {
    db: Db,
    provider: Arc<dyn ExchangeRateProvider>,
}

impl ExchangeRateTask {
    pub fn new(db: Db, provider: Arc<dyn ExchangeRateProvider>) -> Self {
        Self { db, provider }
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
        let base_code = sure_dal::settings::base_currency(&self.db).await?;
        let known_codes: HashSet<String> = sure_dal::currencies::list(&self.db)
            .await?
            .into_iter()
            .map(|c| c.code)
            .collect();

        let quotes = self.provider.fetch_rates(&base_code).await?;
        let mut stored = 0;
        for quote in quotes {
            // The upstream knows far more currencies than we track; only cache the
            // ones we actually have a `currencies` row for (the table's FK requires
            // it).
            if !known_codes.contains(&quote.quote_code) {
                continue;
            }
            sure_dal::exchange_rate_cache::upsert(
                &self.db,
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
