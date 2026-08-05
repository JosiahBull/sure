//! New Zealand PAYE: gross salary in, take-home out.
//!
//! Pure calendar-and-arithmetic, like the rest of `sure-core` — no clock, no I/O, no rounding to
//! a payslip. The forecast asks it two questions per person per month and nothing else does.
//!
//! # Why a statutory table at all
//!
//! The forecast's income baselines are fitted from *net* money landing in the bank, and a salary
//! is quoted *gross*. Something has to bridge the two, and there are only two candidates: a
//! statutory table, or a ratio measured from this household's own history.
//!
//! Measured-from-history is better at almost everything. It absorbs salary sacrifice, ESCT, a
//! secondary tax code, union fees and health insurance deducted at source, none of which a
//! bracket table can see; it is per-person; and it is falsifiable — "modelled gross $X, observed
//! net $Y, so 71.4%" is a number the user can check. What it cannot do is price a raise that has
//! not happened yet. A promotion's increment is taxed at the *bracket the salary lands in*, not
//! at the household's historical average, and over thirty years and several promotions that
//! error compounds in one direction.
//!
//! So this module supplies the marginal rate and the reconciliation supplies the average, and
//! [`crate::income::TakeHome`] carries both. Neither is asked to do the other's job.
//!
//! # Provenance of the figures
//!
//! Every rate and threshold below was read off a published source at the date in the scale's
//! comment, not recalled. They are external facts with no defensible default: an invented
//! bracket is indistinguishable from a real one once a thirty-year projection is derived from
//! it. Appending a newer scale is the only way to change one — an existing entry describes a tax
//! year that has already happened, and editing it would silently restate history.

use std::str::FromStr;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which deduction model applies to an income stream.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaxScaleId {
    /// New Zealand PAYE: progressive income tax, ACC earner levy, KiwiSaver, student loan.
    #[default]
    NzPaye,
    /// No deduction model. The amount recorded is already take-home.
    ///
    /// Not a placeholder — this is what someone outside New Zealand selects, and what makes the
    /// statutory table an option rather than a dependency of the whole feature. It is also the
    /// honest choice for income that never had PAYE applied: a reimbursement, a tax-free grant.
    None,
}

impl TaxScaleId {
    pub fn as_str(self) -> &'static str {
        match self {
            TaxScaleId::NzPaye => "nz_paye",
            TaxScaleId::None => "none",
        }
    }
}

impl FromStr for TaxScaleId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nz_paye" => Ok(TaxScaleId::NzPaye),
            "none" => Ok(TaxScaleId::None),
            other => Err(format!("unknown tax scale '{other}'")),
        }
    }
}

/// A dated snapshot of one jurisdiction's deduction rules.
#[derive(Debug, Clone, Copy)]
pub struct TaxScale {
    /// ISO-8601 date this scale takes effect. Scales are held in ascending order and the latest
    /// one not after the date being asked about wins.
    pub effective_from: &'static str,
    /// `(inclusive_upper_bound_annual_minor, rate_bps)`, ascending. The final bound is
    /// [`i64::MAX`], so there is no "and above" special case to forget.
    pub brackets: &'static [(i64, i64)],
    /// ACC earner levy, in basis points of liable earnings.
    pub acc_levy_bps: i64,
    /// Earnings above this attract no further earner levy.
    pub acc_income_cap_minor: i64,
    /// Annual income above which student loan repayments are deducted.
    pub student_loan_threshold_minor: i64,
    pub student_loan_rate_bps: i64,
    /// ESCT brackets, same `(upper_bound, rate_bps)` shape as `brackets`.
    ///
    /// Employer superannuation contribution tax is taken **off** the employer's KiwiSaver
    /// contribution — business.govt.nz: "the tax you take off the cash contributions you make to
    /// employees' superannuation accounts" — so the amount reaching the account is the contribution
    /// less ESCT, not the contribution. Getting that backwards overstates a KiwiSaver balance by up
    /// to 39% of every employer dollar, which over thirty years is not a rounding error.
    ///
    /// Unlike PAYE this is a *flat* rate chosen by which bracket the employee's total lands in, not
    /// a progressive slice-by-slice calculation.
    pub esct_brackets: &'static [(i64, i64)],
}

/// New Zealand, ascending by `effective_from`.
///
/// Sources, all read 2026-08-05:
///   * income tax brackets — ird.govt.nz, "Tax rates for individuals" (in force from 1 Apr 2025;
///     these replaced the composite part-year rates that applied across the 2024-25 year, when
///     the 31 Jul 2024 threshold change landed mid-year);
///   * ACC earner levy and the liable-earnings cap — calculate.co.nz's IRD/ACC reference table
///     for 2026/27, cross-checked against its own stated maximum levy ($2,741.22 = $156,641 ×
///     1.75%, which reconciles);
///   * student loan threshold — ird.govt.nz/student-loans states the $24,128 figure directly;
///     the 12% rate is long-standing and corroborated by two independent references.
///
/// The 2025-26 scale is carried even though only the levy and cap differ from 2026-27: a
/// projection reconciled against the trailing twelve months of history spans both, and a single
/// scale would price last year's pay at this year's levy.
pub const NZ_TAX_SCALES: &[TaxScale] = &[
    TaxScale {
        effective_from: "2025-04-01",
        brackets: NZ_BRACKETS_2025,
        // 1.67% incl GST, capped at $152,790 — the 2025-26 figures.
        acc_levy_bps: 167,
        acc_income_cap_minor: 152_790_00,
        student_loan_threshold_minor: 24_128_00,
        student_loan_rate_bps: 1_200,
        esct_brackets: NZ_ESCT_2025,
    },
    TaxScale {
        effective_from: "2026-04-01",
        brackets: NZ_BRACKETS_2025,
        // 1.75% incl GST, capped at $156,641.
        acc_levy_bps: 175,
        acc_income_cap_minor: 156_641_00,
        student_loan_threshold_minor: 24_128_00,
        student_loan_rate_bps: 1_200,
        esct_brackets: NZ_ESCT_2025,
    },
];

/// In force from 1 April 2025, and unchanged for 2026-27. Shared between the scales above rather
/// than duplicated, so a future bracket change is visibly a new table and not a typo in one of
/// two copies that were meant to match.
const NZ_BRACKETS_2025: &[(i64, i64)] = &[
    (15_600_00, 1_050),
    (53_500_00, 1_750),
    (78_100_00, 3_000),
    (180_000_00, 3_300),
    (i64::MAX, 3_900),
];

/// ESCT thresholds, reset 1 April 2025 and unchanged for 2026-27.
///
/// They are exactly 20% above [`NZ_BRACKETS_2025`]'s thresholds — 15,600 x 1.2 = 18,720, and so on
/// for every one of them. That is by design rather than coincidence, and it is worth knowing:
/// a future bracket change that leaves these untouched is a signal to go and check, not a saving.
/// The rates themselves are the same five as PAYE.
const NZ_ESCT_2025: &[(i64, i64)] = &[
    (18_720_00, 1_050),
    (64_200_00, 1_750),
    (93_720_00, 3_000),
    (216_000_00, 3_300),
    (i64::MAX, 3_900),
];

/// KiwiSaver employee contribution rates an employee may elect, in basis points.
///
/// 3.5% became the default on 1 April 2026 (rising to 4% on 1 April 2028); 3% remains available
/// on application. Offered as a list because these are the only legal values — a free-text
/// percentage would let someone model a contribution their payroll would refuse.
pub const KIWISAVER_EMPLOYEE_RATES_BPS: &[i64] = &[300, 350, 400, 600, 800, 1_000];

/// The default employee rate, and the employer's compulsory minimum, from 1 April 2026.
pub const KIWISAVER_DEFAULT_BPS: i64 = 350;

/// What one gross annual salary is subject to.
#[derive(Debug, Clone, Copy, Default)]
pub struct PayeInput {
    pub annual_gross_minor: i64,
    /// Employee KiwiSaver contribution, in basis points of gross. 0 for someone not enrolled.
    pub kiwisaver_bps: i64,
    /// The employer's contribution, in basis points of gross.
    ///
    /// Not a deduction from take-home — it is money the employer adds on top — so it does not touch
    /// `net_minor`. It matters because it lands in the KiwiSaver account, which over a thirty-year
    /// horizon is most of the balance. [`KIWISAVER_DEFAULT_BPS`] is the compulsory minimum.
    pub employer_kiwisaver_bps: i64,
    /// Whether IR deducts student loan repayments from this income.
    pub student_loan: bool,
}

/// Every deduction, itemised.
///
/// Itemised rather than reduced to a net figure because the UI has to be able to explain a
/// number the user will not otherwise believe: "modelled take-home is 31% under what you typed"
/// is only actionable if the four components are visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema, Default)]
pub struct PayeBreakdown {
    pub gross_minor: i64,
    pub income_tax_minor: i64,
    pub acc_levy_minor: i64,
    pub kiwisaver_minor: i64,
    pub student_loan_minor: i64,
    pub net_minor: i64,
    /// The employer's gross contribution, before ESCT.
    pub employer_kiwisaver_minor: i64,
    /// ESCT taken off that contribution.
    pub esct_minor: i64,
    /// What actually reaches the KiwiSaver account: the employee's contribution plus the employer's
    /// net of ESCT. This is the figure a projection should credit, and the reason the three
    /// components above are itemised rather than collapsed into it.
    pub kiwisaver_credited_minor: i64,
}

/// The scale in force on `on`, or `None` if the date precedes every scale on record.
///
/// `None` rather than the earliest scale, deliberately: a date before the first entry is a
/// question this table cannot answer, and answering it with rules that had not been written yet
/// would be a guess dressed as a fact.
pub fn scale_for(id: TaxScaleId, on: NaiveDate) -> Option<&'static TaxScale> {
    match id {
        TaxScaleId::None => None,
        TaxScaleId::NzPaye => NZ_TAX_SCALES.iter().rev().find(|s| {
            NaiveDate::parse_from_str(s.effective_from, "%Y-%m-%d").is_ok_and(|d| d <= on)
        }),
    }
}

/// The most recent scale on record, for a caller with no particular date in mind.
pub fn latest_scale(id: TaxScaleId) -> Option<&'static TaxScale> {
    match id {
        TaxScaleId::None => None,
        TaxScaleId::NzPaye => NZ_TAX_SCALES.last(),
    }
}

/// `bps` basis points of `amount_minor`, rounded half away from zero.
///
/// Rounded rather than truncated because every published figure these are checked against is
/// rounded — ACC's stated maximum levy is $2,741.22, and truncating gives $2,741.21. Beyond
/// matching the sources, truncation biases every one of the four deductions *downward*, which
/// overstates take-home: the one direction a forecast should not err in.
fn bps_of(amount_minor: i64, bps: i64) -> i64 {
    let product = amount_minor * bps;
    if product >= 0 {
        (product + 5_000) / 10_000
    } else {
        (product - 5_000) / 10_000
    }
}

/// Progressive income tax on `annual_gross_minor`.
///
/// Each bracket taxes only the slice of income inside it, which is what "progressive" means and
/// what a single average rate cannot express.
fn income_tax_minor(scale: &TaxScale, annual_gross_minor: i64) -> i64 {
    let mut tax = 0i64;
    let mut lower = 0i64;
    for &(upper, rate_bps) in scale.brackets {
        if annual_gross_minor <= lower {
            break;
        }
        let slice = annual_gross_minor.min(upper) - lower;
        tax += bps_of(slice, rate_bps);
        lower = upper;
    }
    tax
}

/// Every deduction applied to one gross annual salary.
///
/// Negative or zero gross yields an all-zero breakdown rather than negative tax: a stream cannot
/// pay less than nothing, and the DB refuses a non-positive `gross_annual_minor` anyway, so this
/// is the belt-and-braces branch for a value that arrived some other way.
pub fn paye(scale: &TaxScale, input: PayeInput) -> PayeBreakdown {
    let gross = input.annual_gross_minor;
    if gross <= 0 {
        return PayeBreakdown::default();
    }
    let income_tax = income_tax_minor(scale, gross);
    // The levy is charged on liable earnings only, and stops at the cap — which is why a
    // marginal rate above it is genuinely lower than the one below it.
    let acc = bps_of(gross.min(scale.acc_income_cap_minor), scale.acc_levy_bps);
    // On gross, before tax — a KiwiSaver contribution is not a deduction from take-home, it is a
    // slice of gross that never becomes take-home.
    let kiwisaver = bps_of(gross, input.kiwisaver_bps.max(0));
    let employer_ks = bps_of(gross, input.employer_kiwisaver_bps.max(0));
    // The rate is chosen by the employee's total — salary plus the employer's contribution — and
    // applied flat, not sliced progressively like income tax. In a projection the current annual
    // figures stand in for IR's "prior year plus expected contributions", which is the same number
    // for anyone whose pay is not changing and the closest available one for anyone whose is.
    let esct = bps_of(employer_ks, esct_rate_bps(scale, gross + employer_ks));
    let student_loan = if input.student_loan {
        bps_of(
            (gross - scale.student_loan_threshold_minor).max(0),
            scale.student_loan_rate_bps,
        )
    } else {
        0
    };
    PayeBreakdown {
        gross_minor: gross,
        income_tax_minor: income_tax,
        acc_levy_minor: acc,
        kiwisaver_minor: kiwisaver,
        student_loan_minor: student_loan,
        // The employer's contribution is deliberately absent here: it was never the employee's to
        // take home, so adding it would overstate spendable income by 3.5% of gross.
        net_minor: gross - income_tax - acc - kiwisaver - student_loan,
        employer_kiwisaver_minor: employer_ks,
        esct_minor: esct,
        kiwisaver_credited_minor: kiwisaver + employer_ks - esct,
    }
}

/// The flat ESCT rate for an employee whose salary plus employer contributions total
/// `esct_income_minor`.
pub fn esct_rate_bps(scale: &TaxScale, esct_income_minor: i64) -> i64 {
    scale
        .esct_brackets
        .iter()
        .find(|&&(upper, _)| esct_income_minor <= upper)
        .map(|&(_, rate)| rate)
        // Unreachable: the last bracket's bound is `i64::MAX`. The top rate is the safe direction
        // to fail if someone appends a table without one.
        .unwrap_or_else(|| scale.esct_brackets.last().map(|&(_, r)| r).unwrap_or(3_900))
}

/// Take-home as a fraction of gross, in basis points — the *average* rate at this salary.
pub fn average_take_home_bps(scale: &TaxScale, input: PayeInput) -> i64 {
    let b = paye(scale, input);
    if b.gross_minor <= 0 {
        return 10_000;
    }
    b.net_minor * 10_000 / b.gross_minor
}

/// What a *raise* keeps, in basis points — the marginal rate at this salary.
///
/// Measured rather than looked up: the combined marginal rate is the income tax bracket plus
/// ACC (only below the cap) plus KiwiSaver plus student loan, and the cap makes the combination
/// non-monotonic in a way that is easy to get wrong by hand. So this differences the actual
/// function over a small step, which cannot disagree with [`paye`] by construction.
///
/// A dollar step would round to nothing against integer bps arithmetic; $1 000 is small enough
/// that it sits inside one bracket everywhere except exactly at a threshold, and large enough to
/// difference cleanly.
pub fn marginal_take_home_bps(scale: &TaxScale, input: PayeInput) -> i64 {
    const STEP_MINOR: i64 = 1_000_00;
    let base = paye(scale, input);
    let stepped = paye(
        scale,
        PayeInput {
            annual_gross_minor: input.annual_gross_minor.max(0) + STEP_MINOR,
            ..input
        },
    );
    (stepped.net_minor - base.net_minor) * 10_000 / STEP_MINOR
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scale() -> &'static TaxScale {
        latest_scale(TaxScaleId::NzPaye).expect("NZ scale exists")
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The brackets are the one thing here that cannot be derived, so they are checked against
    /// hand-computed figures rather than against the function that produced them.
    #[test]
    fn income_tax_is_progressive_at_every_bracket_boundary() {
        let s = scale();
        // Exactly at the first threshold: the whole income at 10.5%.
        assert_eq!(income_tax_minor(s, 15_600_00), 1_638_00);
        // One bracket up: 15,600 @ 10.5% + 37,900 @ 17.5%.
        assert_eq!(income_tax_minor(s, 53_500_00), 1_638_00 + 6_632_50);
        // …and again: + 24,600 @ 30%.
        assert_eq!(
            income_tax_minor(s, 78_100_00),
            1_638_00 + 6_632_50 + 7_380_00
        );
        // …and again: + 101,900 @ 33%.
        assert_eq!(
            income_tax_minor(s, 180_000_00),
            1_638_00 + 6_632_50 + 7_380_00 + 33_627_00
        );
        // Into the top rate: + 20,000 @ 39%.
        assert_eq!(
            income_tax_minor(s, 200_000_00),
            1_638_00 + 6_632_50 + 7_380_00 + 33_627_00 + 7_800_00
        );
        // Zero and below are not negative tax.
        assert_eq!(income_tax_minor(s, 0), 0);
    }

    /// A salary one dollar into a bracket must not be taxed as if all of it were — the mistake a
    /// flat "your bracket × your income" would make, and the one this whole table exists to
    /// avoid.
    #[test]
    fn crossing_a_threshold_taxes_only_the_slice_above_it() {
        let s = scale();
        let below = income_tax_minor(s, 53_500_00);
        let above = income_tax_minor(s, 53_501_00);
        // 30% of one dollar is thirty cents — not 30% of the whole $53,501.
        assert_eq!(above - below, 30);
    }

    #[test]
    fn the_acc_levy_stops_at_the_cap() {
        let s = scale();
        let at_cap = paye(
            s,
            PayeInput {
                annual_gross_minor: s.acc_income_cap_minor,
                kiwisaver_bps: 0,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        let over_cap = paye(
            s,
            PayeInput {
                annual_gross_minor: s.acc_income_cap_minor + 50_000_00,
                kiwisaver_bps: 0,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        assert_eq!(at_cap.acc_levy_minor, over_cap.acc_levy_minor);
        // The published maximum for 2026-27 is $2,741.22 — the arithmetic must reproduce it.
        assert_eq!(over_cap.acc_levy_minor, 2_741_22);
    }

    #[test]
    fn student_loan_applies_only_above_the_threshold() {
        let s = scale();
        let mk = |gross: i64| {
            paye(
                s,
                PayeInput {
                    annual_gross_minor: gross,
                    kiwisaver_bps: 0,
                    employer_kiwisaver_bps: 0,
                    student_loan: true,
                },
            )
            .student_loan_minor
        };
        assert_eq!(mk(s.student_loan_threshold_minor), 0);
        assert_eq!(mk(s.student_loan_threshold_minor - 10_000_00), 0);
        // 12% of the excess, not of the whole salary.
        assert_eq!(mk(s.student_loan_threshold_minor + 10_000_00), 1_200_00);
    }

    #[test]
    fn kiwisaver_comes_off_gross_not_off_take_home() {
        let s = scale();
        let none = paye(
            s,
            PayeInput {
                annual_gross_minor: 100_000_00,
                kiwisaver_bps: 0,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        let with = paye(
            s,
            PayeInput {
                annual_gross_minor: 100_000_00,
                kiwisaver_bps: KIWISAVER_DEFAULT_BPS,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        // 3.5% of gross, and income tax is unchanged by it — the contribution is not deductible.
        assert_eq!(with.kiwisaver_minor, 3_500_00);
        assert_eq!(with.income_tax_minor, none.income_tax_minor);
        assert_eq!(with.net_minor, none.net_minor - 3_500_00);
    }

    /// Every component must reconcile, or an itemised breakdown is worse than none.
    #[test]
    fn the_breakdown_sums_to_net() {
        let s = scale();
        for gross in [1_00, 20_000_00, 60_000_00, 135_000_00, 400_000_00] {
            let b = paye(
                s,
                PayeInput {
                    annual_gross_minor: gross,
                    kiwisaver_bps: KIWISAVER_DEFAULT_BPS,
                    employer_kiwisaver_bps: 0,
                    student_loan: true,
                },
            );
            assert_eq!(
                b.net_minor,
                b.gross_minor
                    - b.income_tax_minor
                    - b.acc_levy_minor
                    - b.kiwisaver_minor
                    - b.student_loan_minor,
                "components did not reconcile at {gross}"
            );
            assert!(b.net_minor > 0, "net went non-positive at {gross}");
        }
    }

    /// The reason this module exists: the marginal rate and the average rate are different
    /// numbers, and a promotion is priced by the marginal one.
    #[test]
    fn the_marginal_rate_is_below_the_average_rate_on_a_progressive_scale() {
        let s = scale();
        let input = PayeInput {
            annual_gross_minor: 135_000_00,
            kiwisaver_bps: KIWISAVER_DEFAULT_BPS,
            employer_kiwisaver_bps: 0,
            student_loan: true,
        };
        let avg = average_take_home_bps(s, input);
        let marginal = marginal_take_home_bps(s, input);
        assert!(
            marginal < avg,
            "marginal {marginal} should keep less than average {avg}"
        );
        // 33% tax + 1.75% ACC + 3.5% KiwiSaver + 12% student loan keeps 49.75%.
        assert_eq!(marginal, 4_975);
    }

    /// Above the ACC cap a raise keeps *more* than one just below it, because the levy has
    /// stopped. Differencing the real function is what gets this right; a lookup table of
    /// "bracket rate" would not.
    #[test]
    fn the_marginal_rate_rises_again_above_the_acc_cap() {
        let s = scale();
        let mk = |gross: i64| {
            marginal_take_home_bps(
                s,
                PayeInput {
                    annual_gross_minor: gross,
                    kiwisaver_bps: 0,
                    employer_kiwisaver_bps: 0,
                    student_loan: false,
                },
            )
        };
        let below = mk(s.acc_income_cap_minor - 20_000_00);
        let above = mk(s.acc_income_cap_minor + 20_000_00);
        assert!(above > below, "expected {above} > {below} past the cap");
        assert_eq!(above - below, s.acc_levy_bps);
    }

    /// ESCT is a *flat* rate picked by which bracket the total lands in — not a progressive slice.
    /// Treating it like income tax would understate it substantially at every level.
    #[test]
    fn esct_is_a_flat_rate_chosen_by_bracket() {
        let s = scale();
        assert_eq!(esct_rate_bps(s, 10_000_00), 1_050);
        assert_eq!(esct_rate_bps(s, 18_720_00), 1_050);
        assert_eq!(esct_rate_bps(s, 18_721_00), 1_750);
        assert_eq!(esct_rate_bps(s, 64_200_00), 1_750);
        assert_eq!(esct_rate_bps(s, 93_720_00), 3_000);
        assert_eq!(esct_rate_bps(s, 216_000_00), 3_300);
        assert_eq!(esct_rate_bps(s, 500_000_00), 3_900);
    }

    /// The ESCT thresholds sit exactly 20% above the income tax thresholds. Pinned because it is a
    /// deliberate relationship: if a future bracket change breaks it, that is worth being told about
    /// rather than silently carrying two tables that have drifted apart.
    #[test]
    fn the_esct_thresholds_are_twenty_percent_above_the_paye_thresholds() {
        let s = scale();
        for (paye, esct) in s.brackets.iter().zip(s.esct_brackets.iter()) {
            if paye.0 == i64::MAX {
                assert_eq!(esct.0, i64::MAX);
                continue;
            }
            assert_eq!(esct.0, paye.0 * 12 / 10, "threshold {paye:?} vs {esct:?}");
            assert_eq!(esct.1, paye.1, "rates should match: {paye:?} vs {esct:?}");
        }
    }

    /// The whole point of modelling the employer side: what reaches the account is the contribution
    /// **less** ESCT. Getting this backwards overstates a KiwiSaver balance by up to 39% of every
    /// employer dollar.
    #[test]
    fn esct_comes_off_the_employer_contribution_not_on_top_of_it() {
        let s = scale();
        let b = paye(
            s,
            PayeInput {
                annual_gross_minor: 100_000_00,
                kiwisaver_bps: KIWISAVER_DEFAULT_BPS,
                employer_kiwisaver_bps: KIWISAVER_DEFAULT_BPS,
                student_loan: false,
            },
        );
        // 3.5% each way on $100k.
        assert_eq!(b.kiwisaver_minor, 3_500_00);
        assert_eq!(b.employer_kiwisaver_minor, 3_500_00);
        // $103,500 total puts the ESCT rate at 33%.
        assert_eq!(b.esct_minor, 1_155_00);
        // The account receives both contributions, less the tax on the employer's.
        assert_eq!(b.kiwisaver_credited_minor, 3_500_00 + 3_500_00 - 1_155_00);
        assert!(b.kiwisaver_credited_minor < b.kiwisaver_minor + b.employer_kiwisaver_minor);
    }

    /// The employer's contribution is not the employee's to spend, so it must not appear in
    /// take-home — an easy and expensive mistake to make while adding it.
    #[test]
    fn the_employer_contribution_does_not_change_take_home() {
        let s = scale();
        let mk = |employer_bps: i64| {
            paye(
                s,
                PayeInput {
                    annual_gross_minor: 90_000_00,
                    kiwisaver_bps: 300,
                    employer_kiwisaver_bps: employer_bps,
                    student_loan: true,
                },
            )
        };
        assert_eq!(mk(0).net_minor, mk(KIWISAVER_DEFAULT_BPS).net_minor);
        assert_eq!(mk(0).kiwisaver_credited_minor, 90_000_00 * 300 / 10_000);
    }

    #[test]
    fn the_scale_in_force_is_the_latest_one_not_after_the_date() {
        assert_eq!(
            scale_for(TaxScaleId::NzPaye, d("2025-06-01"))
                .unwrap()
                .effective_from,
            "2025-04-01"
        );
        assert_eq!(
            scale_for(TaxScaleId::NzPaye, d("2026-08-05"))
                .unwrap()
                .effective_from,
            "2026-04-01"
        );
        // A date before every scale is unanswerable, not silently answered with the oldest.
        assert!(scale_for(TaxScaleId::NzPaye, d("2001-01-01")).is_none());
        // And `None` means no model, at any date.
        assert!(scale_for(TaxScaleId::None, d("2026-08-05")).is_none());
    }

    #[test]
    fn the_scales_are_in_ascending_date_order() {
        // `scale_for` walks them in reverse and takes the first match, which is only correct if
        // they are ordered. Appending an out-of-order entry would break it silently.
        let dates: Vec<NaiveDate> = NZ_TAX_SCALES.iter().map(|s| d(s.effective_from)).collect();
        assert!(dates.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn tax_scale_id_round_trips_through_text() {
        for id in [TaxScaleId::NzPaye, TaxScaleId::None] {
            assert_eq!(TaxScaleId::from_str(id.as_str()), Ok(id));
        }
        assert!(TaxScaleId::from_str("uk_paye").is_err());
    }
}
