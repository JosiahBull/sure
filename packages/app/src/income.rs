//! Turning a recorded income stream into something the monthly Monte Carlo loop can consume.
//!
//! Everything here is pure: a payment calendar and the tax scales that price it. The level
//! schedule and gross→net map that used to sit beside it had no caller once the forecast engine
//! was removed, and went with it; what survives is what the payment matcher reads.
//!
//! The awkward part, and the reason this is its own module, is that **a month is not a pay
//! period.** The simulation steps in calendar months; payroll does not. A fortnightly payer is
//! paid 26 times a year, which is not twice a month — three paydays land in some months, and
//! which months those are depends on the anchor date. Dividing an annual salary by twelve gets
//! the year right and every individual month wrong, which for a cash-flow projection is the
//! wrong trade: the question "can we afford this" is asked of months.

use chrono::{Datelike, NaiveDate};
use sure_core::{PayFrequency, ResolvedScale, StoredTaxScale, income::PayStep, tax};

/// The stored scales, resolved once per simulation into something the arithmetic can borrow.
///
/// Falls back to the built-in constants when the table is empty — which should not happen, since
/// `migrate` seeds it, but "every gross salary is suddenly untaxed" is too quiet a failure to leave
/// to that assumption.
pub(crate) struct TaxScales {
    resolved: Vec<(String, ResolvedScale)>,
}

impl TaxScales {
    pub(crate) fn new(stored: &[StoredTaxScale]) -> Self {
        let mut resolved: Vec<(String, ResolvedScale)> = stored
            .iter()
            .map(|s| (s.scale.effective_from.clone(), ResolvedScale::new(&s.scale)))
            .collect();
        if resolved.is_empty() {
            resolved = tax::builtin_scales(tax::TaxScaleId::NzPaye)
                .iter()
                .map(|s| (s.effective_from.clone(), ResolvedScale::new(s)))
                .collect();
        }
        resolved.sort_by(|a, b| a.0.cmp(&b.0));
        TaxScales { resolved }
    }

    /// The scale in force on `on` — the latest one not after it.
    ///
    /// `None` when every scale starts later than the date asked about: a question this table cannot
    /// answer, and answering it with rules that had not been written yet would be a guess dressed as
    /// a fact.
    pub(crate) fn at(&self, on: NaiveDate) -> Option<&ResolvedScale> {
        let iso = on.to_string();
        self.resolved
            .iter()
            .rev()
            .find(|(from, _)| from.as_str() <= iso.as_str())
            .map(|(_, s)| s)
    }
}

/// Every payday of `freq`, anchored at `anchor`, that lands inside `[from, to]` — ascending.
///
/// [`payment_counts`]'s other half: the same enumeration (jump to the window, then step), keeping
/// the dates the counts throw away, over an arbitrary date range rather than whole months from a
/// projection's `today`. The matcher lives on these dates; the simulation only ever needed the
/// counts.
///
/// Semi-monthly is the one frequency [`PayStep`] cannot express, and the one place this holds
/// calendar logic of its own: the recurring pay days are `{d, d + 14}` where `d` is the anchor's
/// day-of-month reduced below 15 (`day` if it is 1–14, else `day − 14`), the later one clamped to
/// short months. An anchor on the 14th pays the 14th and the 28th; one on the *28th* — which is
/// what "start from the next detected payday" naturally produces mid-cycle — pays the 28th and
/// then the 14th and 28th of every month after. Either way the anchor itself is the first
/// payment; unlike the special case `payment_counts` used to carry, months before it are not
/// paid, exactly as for every other frequency.
pub(crate) fn payment_dates(
    freq: PayFrequency,
    anchor: NaiveDate,
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<NaiveDate> {
    let mut dates = Vec::new();
    if to < from || to < anchor {
        return dates;
    }
    let from = from.max(anchor);

    if freq == PayFrequency::SemiMonthly {
        let day = anchor.day();
        let first = if day > 14 { day - 14 } else { day };
        let mut month_start = from.with_day(1).unwrap_or(from);
        while month_start <= to {
            let last =
                crate::reports::last_day_of_month_pub(month_start.year(), month_start.month());
            for pay_day in [first, first + 14] {
                if let Some(date) = month_start.with_day(pay_day.min(last.day()))
                    && date >= from
                    && date <= to
                {
                    dates.push(date);
                }
            }
            month_start = crate::dates::add_months(month_start, 1);
        }
        // Belt-and-braces against short-month clamping ever landing both days on one date; a
        // duplicate would promise two pays on one day.
        dates.dedup();
        return dates;
    }

    match freq.step() {
        PayStep::Days(n) => {
            // Jump straight to the first payment inside the window instead of walking from the
            // anchor: an anchor set years ago would otherwise iterate thousands of times per
            // stream, per request.
            let elapsed = (from - anchor).num_days();
            let k = if elapsed <= 0 {
                0
            } else {
                elapsed.div_euclid(n)
            };
            let mut date = anchor + chrono::Duration::days(k * n);
            while date < from {
                date += chrono::Duration::days(n);
            }
            while date <= to {
                dates.push(date);
                date += chrono::Duration::days(n);
            }
        }
        PayStep::Months(n) => {
            let elapsed = crate::dates::months_between(anchor, from);
            let k = if elapsed <= 0 {
                0
            } else {
                elapsed.div_euclid(n)
            };
            let mut step = k * n;
            let mut date = crate::dates::add_months(anchor, step);
            while date < from {
                step += n;
                date = crate::dates::add_months(anchor, step);
            }
            while date <= to {
                dates.push(date);
                step += n;
                date = crate::dates::add_months(anchor, step);
            }
        }
    }
    dates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The 14th-and-28th convention: a semi-monthly anchor names the first pay day, and the
    /// second is fourteen days after it, clamped to short months.
    #[test]
    fn semi_monthly_dates_are_the_anchor_day_and_fourteen_later() {
        let dates = payment_dates(
            PayFrequency::SemiMonthly,
            d("2026-01-14"),
            d("2026-01-01"),
            d("2026-03-31"),
        );
        assert_eq!(
            dates,
            vec![
                d("2026-01-14"),
                d("2026-01-28"),
                d("2026-02-14"),
                d("2026-02-28"),
                d("2026-03-14"),
                d("2026-03-28"),
            ]
        );
        // An anchor on the *second* pay day of the month — what "start from the next detected
        // payday" produces mid-cycle — still pays both days from the following month, and pays
        // nothing before itself.
        let mid_cycle = payment_dates(
            PayFrequency::SemiMonthly,
            d("2026-01-28"),
            d("2026-01-01"),
            d("2026-02-28"),
        );
        assert_eq!(
            mid_cycle,
            vec![d("2026-01-28"), d("2026-02-14"), d("2026-02-28")]
        );
    }

    /// A window that starts mid-stream picks up exactly the paydays inside it — the matcher asks
    /// this question for every sync.
    #[test]
    fn payment_dates_respect_an_arbitrary_window() {
        // Fortnightly from 2026-01-02: Jan 2/16/30, Feb 13/27, Mar 13/27 …
        let dates = payment_dates(
            PayFrequency::Fortnightly,
            d("2026-01-02"),
            d("2026-03-01"),
            d("2026-03-31"),
        );
        assert_eq!(dates, vec![d("2026-03-13"), d("2026-03-27")]);

        // A month-end monthly anchor clamps into February.
        let month_end = payment_dates(
            PayFrequency::Monthly,
            d("2026-01-31"),
            d("2026-02-01"),
            d("2026-02-28"),
        );
        assert_eq!(month_end, vec![d("2026-02-28")]);

        // An empty or pre-anchor window is empty, not an error.
        assert!(
            payment_dates(
                PayFrequency::Monthly,
                d("2026-06-01"),
                d("2026-01-01"),
                d("2026-05-31"),
            )
            .is_empty()
        );
    }
}
