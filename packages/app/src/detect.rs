//! Working out what someone earns from what has actually landed in their account.
//!
//! Typing a salary in is error-prone in a specific way: people know their annual figure and their
//! rough cadence, and are wrong about the details that matter most to a monthly projection — whether
//! "fortnightly" means every fourteen days or twice a month, which day it lands, and what the *net*
//! figure actually is after everything their payroll takes off. All three are already recorded in
//! the ledger, so the honest move is to read them rather than ask.

use std::collections::HashMap;

use chrono::{Datelike, NaiveDate};
use sure_core::{PayFrequency, Transaction};

use crate::ports::ImportRow;

/// How many days apart two payments have to be to count as the same cadence.
///
/// One day of slack, because payroll moves off a weekend: a fortnightly run landing on Friday the
/// 13th and then Friday the 27th is fourteen days, but a run that hits a public holiday shifts.
const DAY_TOLERANCE: i64 = 2;

/// Below this many payments there is no cadence to find — two points make a line through anything.
const MIN_PAYMENTS: usize = 4;

/// A salary the ledger appears to contain.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectedStream {
    /// What the payments call themselves — the most common description, which is usually the
    /// employer's name as it appears on the statement.
    pub label: String,
    pub account_id: i64,
    pub category_id: Option<i64>,
    pub currency_code: String,
    pub pay_frequency: PayFrequency,
    /// The most recent payment, which is the anchor a projection should count forward from.
    pub last_paid_on: String,
    /// The next payment after `last_paid_on`, so the stream starts in the future rather than
    /// re-crediting one already in the ledger.
    pub next_payment_on: String,
    /// Typical amount per payment, in minor units — the median, so one odd month does not move it.
    pub per_payment_minor: i64,
    /// `per_payment_minor` x the cadence's payments per year. **Net**, because it is what landed.
    pub annual_net_minor: i64,
    pub payments_seen: usize,
    /// The days of the month payments land on, for a semi-monthly cadence — the evidence for
    /// calling it that rather than fortnightly.
    pub days_of_month: Vec<u32>,
    /// How much the amounts vary, in basis points of the median. A salary is near zero; a
    /// reimbursement or a fluctuating contract is not, and is worth flagging rather than modelling
    /// as a fixed figure.
    pub variability_bps: i64,
}

/// Group income transactions into candidate salaries.
///
/// Grouped by `(account, description)` rather than by amount: an employer's payments carry the same
/// description whatever the figure, and grouping by amount would split a salary in half the moment
/// it changed. Descriptions are matched case-insensitively on their first two words, because payroll
/// references often append a run number that differs every time.
pub fn detect(txns: &[Transaction], today: NaiveDate) -> Vec<DetectedStream> {
    let mut groups: HashMap<(i64, String), Vec<&Transaction>> = HashMap::new();
    for t in txns {
        // Income only. A refund is not a salary, and neither is anything leaving the account.
        if t.amount_minor <= 0 || t.is_one_off || t.linked_transaction_id.is_some() {
            continue;
        }
        let key = description_key(&t.description);
        if key.is_empty() {
            continue;
        }
        groups.entry((t.account_id, key)).or_default().push(t);
    }

    let mut out: Vec<DetectedStream> = groups
        .into_iter()
        .filter_map(|((account_id, _), mut group)| {
            group.sort_by(|a, b| a.posted_at.cmp(&b.posted_at));
            candidate(account_id, &group, today)
        })
        .collect();
    // Biggest first: the salary is what someone is here to record, and it is almost always the
    // largest recurring credit.
    out.sort_by_key(|s| std::cmp::Reverse(s.annual_net_minor));
    out
}

/// The first two words, lowercased — enough to identify an employer, short enough to survive a
/// payroll reference that changes every run.
fn description_key(description: &str) -> String {
    description
        .split_whitespace()
        .take(2)
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

fn candidate(account_id: i64, group: &[&Transaction], today: NaiveDate) -> Option<DetectedStream> {
    if group.len() < MIN_PAYMENTS {
        return None;
    }
    let dates: Vec<NaiveDate> = group
        .iter()
        .filter_map(|t| crate::reports::parse_date_pub(&t.posted_at))
        .collect();
    if dates.len() < MIN_PAYMENTS {
        return None;
    }
    let gaps: Vec<i64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days()).collect();
    let days_of_month: Vec<u32> = {
        let mut d: Vec<u32> = dates.iter().map(|d| d.day()).collect();
        d.sort_unstable();
        d.dedup();
        d
    };
    let pay_frequency = classify(&gaps, &days_of_month)?;

    let mut amounts: Vec<i64> = group.iter().map(|t| t.amount_minor).collect();
    amounts.sort_unstable();
    let per_payment_minor = amounts[amounts.len() / 2];
    if per_payment_minor <= 0 {
        return None;
    }
    // Spread as a share of the typical payment. A salary sits near zero; anything wide is a
    // fluctuating payment that a fixed annual figure would misrepresent.
    let spread = amounts.last().copied().unwrap_or(0) - amounts.first().copied().unwrap_or(0);
    let variability_bps = spread.saturating_mul(10_000) / per_payment_minor.max(1);

    let last = *dates.last()?;
    let next = next_after(last, pay_frequency, &days_of_month);
    // Anything whose next payment is already well past is a job that ended, not a salary to record.
    if next < today - chrono::Duration::days(45) {
        return None;
    }

    let label = most_common_description(group);
    let category_id = group.iter().find_map(|t| t.category_id);
    Some(DetectedStream {
        label,
        account_id,
        category_id,
        currency_code: group[0].currency_code.clone(),
        pay_frequency,
        last_paid_on: last.to_string(),
        next_payment_on: next.max(today).to_string(),
        per_payment_minor,
        annual_net_minor: (per_payment_minor as f64 * pay_frequency.periods_per_year()).round()
            as i64,
        payments_seen: dates.len(),
        days_of_month,
        variability_bps,
    })
}

/// The cadence a run of gaps describes.
///
/// The interesting case is 14 versus twice-a-month, which people conflate constantly: both average
/// about a fortnight, but one is 26 payments a year and the other 24. They are told apart by
/// *shape*, not by average — twice-monthly alternates long and short gaps (the 14th to the 28th is
/// 14 days, the 28th to the 14th is 17) and lands on the same two days every month, where genuinely
/// fortnightly walks through the calendar and hits every day of the month eventually.
fn classify(gaps: &[i64], days_of_month: &[u32]) -> Option<PayFrequency> {
    if gaps.is_empty() {
        return None;
    }
    let near = |target: i64| gaps.iter().all(|g| (g - target).abs() <= DAY_TOLERANCE);

    // Two fixed days of the month, with alternating gaps that average a fortnight but are not
    // consistently fourteen days.
    let mean = gaps.iter().sum::<i64>() as f64 / gaps.len() as f64;
    if days_of_month.len() == 2 && (13.0..=18.0).contains(&mean) && !near(14) {
        return Some(PayFrequency::SemiMonthly);
    }
    if near(7) {
        return Some(PayFrequency::Weekly);
    }
    if near(14) {
        return Some(PayFrequency::Fortnightly);
    }
    if near(28) {
        return Some(PayFrequency::FourWeekly);
    }
    // Month-stepped cadences drift with month length, so they are matched on a range rather than a
    // tolerance: 28 to 31 days is "monthly" however February behaves.
    if gaps.iter().all(|g| (28..=31).contains(g)) {
        return Some(PayFrequency::Monthly);
    }
    if gaps.iter().all(|g| (89..=93).contains(g)) {
        return Some(PayFrequency::Quarterly);
    }
    if gaps.iter().all(|g| (362..=368).contains(g)) {
        return Some(PayFrequency::Annual);
    }
    None
}

/// The next payment due after `last`.
fn next_after(last: NaiveDate, freq: PayFrequency, days_of_month: &[u32]) -> NaiveDate {
    match freq {
        PayFrequency::Weekly => last + chrono::Duration::days(7),
        PayFrequency::Fortnightly => last + chrono::Duration::days(14),
        PayFrequency::FourWeekly => last + chrono::Duration::days(28),
        PayFrequency::SemiMonthly => {
            // Whichever of the two days comes next — the other one this month, or the earlier one
            // next month.
            let mut days = days_of_month.to_vec();
            days.sort_unstable();
            match days.iter().find(|&&d| d > last.day()) {
                Some(&d) => last.with_day(d).unwrap_or(last),
                None => {
                    let next_month = crate::forecast::add_months_pub(last, 1);
                    next_month.with_day(days[0]).unwrap_or(next_month)
                }
            }
        }
        PayFrequency::Monthly => crate::forecast::add_months_pub(last, 1),
        PayFrequency::Quarterly => crate::forecast::add_months_pub(last, 3),
        PayFrequency::Annual => crate::forecast::add_months_pub(last, 12),
    }
}

/// How many times bigger than every other movement on the account the drawdown has to be
/// before it is unambiguous.
///
/// Two advances of similar size are a facility drawn in tranches, not one loan with one
/// original amount, and naming either would be a guess. It also has to clear the *repayments*,
/// which is what rules out a truncated history whose earliest row is an ordinary interest
/// charge. Two is loose enough for any real loan — a $485,000 mortgage against $2,000
/// repayments is a factor of 280 — and tight enough that the ambiguous cases decline to answer.
const DRAWDOWN_DOMINANCE: i64 = 2;

/// The amount a mortgage or loan was originally drawn down for, read out of the history the
/// feed just handed over, or `None` when that history doesn't unambiguously contain it.
///
/// A bank reports the original amount as a field only sometimes — Akahu has
/// `meta.loan_details.initial_principal`, and for an ASB mortgage it is routinely absent — but
/// when the account was opened recently enough the history still holds the drawdown itself, and
/// that row *is* the answer. Until now it went unread and the figure had to be typed in, which
/// is the kind of number people are wrong about: it is the amount borrowed, not the balance on
/// the day they set the account up.
///
/// `outstanding_minor` is the account's current balance in the same signed convention as the
/// rows (a liability is negative). It is what makes this safe on a *truncated* history — the
/// ordinary case, since a feed reaches back a year or so and a mortgage runs for thirty. When
/// the drawdown is off the front of the window the largest advance left is an interest charge,
/// and an interest charge is nowhere near the balance still owed, so no answer is given.
///
/// Deliberately silent rather than approximate: every guard below returns `None`, because a
/// wrong original amount is worse than an absent one. It is shown as a paid-down percentage
/// and feeds the forecast's amortisation schedule, where being quietly wrong is invisible,
/// whereas an absent one is a field the user can still fill in.
pub fn drawdown_original_amount(rows: &[ImportRow], outstanding_minor: i64) -> Option<i64> {
    /// The date part of a row's timestamp, which is all this orders on. Same `..10` slice the
    /// import pipeline uses: the times within a day are not comparable across sources, and a
    /// drawdown and its establishment fee routinely land on one day anyway.
    fn day(r: &ImportRow) -> &str {
        r.posted_at.get(..10).unwrap_or(&r.posted_at)
    }

    // On a liability a negative amount grows the debt, so an advance is a negative row — the
    // one signed convention `tasks::balance_delta` differences valuations into. `min_by_key`
    // keeps the first of equal candidates, so this is stable rather than order-dependent.
    let (at, drawdown) = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.amount_minor < 0)
        .min_by_key(|(_, r)| r.amount_minor)?;
    let original = drawdown.amount_minor.checked_neg()?;

    // Nothing may predate it. A loan's first event is the money arriving; a row before it means
    // the window opened mid-life and whatever this is, it is not the drawdown.
    if day(drawdown) > rows.iter().map(day).min()? {
        return None;
    }

    // And it must dwarf everything else the account did — see `DRAWDOWN_DOMINANCE`.
    let largest_other = rows
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at)
        .map(|(_, r)| r.amount_minor.saturating_abs())
        .max()
        .unwrap_or(0);
    if original < largest_other.saturating_mul(DRAWDOWN_DOMINANCE) {
        return None;
    }

    // You cannot still owe more than you borrowed. `min(0)` because a liability is negative and
    // a credit balance is not a debt to measure against.
    if original < outstanding_minor.min(0).saturating_neg() {
        return None;
    }

    Some(original)
}

fn most_common_description(group: &[&Transaction]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for t in group {
        *counts.entry(t.description.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(d, n)| (n, std::cmp::Reverse(d)))
        .map(|(d, _)| d.to_string())
        .unwrap_or_else(|| "Salary".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sure_core::Ownership;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn tx(posted_at: &str, amount_minor: i64, description: &str) -> Transaction {
        Transaction {
            id: 0,
            account_id: 1,
            posted_at: posted_at.to_string(),
            amount_minor,
            currency_code: "NZD".into(),
            description: description.to_string(),
            merchant: None,
            merchant_id: None,
            notes: None,
            category_id: Some(3),
            is_one_off: false,
            linked_transaction_id: None,
            provider: None,
            external_id: None,
            categorized_by_rule_id: None,
            ownership: Some(Ownership::Joint),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// Every fourteen days, walking through the calendar.
    fn fortnightly(from: &str, n: usize, amount: i64) -> Vec<Transaction> {
        let mut date = d(from);
        (0..n)
            .map(|_| {
                let t = tx(&date.to_string(), amount, "ACME PAYROLL");
                date += chrono::Duration::days(14);
                t
            })
            .collect()
    }

    /// The 14th and the 28th of every month.
    fn semi_monthly(from_month: &str, n: usize, amount: i64) -> Vec<Transaction> {
        let start = d(from_month);
        (0..n)
            .map(|i| {
                let month = crate::forecast::add_months_pub(start, (i / 2) as i64);
                let day = if i % 2 == 0 { 14 } else { 28 };
                let date = month.with_day(day).unwrap();
                tx(&date.to_string(), amount, "ACME PAYROLL")
            })
            .collect()
    }

    /// The distinction the whole detector exists for. Both average about a fortnight; one is 26
    /// payments a year and the other 24, and people describe both as "fortnightly".
    #[test]
    fn twice_a_month_is_told_apart_from_every_fourteen_days() {
        let semi = detect(&semi_monthly("2026-01-14", 12, 5_625_00), d("2026-07-01"));
        assert_eq!(semi.len(), 1);
        assert_eq!(semi[0].pay_frequency, PayFrequency::SemiMonthly);
        assert_eq!(semi[0].days_of_month, vec![14, 28]);
        // 24 payments a year, not 26 — which is the figure a projection needs.
        assert_eq!(semi[0].annual_net_minor, 5_625_00 * 24);

        let fort = detect(&fortnightly("2026-01-02", 12, 5_192_00), d("2026-07-01"));
        assert_eq!(fort.len(), 1);
        assert_eq!(fort[0].pay_frequency, PayFrequency::Fortnightly);
        assert_eq!(fort[0].annual_net_minor, 5_192_00 * 26);
    }

    #[test]
    fn weekly_monthly_quarterly_and_annual_are_recognised() {
        let mk = |step_days: i64, n: usize| {
            let mut date = d("2026-01-05");
            (0..n)
                .map(|_| {
                    let t = tx(&date.to_string(), 1_000_00, "PAYROLL RUN");
                    date += chrono::Duration::days(step_days);
                    t
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            detect(&mk(7, 10), d("2026-04-01"))[0].pay_frequency,
            PayFrequency::Weekly
        );
        assert_eq!(
            detect(&mk(91, 6), d("2027-06-01"))[0].pay_frequency,
            PayFrequency::Quarterly
        );

        // Monthly drifts with month length, so it is matched on a range.
        let monthly: Vec<Transaction> = (0..8)
            .map(|i| {
                let date = crate::forecast::add_months_pub(d("2026-01-20"), i);
                tx(&date.to_string(), 4_000_00, "PAYROLL RUN")
            })
            .collect();
        assert_eq!(
            detect(&monthly, d("2026-09-01"))[0].pay_frequency,
            PayFrequency::Monthly
        );
    }

    /// The anchor has to be the *next* payment: starting a stream on one already in the ledger would
    /// credit it twice, once as history and once as a projection.
    #[test]
    fn the_anchor_is_the_next_payment_not_the_last_one() {
        // Last payment 2026-06-05, so the next is due on the 19th — comfortably ahead of "today".
        let ahead = detect(&fortnightly("2026-01-02", 12, 5_000_00), d("2026-06-10"));
        assert_eq!(ahead[0].last_paid_on, "2026-06-05");
        assert_eq!(ahead[0].next_payment_on, "2026-06-19");

        // …and when the next one has already slipped past — payroll ran but the ledger has not
        // caught up — the anchor is today rather than a date in the past, which would credit a
        // payment the projection cannot know happened.
        let behind = detect(&fortnightly("2026-01-02", 12, 5_000_00), d("2026-06-20"));
        assert_eq!(behind[0].next_payment_on, "2026-06-20");
        assert!(behind[0].next_payment_on > behind[0].last_paid_on);
    }

    /// A wandering amount is reported rather than smoothed into a fixed salary — a reimbursement or
    /// a fluctuating contract is not something a fixed annual figure describes.
    #[test]
    fn a_variable_amount_is_flagged_rather_than_averaged_away() {
        let mut txns = fortnightly("2026-01-02", 10, 2_000_00);
        for (i, t) in txns.iter_mut().enumerate() {
            t.amount_minor = 2_000_00 + (i as i64 * 400_00);
        }
        let found = detect(&txns, d("2026-06-01"));
        assert!(found[0].variability_bps > 5_000, "{found:?}");

        let steady = detect(&fortnightly("2026-01-02", 10, 2_000_00), d("2026-06-01"));
        assert_eq!(steady[0].variability_bps, 0);
    }

    #[test]
    fn irregular_credits_are_not_mistaken_for_a_salary() {
        let txns = vec![
            tx("2026-01-03", 500_00, "SOME REFUND"),
            tx("2026-02-19", 500_00, "SOME REFUND"),
            tx("2026-05-02", 500_00, "SOME REFUND"),
            tx("2026-05-30", 500_00, "SOME REFUND"),
        ];
        assert!(detect(&txns, d("2026-06-01")).is_empty());
    }

    #[test]
    fn spending_transfers_and_one_offs_are_ignored() {
        let mut txns = fortnightly("2026-01-02", 8, 3_000_00);
        // Outgoings, a one-off bonus and a transfer leg must not become salaries.
        txns.push(tx("2026-02-01", -900_00, "RENT PAYMENT"));
        let mut bonus = tx("2026-03-01", 9_000_00, "ACME BONUS");
        bonus.is_one_off = true;
        txns.push(bonus);
        let mut transfer = tx("2026-03-02", 4_000_00, "TRANSFER IN");
        transfer.linked_transaction_id = Some(7);
        txns.push(transfer);

        let found = detect(&txns, d("2026-05-01"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pay_frequency, PayFrequency::Fortnightly);
    }

    #[test]
    fn too_few_payments_are_not_a_cadence() {
        assert!(detect(&fortnightly("2026-05-01", 3, 1_000_00), d("2026-06-01")).is_empty());
    }

    /// A job that stopped months ago is history, not something to record as current income.
    #[test]
    fn a_stream_that_stopped_long_ago_is_not_offered() {
        assert!(detect(&fortnightly("2024-01-05", 10, 3_000_00), d("2026-06-01")).is_empty());
    }

    /// Payroll shifting off a weekend must not break the cadence.
    #[test]
    fn a_payday_nudged_by_a_holiday_is_still_the_same_cadence() {
        let mut txns = fortnightly("2026-01-02", 8, 3_000_00);
        txns[4].posted_at = (d(&txns[4].posted_at) - chrono::Duration::days(1)).to_string();
        let found = detect(&txns, d("2026-05-01"));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pay_frequency, PayFrequency::Fortnightly);
    }

    /// The biggest recurring credit first — that is the one someone came here to record.
    #[test]
    fn the_largest_candidate_is_offered_first() {
        let mut txns = fortnightly("2026-01-02", 10, 5_000_00);
        let mut side = fortnightly("2026-01-09", 10, 400_00);
        for t in side.iter_mut() {
            t.description = "SIDE GIG".into();
        }
        txns.extend(side);
        let found = detect(&txns, d("2026-06-01"));
        assert_eq!(found.len(), 2);
        assert!(found[0].annual_net_minor > found[1].annual_net_minor);
        assert_eq!(found[0].label, "ACME PAYROLL");
    }

    /// A movement on a loan account, in the signed convention the ledger stores: negative grows
    /// the debt, positive repays it.
    fn movement(posted_at: &str, amount_minor: i64, description: &str) -> ImportRow {
        ImportRow {
            external_id: format!("{posted_at}:{amount_minor}"),
            posted_at: format!("{posted_at}T00:00:00Z"),
            amount_minor,
            currency_code: Some("NZD".into()),
            description: description.to_string(),
            merchant: None,
            category_name: None,
            category_group: None,
            category_kind: None,
            is_one_off: false,
        }
    }

    /// The month after a mortgage is drawn down: the advance, then a payment and the interest
    /// it covers. The advance is the original amount, and nothing else comes close to it.
    #[test]
    fn a_fresh_mortgages_drawdown_is_the_amount_borrowed() {
        let rows = vec![
            movement("2026-03-02", -485_000_00, "Loan drawdown"),
            movement("2026-03-31", -2_310_00, "Interest"),
            movement("2026-03-31", 3_100_00, "Payment received"),
        ];
        assert_eq!(
            drawdown_original_amount(&rows, -484_210_00),
            Some(485_000_00)
        );
    }

    /// The drawdown alone, which is what the very first sync after taking out a loan sees.
    #[test]
    fn a_drawdown_with_nothing_after_it_still_answers() {
        let rows = vec![movement("2026-03-02", -25_000_00, "Advance")];
        assert_eq!(drawdown_original_amount(&rows, -25_000_00), Some(25_000_00));
    }

    /// The ordinary case, and the one that has to stay silent: a feed reaches back a year, a
    /// mortgage runs for thirty, so the drawdown is off the front of the window entirely. The
    /// largest advance left is a monthly interest charge — nowhere near what is still owed.
    #[test]
    fn a_truncated_history_offers_nothing() {
        let mut rows = Vec::new();
        for month in 1..=9 {
            rows.push(movement(
                &format!("2026-{month:02}-28"),
                -2_290_00,
                "Interest",
            ));
            rows.push(movement(
                &format!("2026-{month:02}-28"),
                3_100_00,
                "Payment received",
            ));
        }
        assert_eq!(drawdown_original_amount(&rows, -512_400_00), None);
    }

    /// Even a nearly-repaid loan, where the balance is small enough that an interest charge
    /// could clear it — the dominance rule is what refuses this one, not the balance.
    #[test]
    fn a_nearly_repaid_loan_with_no_drawdown_in_view_offers_nothing() {
        let rows = vec![
            movement("2026-06-30", -3_00, "Interest"),
            movement("2026-06-30", 500_00, "Payment received"),
            movement("2026-07-31", -1_00, "Interest"),
        ];
        assert_eq!(drawdown_original_amount(&rows, -1_00), None);
    }

    /// A facility drawn in two tranches has no single original amount, so naming either is a
    /// guess. Declining leaves a field the user can fill in; guessing leaves one they won't check.
    #[test]
    fn two_similar_advances_are_ambiguous_and_decline() {
        let rows = vec![
            movement("2026-03-02", -300_000_00, "Drawdown"),
            movement("2026-05-02", -250_000_00, "Drawdown"),
            movement("2026-05-31", 4_000_00, "Payment received"),
        ];
        assert_eq!(drawdown_original_amount(&rows, -546_000_00), None);
    }

    /// A big advance that isn't the first thing on the account is a top-up, not the drawdown.
    #[test]
    fn an_advance_that_something_predates_is_not_a_drawdown() {
        let rows = vec![
            movement("2026-02-15", 1_200_00, "Payment received"),
            movement("2026-03-02", -80_000_00, "Further advance"),
        ];
        assert_eq!(drawdown_original_amount(&rows, -78_800_00), None);
    }

    /// You cannot still owe more than you borrowed: an advance smaller than the balance means
    /// the real drawdown is somewhere off-window, whatever this row is.
    #[test]
    fn an_advance_smaller_than_the_balance_is_refused() {
        let rows = vec![movement("2026-03-02", -40_000_00, "Advance")];
        assert_eq!(drawdown_original_amount(&rows, -520_000_00), None);
    }

    /// An account whose history is all repayments has no advance to read at all.
    #[test]
    fn a_history_with_no_advance_offers_nothing() {
        let rows = vec![
            movement("2026-03-31", 3_100_00, "Payment received"),
            movement("2026-04-30", 3_100_00, "Payment received"),
        ];
        assert_eq!(drawdown_original_amount(&rows, -400_000_00), None);
        assert_eq!(drawdown_original_amount(&[], -400_000_00), None);
    }
}
