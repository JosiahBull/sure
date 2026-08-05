//! Turning a recorded income stream into something the monthly Monte Carlo loop can consume.
//!
//! Everything here is pure and path-invariant: a payment calendar, a level schedule, and a
//! gross→net map. The per-path parts (a promotion moving the level, a career break scaling the
//! payout) live in `crate::forecast`, because only the simulation knows which path it is on.
//!
//! The awkward part, and the reason this is its own module, is that **a month is not a pay
//! period.** The simulation steps in calendar months; payroll does not. A fortnightly payer is
//! paid 26 times a year, which is not twice a month — three paydays land in some months, and
//! which months those are depends on the anchor date. Dividing an annual salary by twelve gets
//! the year right and every individual month wrong, which for a cash-flow projection is the
//! wrong trade: the question "can we afford this" is asked of months.

use chrono::{Datelike, NaiveDate};
use sure_core::{
    income::PayStep, tax, IncomeBasis, IncomeStream, PayFrequency, ResolvedScale, StoredTaxScale,
    TakeHome, TakeHomeSource,
};

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

/// How many paydays land in each month index `0..=horizon`, counting from `today`.
///
/// Index 0 is always 0: the loop runs `1..=horizon`, and the current month is already inside the
/// history the projection starts from — crediting it would pay this month twice.
///
/// Payments are *enumerated* from the anchor rather than derived from a rate, which is what gets
/// the two hard cases right. A four-weekly stream pays thirteen times in 364 days, so its anchor
/// walks a day earlier each year and the extra payday eventually lands in a different month; and a
/// monthly stream anchored on the 31st pays on the 30th in April, via the same day-of-month
/// clamping `add_months` and `crons::period_date` already use.
pub(crate) fn payment_counts(
    freq: PayFrequency,
    anchor: NaiveDate,
    today: NaiveDate,
    horizon: i64,
) -> Vec<u8> {
    let mut counts = vec![0u8; (horizon + 1) as usize];
    // The window is whole calendar months: the first day of month 1 to the last day of month
    // `horizon`. Both ends matter and they have to agree. Anchoring the end on today's
    // day-of-month instead — the obvious thing — silently drops every payday later in the final
    // month than today's date, so a stream paid on the 28th would lose its last payment while one
    // paid on the 2nd kept it, for no reason the reader could see.
    let first = crate::forecast::add_months_pub(today, 1);
    let window_start = first.with_day(1).unwrap_or(first);
    let last = crate::forecast::add_months_pub(today, horizon);
    let window_end = crate::reports::last_day_of_month_pub(last.year(), last.month());

    // Twice every calendar month, by definition — so the calendar is the window itself and there is
    // nothing to enumerate. Handled before `step()` because `PayStep` can say "step by one month"
    // but not "twice", and treating it as monthly would pay 12 times a year instead of 24.
    if freq == PayFrequency::SemiMonthly {
        for m in 1..=horizon {
            counts[m as usize] = 2;
        }
        return counts;
    }

    match freq.step() {
        PayStep::Days(n) => {
            // Jump straight to the first payment inside the window instead of walking from the
            // anchor: an anchor set years ago would otherwise iterate thousands of times per
            // stream, per request.
            let elapsed = (window_start - anchor).num_days();
            let k = if elapsed <= 0 {
                0
            } else {
                elapsed.div_euclid(n)
            };
            let mut date = anchor + chrono::Duration::days(k * n);
            while date < window_start {
                date += chrono::Duration::days(n);
            }
            while date <= window_end {
                bump(&mut counts, today, date, horizon);
                date += chrono::Duration::days(n);
            }
        }
        PayStep::Months(n) => {
            let elapsed = crate::forecast::months_between_pub(anchor, window_start);
            let k = if elapsed <= 0 {
                0
            } else {
                elapsed.div_euclid(n)
            };
            let mut step = k * n;
            let mut date = crate::forecast::add_months_pub(anchor, step);
            while date < window_start {
                step += n;
                date = crate::forecast::add_months_pub(anchor, step);
            }
            while date <= window_end {
                bump(&mut counts, today, date, horizon);
                step += n;
                date = crate::forecast::add_months_pub(anchor, step);
            }
        }
    }
    counts
}

fn bump(counts: &mut [u8], today: NaiveDate, date: NaiveDate, horizon: i64) {
    let idx = crate::forecast::months_between_pub(today, date);
    if idx >= 1 && idx <= horizon {
        // Saturating: no frequency here can put more than five paydays in a month, so this can
        // only be reached by a corrupt anchor, and a saturated count is a better answer than a
        // panic on a live GET.
        counts[idx as usize] = counts[idx as usize].saturating_add(1);
    }
}

/// The stream's annual level at each month index, from its dated pay-scale steps plus the residual
/// annual increase that applies once the published scale runs out.
///
/// A step effective *this month or earlier* is already the current level, so it is folded into
/// month 0 rather than applied again later. The residual increase starts only after the last step:
/// a published scale and a hand-typed "and 2% a year after that" must not both apply to the same
/// month, or a teacher's scale would compound against itself.
pub(crate) fn level_schedule(
    stream: &IncomeStream,
    today: NaiveDate,
    horizon: i64,
) -> (f64, Vec<(i64, f64)>, i64, f64) {
    let mut steps: Vec<(i64, f64)> = Vec::new();
    let mut start_level = stream.annual_amount_minor as f64;
    let mut last_step_month = 0i64;

    for s in &stream.steps {
        let Some(on) = crate::reports::parse_date_pub(&s.effective_on) else {
            continue;
        };
        let idx = crate::forecast::months_between_pub(today, on);
        let amount = s.annual_amount_minor as f64;
        if idx <= 0 {
            // Already in force.
            start_level = amount;
        } else if idx <= horizon {
            steps.push((idx, amount));
            last_step_month = last_step_month.max(idx);
        } else {
            // Beyond the horizon: never reached, and not folded in — a scale step in year forty
            // is not this projection's business.
        }
    }
    steps.sort_by_key(|&(m, _)| m);
    let monthly_increase = if stream.annual_increase_bps == 0 {
        1.0
    } else {
        (1.0 + stream.annual_increase_bps as f64 / 10_000.0).powf(1.0 / 12.0)
    };
    (start_level, steps, last_step_month, monthly_increase)
}

/// The gross→net map for one stream, and where it came from.
///
/// Precedence: an explicit override, then "already net", then the statutory scale.
///
/// The statutory rates are computed against `person_annual_gross` — the sum of every gross stream
/// this person has — not against this stream alone. PAYE brackets are progressive over *total*
/// income, so pricing two salaries separately would tax each as if the other did not exist, and
/// under-tax both. That is also why `person_annual_gross` is threaded in from the caller rather
/// than read off the stream.
pub(crate) fn take_home(
    stream: &IncomeStream,
    person_annual_gross: i64,
    on: NaiveDate,
    scales: &TaxScales,
) -> TakeHome {
    if let Some(bps) = stream.take_home_bps {
        return TakeHome {
            average_bps: bps,
            marginal_bps: bps,
            source: TakeHomeSource::Override,
        };
    }
    match stream.basis {
        IncomeBasis::Net => TakeHome::all_of_it(),
        IncomeBasis::GrossNzPaye => {
            // No scale on record for this date — the earliest starts well before any date a
            // projection runs from, so this is the corrupt-input branch rather than a real case.
            // Treating the figure as net would silently inflate income; treating it as fully taxed
            // would silently destroy it. `all_of_it` is the one that cannot hide, because the
            // reconciliation then reports a drift the size of the whole tax bill.
            let Some(resolved) = scales.at(on) else {
                return TakeHome::all_of_it();
            };
            let scale = resolved.as_scale();
            let input = tax::PayeInput {
                annual_gross_minor: person_annual_gross,
                kiwisaver_bps: stream.kiwisaver_bps,
                // Deliberately 0 here: the employer's contribution never touches take-home, and
                // including it would leave the average and marginal rates unchanged while making the
                // reader wonder whether it did. `contribution_rates` is where it matters.
                employer_kiwisaver_bps: 0,
                student_loan: stream.student_loan,
            };
            TakeHome {
                average_bps: tax::average_take_home_bps(&scale, input),
                marginal_bps: tax::marginal_take_home_bps(&scale, input),
                source: TakeHomeSource::Statutory,
            }
        }
    }
}

/// What share of each gross dollar leaves the payslip for somewhere it can be tracked.
///
/// Returned as fractions so the caller can multiply the month's *actual* gross by them — the same
/// arrangement the take-home ratio uses, and for the same reason: the rates are annual questions and
/// the amounts are monthly ones.
///
/// Both are computed against the person's whole gross, because both thresholds are personal rather
/// than per-job: the student-loan threshold applies once across all income, and the ESCT rate is
/// chosen by the total. Where a person has several gross streams and only some carry a student loan,
/// attributing the deduction proportionally across all of them is an approximation — a small one, and
/// the same one the take-home ratio already makes.
pub(crate) fn contribution_rates(
    stream: &IncomeStream,
    person_annual_gross: i64,
    on: NaiveDate,
    scales: &TaxScales,
) -> (f64, f64) {
    if !stream.basis.is_gross() || person_annual_gross <= 0 {
        return (0.0, 0.0);
    }
    let Some(resolved) = scales.at(on) else {
        return (0.0, 0.0);
    };
    let scale = resolved.as_scale();
    let b = tax::paye(
        &scale,
        tax::PayeInput {
            annual_gross_minor: person_annual_gross,
            kiwisaver_bps: stream.kiwisaver_bps,
            employer_kiwisaver_bps: stream.employer_kiwisaver_bps,
            student_loan: stream.student_loan,
        },
    );
    let per_dollar = |v: i64| v as f64 / person_annual_gross as f64;
    (
        per_dollar(b.kiwisaver_credited_minor),
        per_dollar(b.student_loan_minor),
    )
}

/// A stream's active window as month indices, clamped into `0..=horizon`.
///
/// `None` for a stream whose whole window falls outside the projection — it is not in the
/// simulation at all, rather than in it contributing nothing, so it cannot be mistaken for a
/// stream that pays zero.
pub(crate) fn active_window(
    stream: &IncomeStream,
    today: NaiveDate,
    horizon: i64,
) -> Option<(i64, i64)> {
    let from = crate::reports::parse_date_pub(&stream.starts_on)
        .map(|d| crate::forecast::months_between_pub(today, d))
        .unwrap_or(0)
        .max(0);
    let to = match stream
        .ends_on
        .as_deref()
        .and_then(crate::reports::parse_date_pub)
    {
        Some(d) => crate::forecast::months_between_pub(today, d),
        None => horizon,
    };
    if from > horizon || to < 1 || from > to {
        return None;
    }
    Some((from.max(1), to.min(horizon)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::{IncomeStream, IncomeStreamStep};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// An empty store, so `TaxScales` falls back to the built-in figures these tests assert against.
    fn scales() -> TaxScales {
        TaxScales::new(&[])
    }

    fn stream(freq: PayFrequency) -> IncomeStream {
        IncomeStream {
            id: 1,
            person_id: 1,
            label: "Salary".into(),
            employer: None,
            currency_code: "NZD".into(),
            annual_amount_minor: 88_000_00,
            basis: IncomeBasis::GrossNzPaye,
            pay_frequency: freq,
            first_payment_on: "2026-01-02".into(),
            starts_on: "2026-01-01".into(),
            ends_on: None,
            annual_increase_bps: 0,
            kiwisaver_bps: 0,
            employer_kiwisaver_bps: 0,
            student_loan: false,
            take_home_bps: None,
            linked_category_id: None,
            kiwisaver_account_id: None,
            student_loan_account_id: None,
            enabled: true,
            sort_order: 0,
            notes: None,
            steps: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// The whole reason payments are enumerated rather than divided: a fortnightly stream is paid
    /// 26 times a year, which is not twice a month. Some months genuinely have three paydays, and
    /// a cash-flow projection is asked about months.
    ///
    /// Asserted as a *property* rather than a magic total, because the total legitimately depends
    /// on where the anchor's phase falls relative to the year — 26 or 27 are both correct, and a
    /// test that pins one of them pins the phase by accident and breaks on an unrelated edit.
    #[test]
    fn a_fortnightly_stream_puts_three_paydays_in_the_months_that_have_them() {
        let counts = payment_counts(
            PayFrequency::Fortnightly,
            d("2026-01-02"),
            d("2025-12-31"),
            12,
        );
        assert_eq!(counts[0], 0, "the current month is already history");
        assert!(
            counts[1..=12].iter().all(|&c| c == 2 || c == 3),
            "every month gets two or three fortnightly paydays, got {counts:?}"
        );
        assert!(
            counts[1..=12].contains(&3),
            "a year of fortnights has to put three in some month, got {counts:?}"
        );
        let total: i64 = counts.iter().map(|&c| c as i64).sum();
        assert!(
            (26..=27).contains(&total),
            "a year of fortnights, got {total}"
        );
    }

    #[test]
    fn a_weekly_stream_pays_four_or_five_times_a_month() {
        let counts = payment_counts(PayFrequency::Weekly, d("2026-01-02"), d("2025-12-31"), 12);
        assert!(
            counts[1..=12].iter().all(|&c| c == 4 || c == 5),
            "got {counts:?}"
        );
        let total: i64 = counts.iter().map(|&c| c as i64).sum();
        assert!((52..=53).contains(&total), "a year of weeks, got {total}");
    }

    /// Thirteen four-weekly payments is 364 days, so the anchor drifts a day earlier each year and
    /// the extra payday eventually lands in a different month. Two years is 26 payments plus
    /// whatever that drift pulls in — 26 or 27, and both are the calendar being honest.
    #[test]
    fn four_weekly_drift_still_lands_about_thirteen_payments_a_year() {
        let counts = payment_counts(
            PayFrequency::FourWeekly,
            d("2026-01-02"),
            d("2025-12-31"),
            24,
        );
        let total: i64 = counts.iter().map(|&c| c as i64).sum();
        assert!(
            (26..=27).contains(&total),
            "two years four-weekly, got {total}"
        );
        assert!(counts[1..=24].iter().all(|&c| c <= 2), "got {counts:?}");
    }

    /// A quarterly payment lands in the four months it lands in — not smeared across twelve. This
    /// is what `first_payment_on` is for.
    #[test]
    fn a_quarterly_stream_lands_in_four_specific_months() {
        let counts = payment_counts(
            PayFrequency::Quarterly,
            d("2026-02-15"),
            d("2025-12-31"),
            12,
        );
        assert_eq!(counts.iter().map(|&c| c as i64).sum::<i64>(), 4);
        // Feb, May, Aug, Nov — month indices 2, 5, 8, 11 from a 2025-12-31 "today".
        let months: Vec<usize> = counts
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(months, vec![2, 5, 8, 11]);
    }

    /// Twice a month is 24 a year, not 26 — and every month gets exactly two, which is the whole
    /// difference from fortnightly (where two months in a year get three).
    #[test]
    fn a_semi_monthly_stream_pays_twice_in_every_month() {
        let counts = payment_counts(
            PayFrequency::SemiMonthly,
            d("2026-01-14"),
            d("2025-12-31"),
            12,
        );
        assert_eq!(counts.iter().map(|&c| c as i64).sum::<i64>(), 24);
        assert!(counts[1..=12].iter().all(|&c| c == 2), "got {counts:?}");
        // …where fortnightly over the same window puts three in a couple of months.
        let fortnightly = payment_counts(
            PayFrequency::Fortnightly,
            d("2026-01-14"),
            d("2025-12-31"),
            12,
        );
        assert!(fortnightly[1..=12].contains(&3));
    }

    #[test]
    fn an_annual_stream_pays_once_a_year() {
        let counts = payment_counts(PayFrequency::Annual, d("2026-07-01"), d("2025-12-31"), 24);
        assert_eq!(counts.iter().map(|&c| c as i64).sum::<i64>(), 2);
    }

    /// A monthly stream anchored on the 31st still pays in February, on the last day.
    #[test]
    fn a_month_end_anchor_pays_every_month_including_february() {
        let counts = payment_counts(PayFrequency::Monthly, d("2026-01-31"), d("2025-12-31"), 12);
        assert_eq!(counts.iter().map(|&c| c as i64).sum::<i64>(), 12);
        assert!(
            counts[1..=12].iter().all(|&c| c == 1),
            "every month should get exactly one payday, got {counts:?}"
        );
    }

    /// An anchor set years ago must not be walked from payment by payment — the guard against a
    /// per-request loop over a decade of fortnights — and must still produce a full year of pay.
    #[test]
    fn a_long_past_anchor_still_produces_a_full_year_of_paydays() {
        let counts = payment_counts(
            PayFrequency::Fortnightly,
            d("2010-01-01"),
            d("2026-01-15"),
            12,
        );
        let total: i64 = counts.iter().map(|&c| c as i64).sum();
        assert!((26..=27).contains(&total), "got {total} from a 2010 anchor");
        assert!(counts[1..=12].iter().all(|&c| c >= 2), "got {counts:?}");
    }

    #[test]
    fn a_future_anchor_pays_nothing_until_it_arrives() {
        let counts = payment_counts(PayFrequency::Monthly, d("2027-01-05"), d("2026-01-15"), 12);
        // Twelve months from mid-Jan 2026 reaches Jan 2027, which is exactly one payday.
        assert_eq!(counts.iter().map(|&c| c as i64).sum::<i64>(), 1);
    }

    /// A step already in force is the current level, not a future change — otherwise a scale
    /// entered last year would "raise" the salary again next month.
    #[test]
    fn a_step_already_in_force_becomes_the_starting_level() {
        let mut s = stream(PayFrequency::Fortnightly);
        s.steps = vec![
            IncomeStreamStep {
                id: 1,
                income_stream_id: 1,
                effective_on: "2026-01-01".into(),
                annual_amount_minor: 90_000_00,
                label: None,
            },
            IncomeStreamStep {
                id: 2,
                income_stream_id: 1,
                effective_on: "2027-04-01".into(),
                annual_amount_minor: 94_000_00,
                label: None,
            },
        ];
        let (start, steps, last, _) = level_schedule(&s, d("2026-06-01"), 60);
        assert_eq!(start, 90_000_00.0);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].1, 94_000_00.0);
        assert_eq!(last, steps[0].0);
    }

    #[test]
    fn a_step_beyond_the_horizon_is_not_folded_in() {
        let mut s = stream(PayFrequency::Fortnightly);
        s.steps = vec![IncomeStreamStep {
            id: 1,
            income_stream_id: 1,
            effective_on: "2040-04-01".into(),
            annual_amount_minor: 200_000_00,
            label: None,
        }];
        let (start, steps, _, _) = level_schedule(&s, d("2026-06-01"), 12);
        assert_eq!(start, 88_000_00.0);
        assert!(steps.is_empty());
    }

    /// Two gross streams for one person are taxed together, because the brackets are progressive
    /// over total income. Pricing each alone would under-tax both.
    #[test]
    fn a_second_salary_is_taxed_at_the_rate_the_pair_reaches() {
        let s = stream(PayFrequency::Fortnightly);
        let alone = take_home(&s, 88_000_00, d("2026-08-05"), &scales());
        let together = take_home(&s, 88_000_00 + 60_000_00, d("2026-08-05"), &scales());
        assert!(
            together.average_bps < alone.average_bps,
            "the pair should keep proportionally less: {} vs {}",
            together.average_bps,
            alone.average_bps
        );
        assert_eq!(together.source, TakeHomeSource::Statutory);
    }

    #[test]
    fn an_override_beats_the_statutory_scale_and_a_net_stream_keeps_everything() {
        let mut s = stream(PayFrequency::Fortnightly);
        s.take_home_bps = Some(6_600);
        let th = take_home(&s, 88_000_00, d("2026-08-05"), &scales());
        assert_eq!(th.average_bps, 6_600);
        assert_eq!(th.marginal_bps, 6_600);
        assert_eq!(th.source, TakeHomeSource::Override);

        let mut n = stream(PayFrequency::Fortnightly);
        n.basis = IncomeBasis::Net;
        let th = take_home(&n, 88_000_00, d("2026-08-05"), &scales());
        assert_eq!(th.average_bps, 10_000);
        assert_eq!(th.source, TakeHomeSource::AlreadyNet);
    }

    /// Both contributions come out of gross, and the KiwiSaver share includes the employer's — net
    /// of ESCT. A projection that credited only the employee's half would understate the balance by
    /// roughly half over a career.
    #[test]
    fn contribution_rates_include_the_employer_and_government_shares() {
        let mut s = stream(PayFrequency::Fortnightly);
        s.annual_amount_minor = 100_000_00;
        s.kiwisaver_bps = 350;
        s.employer_kiwisaver_bps = 350;
        s.student_loan = true;
        let (ks, sl) = contribution_rates(&s, 100_000_00, d("2026-08-05"), &scales());
        // 3.5% member + 3.5% employer less 33% ESCT on the employer half = 5.845% of gross, plus
        // the government's $260.72 (capped, and matched against the member's half alone) = 0.26%.
        assert!((ks - 0.061_057_2).abs() < 1e-6, "kiwisaver share was {ks}");
        // 12% of the excess over the threshold, as a share of the whole salary.
        let expected_sl = 0.12 * (100_000.0 - 24_128.0) / 100_000.0;
        assert!(
            (sl - expected_sl).abs() < 1e-4,
            "student loan share was {sl}"
        );
    }

    /// A stream recorded as take-home has already had everything taken off it, so there is nothing
    /// left to route anywhere.
    #[test]
    fn a_net_stream_contributes_nothing_to_route() {
        let mut s = stream(PayFrequency::Monthly);
        s.basis = IncomeBasis::Net;
        s.kiwisaver_bps = 350;
        s.student_loan = true;
        assert_eq!(
            contribution_rates(&s, 80_000_00, d("2026-08-05"), &scales()),
            (0.0, 0.0)
        );
    }

    #[test]
    fn an_active_window_is_clamped_into_the_horizon_and_absent_when_it_misses() {
        let mut s = stream(PayFrequency::Monthly);
        s.starts_on = "2026-01-01".into();
        s.ends_on = None;
        assert_eq!(active_window(&s, d("2026-06-01"), 12), Some((1, 12)));

        // Ends before the projection begins: not in the simulation at all.
        s.ends_on = Some("2026-06-15".into());
        assert_eq!(active_window(&s, d("2026-06-01"), 12), None);

        // Starts after it ends: same.
        s.starts_on = "2030-01-01".into();
        s.ends_on = None;
        assert_eq!(active_window(&s, d("2026-06-01"), 12), None);
    }
}
