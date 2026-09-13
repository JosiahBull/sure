//! Calendar arithmetic shared by the income payment schedule and the pay detector.
//!
//! These two lived in `crate::forecast` until the forecast engine was removed, and were reached
//! through `_pub` wrappers because the simulation owned them. Nothing about them is a projection:
//! they answer "what month is this date in" and "what is this date plus n months", which the
//! payment calendar needs in order to agree with itself. One copy, so two callers cannot drift
//! into two answers.

use chrono::{Datelike, NaiveDate};

use crate::reports;

/// Whole calendar months from `a` to `b`, ignoring day-of-month — negative when `b` precedes `a`.
pub(crate) fn months_between(a: NaiveDate, b: NaiveDate) -> i64 {
    (b.year() as i64 - a.year() as i64) * 12 + (b.month() as i64 - a.month() as i64)
}

/// `d` plus `n` calendar months, clamping the day-of-month to the target month's length
/// (matches `crons::period_date`'s convention for the same problem).
pub(crate) fn add_months(d: NaiveDate, n: i64) -> NaiveDate {
    let total = d.year() as i64 * 12 + (d.month() as i64 - 1) + n;
    let year = total.div_euclid(12) as i32;
    let month = total.rem_euclid(12) as u32 + 1;
    let day = d.day().min(reports::last_day_of_month(year, month).day());
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn months_between_counts_calendar_months_in_both_directions() {
        assert_eq!(months_between(d("2026-01-31"), d("2026-02-01")), 1);
        assert_eq!(months_between(d("2026-03-01"), d("2026-01-01")), -2);
        assert_eq!(months_between(d("2025-11-15"), d("2026-02-15")), 3);
    }

    /// The reason the clamp exists: stepping off a 31st into a shorter month must not overflow
    /// into the following one, which is the bug `crons::period_date` carries the same guard for.
    #[test]
    fn add_months_clamps_the_day_to_the_target_month() {
        assert_eq!(add_months(d("2026-01-31"), 1), d("2026-02-28"));
        assert_eq!(add_months(d("2026-01-31"), 3), d("2026-04-30"));
        assert_eq!(add_months(d("2026-03-15"), -3), d("2025-12-15"));
    }
}
