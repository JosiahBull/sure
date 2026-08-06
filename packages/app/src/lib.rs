// Money is stored in minor units and written with a `dollars_cents` digit grouping
// (e.g. `485_000_00` == $485,000.00), which reads far better than 3-digit groups for
// financial values; clippy's grouping lint fights it, so allow it crate-wide.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod brokerage;
pub mod detect;
pub mod forecast;
pub mod fx;
pub mod import;
pub mod income;
pub mod ports;
pub mod reports;
pub mod rules;
pub mod stock_prices;
pub mod sync;
pub mod tasks;

pub use ports::{Clock, SystemClock};

/// A fixed [`Clock`] shared by every service's unit tests, so day-by-day logic
/// (backfills, report windows, poll tasks) is deterministic without a real wall clock.
#[cfg(test)]
pub(crate) mod test_clock {
    use chrono::{DateTime, NaiveDate, Utc};

    use crate::ports::Clock;

    pub struct FixedClock(pub NaiveDate);

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            DateTime::<Utc>::from_naive_utc_and_offset(self.0.and_hms_opt(0, 0, 0).unwrap(), Utc)
        }
        fn today(&self) -> NaiveDate {
            self.0
        }
    }
}
