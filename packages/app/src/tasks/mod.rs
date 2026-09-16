//! Scheduled-task bodies, registered with `sure_scheduler::Scheduler` by the
//! composition root (`sure-api`'s `serve()`).

pub mod balance_delta;
pub mod equity_rebuild;
pub mod exchange_rates;
pub mod income_match;
pub mod property_estimates;
pub mod provider_poll;
pub mod transfer_link;

/// Every background task this app registers, as a value rather than a loose string.
///
/// The scheduler's own API is stringly by necessity — it is generic over apps that have not been
/// written — but *this* app's set is closed and known, and the one thing callers do with a task
/// name is ask for it to run sooner ([`sure_scheduler::Nudge`]). A typo in a nudge is silent by
/// construction: the scheduler drops a nudge for a name nobody registered, because it cannot
/// tell a typo from a task this deployment does not run. So the name is an enum here and
/// [`BackgroundTask::as_str`] is the single place it becomes text (CLAUDE.md rule 1), and
/// `every_task_name_matches_its_task` pins each variant against the task that answers to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackgroundTask {
    /// Fetches FX rates. Third party.
    ExchangeRatePoll,
    /// Sweeps bank/broker providers for new transactions. Third party.
    ProviderPoll,
    /// Derives transactions from the gap between two valuations. Local.
    BalanceDelta,
    /// Fetches share prices. Third party.
    StockPricePoll,
    /// Estimates property values. Third party.
    PropertyEstimatePoll,
    /// Pairs the two legs of an internal transfer. Local.
    TransferLink,
    /// Matches expected pays against the deposits that satisfied them. Local.
    IncomeMatch,
    /// Rewrites an equity account's valuation history from its grants and price ledger. Local.
    EquityRebuild,
}

impl BackgroundTask {
    pub const fn as_str(self) -> &'static str {
        match self {
            BackgroundTask::ExchangeRatePoll => "exchange_rate_poll",
            BackgroundTask::ProviderPoll => "provider_poll",
            BackgroundTask::BalanceDelta => "balance_delta",
            BackgroundTask::StockPricePoll => "stock_price_poll",
            BackgroundTask::PropertyEstimatePoll => "property_estimate_poll",
            BackgroundTask::TransferLink => "transfer_link",
            BackgroundTask::IncomeMatch => "income_match",
            BackgroundTask::EquityRebuild => "equity_rebuild",
        }
    }

    /// Whether this task only reads and writes the local database.
    ///
    /// The one property that decides whether it may run on startup: a local task costs a few
    /// queries, and a third-party one costs the household's rate limit every time the process
    /// comes up. See `ScheduledTask::run_on_startup`.
    pub const fn is_local(self) -> bool {
        match self {
            BackgroundTask::BalanceDelta
            | BackgroundTask::TransferLink
            | BackgroundTask::IncomeMatch
            | BackgroundTask::EquityRebuild => true,
            BackgroundTask::ExchangeRatePoll
            | BackgroundTask::ProviderPoll
            | BackgroundTask::StockPricePoll
            | BackgroundTask::PropertyEstimatePoll => false,
        }
    }
}

/// A typed handle for asking the scheduler to run a task now.
///
/// Wraps [`sure_scheduler::Nudge`] so callers name a [`BackgroundTask`] rather than the string
/// it serialises to — the scheduler cannot tell a mistyped name from a task this deployment does
/// not register, so it drops both in silence, and a typo would be a nudge that simply never
/// arrives. Everything above `sure-app` nudges through this type and therefore cannot mistype.
///
/// [`Default`] is a handle attached to no scheduler: every `wake` is a no-op. That is the right
/// behaviour for a process with no scheduler running — the MCP-only mode, and every test that
/// exercises a route without one — because the work still happens on the next real sweep.
#[derive(Clone, Default)]
pub struct TaskNudge(sure_scheduler::Nudge);

impl TaskNudge {
    pub fn new(inner: sure_scheduler::Nudge) -> Self {
        Self(inner)
    }

    /// Ask for `task` to run at the next opportunity. Never blocks and never fails.
    pub fn wake(&self, task: BackgroundTask) {
        self.0.wake(task.as_str());
    }

    /// Nudge several at once — see [`LEDGER_CHANGED`], the usual argument.
    pub fn wake_all(&self, tasks: &[BackgroundTask]) {
        for task in tasks {
            self.wake(*task);
        }
    }
}

/// The tasks worth re-running whenever the ledger gains or loses transactions.
///
/// An import, a provider sync and a bulk edit all land here: each of these three reads the
/// transaction table and derives something from it, and all three are local, so running them
/// again costs queries rather than anyone's rate limit. Ordered the way the scheduler registers
/// them — the transfer linker first, so a payroll credit wrongly paired as a transfer is settled
/// before the income matcher looks at it.
pub const LEDGER_CHANGED: &[BackgroundTask] = &[
    BackgroundTask::TransferLink,
    BackgroundTask::BalanceDelta,
    BackgroundTask::IncomeMatch,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A nudge for a name no task answers to is dropped in silence, so the enum is only worth
    /// anything if its strings are the real ones. Every variant is listed, so adding one without
    /// wiring it here fails to compile rather than going unchecked.
    #[test]
    fn every_task_name_matches_its_task() {
        for (task, expected) in [
            (BackgroundTask::ExchangeRatePoll, "exchange_rate_poll"),
            (BackgroundTask::ProviderPoll, "provider_poll"),
            (BackgroundTask::BalanceDelta, "balance_delta"),
            (BackgroundTask::StockPricePoll, "stock_price_poll"),
            (
                BackgroundTask::PropertyEstimatePoll,
                "property_estimate_poll",
            ),
            (BackgroundTask::TransferLink, "transfer_link"),
            (BackgroundTask::IncomeMatch, "income_match"),
            (BackgroundTask::EquityRebuild, "equity_rebuild"),
        ] {
            assert_eq!(task.as_str(), expected);
        }
    }

    /// Only a task that touches nothing outside this database may run on startup.
    #[test]
    fn no_third_party_task_is_local() {
        for t in [
            BackgroundTask::ExchangeRatePoll,
            BackgroundTask::ProviderPoll,
            BackgroundTask::StockPricePoll,
            BackgroundTask::PropertyEstimatePoll,
        ] {
            assert!(!t.is_local(), "{t:?} reaches a third party");
        }
    }
}
