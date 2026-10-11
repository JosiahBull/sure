//! Recovering a loan original amount from its observed drawdown.
use crate::ports::ImportRow;

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

#[cfg(test)]
mod tests {
    use super::*;
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
            exclude_from_cashflow: false,
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
