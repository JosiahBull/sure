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
///
/// Borrowed rather than owned, so the built-in constants below cost nothing. `sure-dal` stores an
/// editable copy and hands out `&'static`-free equivalents via [`OwnedTaxScale`]; both satisfy the
/// same functions because everything here reads through slices.
#[derive(Debug, Clone, Copy)]
pub struct TaxScale<'a> {
    /// ISO-8601 date this scale takes effect. Scales are held in ascending order and the latest
    /// one not after the date being asked about wins.
    pub effective_from: &'static str,
    /// `(inclusive_upper_bound_annual_minor, rate_bps)`, ascending. The final bound is
    /// [`i64::MAX`], so there is no "and above" special case to forget.
    pub brackets: &'a [(i64, i64)],
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
    pub esct_brackets: &'a [(i64, i64)],
    /// The compulsory employer contribution: the least an employer may pay into a contributing
    /// member's KiwiSaver account, in basis points of gross.
    ///
    /// Dated because it moves — 3% until 31 March 2026, 3.5% from 1 April 2026, 4% from 1 April
    /// 2028 — and a rate that moves on a date is the one thing a constant cannot express.
    ///
    /// Read by nobody in this module: what an employer actually pays is per-job, and plenty pay
    /// above it or (for a total-remuneration package, or a member under 18) below it, so
    /// [`PayeInput::employer_kiwisaver_bps`] carries the real figure and this is the statutory
    /// reference the UI offers as a default. Kept on the scale rather than beside it because it
    /// changes on the same dates and by the same mechanism as everything else here.
    pub kiwisaver_employer_min_bps: i64,
    /// What the government adds per dollar the *member* contributes, in basis points.
    ///
    /// Employer contributions deliberately do not count toward it — easy to miss, and it would
    /// otherwise overstate the credit for anyone whose employer matches them.
    pub kiwisaver_govt_match_bps: i64,
    /// The annual ceiling on that contribution.
    pub kiwisaver_govt_max_minor: i64,
    /// Taxable income above which no government contribution is paid at all. [`i64::MAX`] for the
    /// years before an income test existed.
    pub kiwisaver_govt_income_cap_minor: i64,
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
pub const NZ_TAX_SCALES: &[TaxScale<'static>] = &[
    TaxScale {
        effective_from: "2025-04-01",
        brackets: NZ_BRACKETS_2025,
        // 1.67% incl GST, capped at $152,790 — the 2025-26 figures.
        acc_levy_bps: 167,
        acc_income_cap_minor: 152_790_00,
        student_loan_threshold_minor: 24_128_00,
        student_loan_rate_bps: 1_200,
        esct_brackets: NZ_ESCT_2025,
        kiwisaver_employer_min_bps: 300,
        // The pre-Budget-2025 government contribution: 50c per member dollar, capped at $521.43,
        // with no income test.
        kiwisaver_govt_match_bps: 5_000,
        kiwisaver_govt_max_minor: 521_43,
        kiwisaver_govt_income_cap_minor: i64::MAX,
    },
    // Budget 2025 halved the government contribution and added an income test, from 1 July 2025 —
    // which lands mid-tax-year. A scale starting on that date is exactly what dated scales are for:
    // the alternative is one entry that is wrong for a quarter of its own span.
    TaxScale {
        effective_from: "2025-07-01",
        brackets: NZ_BRACKETS_2025,
        acc_levy_bps: 167,
        acc_income_cap_minor: 152_790_00,
        student_loan_threshold_minor: 24_128_00,
        student_loan_rate_bps: 1_200,
        esct_brackets: NZ_ESCT_2025,
        kiwisaver_employer_min_bps: 300,
        kiwisaver_govt_match_bps: 2_500,
        kiwisaver_govt_max_minor: 260_72,
        kiwisaver_govt_income_cap_minor: 180_000_00,
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
        // Budget 2025's second KiwiSaver change, and the one that lands on the tax-year boundary
        // rather than mid-year: the compulsory employer contribution steps 3% -> 3.5% here.
        kiwisaver_employer_min_bps: 350,
        kiwisaver_govt_match_bps: 2_500,
        kiwisaver_govt_max_minor: 260_72,
        kiwisaver_govt_income_cap_minor: 180_000_00,
    },
    // The second half of Budget 2025's KiwiSaver step, legislated on the same day as the first:
    // 3.5% -> 4%. Carried here rather than left for someone to type in 2028, because a projection
    // run today already spans the date and would otherwise price years 2028+ at 3.5%.
    //
    // **Only the KiwiSaver rate is a 2028 figure.** Everything else is the 2026-27 scale carried
    // forward unchanged, because it has not been published yet — the ACC levy and cap in
    // particular are reset annually and will not really be these. That is the honest default (a
    // projection has to price 2028 as *something*, and this year's rates are the least-wrong
    // guess) but it is a carry-forward, not a source, and this comment is the difference.
    TaxScale {
        effective_from: "2028-04-01",
        brackets: NZ_BRACKETS_2025,
        acc_levy_bps: 175,
        acc_income_cap_minor: 156_641_00,
        student_loan_threshold_minor: 24_128_00,
        student_loan_rate_bps: 1_200,
        esct_brackets: NZ_ESCT_2025,
        kiwisaver_employer_min_bps: 400,
        kiwisaver_govt_match_bps: 2_500,
        kiwisaver_govt_max_minor: 260_72,
        kiwisaver_govt_income_cap_minor: 180_000_00,
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

/// A tax scale that owns its brackets — what a stored, user-editable one deserialises into.
///
/// The same fields as [`TaxScale`], and [`OwnedTaxScale::as_scale`] hands back a borrowed view, so
/// every function in this module works on either. That is the whole reason the built-in constants
/// are not simply deleted once the table exists: they remain the seed, the fallback for an empty
/// table, and the thing the tests are written against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnedTaxScale {
    pub effective_from: String,
    /// `(upper_bound_annual_minor, rate_bps)`. `None` on the bound means "and above".
    pub brackets: Vec<(Option<i64>, i64)>,
    pub acc_levy_bps: i64,
    pub acc_income_cap_minor: i64,
    pub student_loan_threshold_minor: i64,
    pub student_loan_rate_bps: i64,
    pub esct_brackets: Vec<(Option<i64>, i64)>,
    /// The compulsory employer contribution — see [`TaxScale::kiwisaver_employer_min_bps`].
    pub kiwisaver_employer_min_bps: i64,
    pub kiwisaver_govt_match_bps: i64,
    pub kiwisaver_govt_max_minor: i64,
    /// `None` for "no income test", which is what [`i64::MAX`] means internally — nobody wants to
    /// see 9223372036854775807 in a settings field.
    pub kiwisaver_govt_income_cap_minor: Option<i64>,
}

impl OwnedTaxScale {
    /// Flatten `None` bounds back to [`i64::MAX`] for the arithmetic.
    fn bounded(brackets: &[(Option<i64>, i64)]) -> Vec<(i64, i64)> {
        brackets
            .iter()
            .map(|&(upper, rate)| (upper.unwrap_or(i64::MAX), rate))
            .collect()
    }

    /// Every way a stored scale can be nonsense, collected at once.
    ///
    /// Checked here rather than by the database because the interesting failures are *relationships*
    /// — an unordered bracket list, a table with no open top — which a column CHECK cannot see.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        let check = |name: &str, b: &[(Option<i64>, i64)], problems: &mut Vec<String>| {
            if b.is_empty() {
                problems.push(format!("{name} must have at least one band"));
                return;
            }
            // The last band has to be open, or income above it is taxed at nothing at all.
            if b.last().is_some_and(|&(upper, _)| upper.is_some()) {
                problems.push(format!(
                    "{name}'s last band must be open-ended — income above it would otherwise be untaxed"
                ));
            }
            let bounds = Self::bounded(b);
            if bounds.windows(2).any(|w| w[0].0 >= w[1].0) {
                problems.push(format!("{name}'s bands must be in ascending order"));
            }
            if bounds
                .iter()
                .any(|&(_, rate)| !(0..=10_000).contains(&rate))
            {
                problems.push(format!("{name}'s rates must be between 0 and 100%"));
            }
            if bounds.iter().any(|&(upper, _)| upper < 0) {
                problems.push(format!("{name}'s bands cannot be negative"));
            }
        };
        check("brackets", &self.brackets, &mut problems);
        check("esct_brackets", &self.esct_brackets, &mut problems);
        if !(0..=10_000).contains(&self.acc_levy_bps) {
            problems.push("acc_levy_bps must be between 0 and 100%".into());
        }
        if !(0..=10_000).contains(&self.student_loan_rate_bps) {
            problems.push("student_loan_rate_bps must be between 0 and 100%".into());
        }
        if !(0..=10_000).contains(&self.kiwisaver_govt_match_bps) {
            problems.push("kiwisaver_govt_match_bps must be between 0 and 100%".into());
        }
        if !(0..=10_000).contains(&self.kiwisaver_employer_min_bps) {
            problems.push("kiwisaver_employer_min_bps must be between 0 and 100%".into());
        }
        if self.kiwisaver_govt_max_minor < 0 {
            problems.push("the government contribution cap cannot be negative".into());
        }
        if self.acc_income_cap_minor < 0 || self.student_loan_threshold_minor < 0 {
            problems.push("caps and thresholds cannot be negative".into());
        }
        if crate::iso_date::IsoDate::parse(&self.effective_from).is_err() {
            problems.push(format!(
                "effective_from must be an ISO-8601 date, got {:?}",
                self.effective_from
            ));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

impl From<&TaxScale<'_>> for OwnedTaxScale {
    fn from(s: &TaxScale<'_>) -> Self {
        let open = |b: &[(i64, i64)]| -> Vec<(Option<i64>, i64)> {
            b.iter()
                .map(|&(upper, rate)| ((upper != i64::MAX).then_some(upper), rate))
                .collect()
        };
        OwnedTaxScale {
            effective_from: s.effective_from.to_string(),
            brackets: open(s.brackets),
            acc_levy_bps: s.acc_levy_bps,
            acc_income_cap_minor: s.acc_income_cap_minor,
            student_loan_threshold_minor: s.student_loan_threshold_minor,
            student_loan_rate_bps: s.student_loan_rate_bps,
            esct_brackets: open(s.esct_brackets),
            kiwisaver_employer_min_bps: s.kiwisaver_employer_min_bps,
            kiwisaver_govt_match_bps: s.kiwisaver_govt_match_bps,
            kiwisaver_govt_max_minor: s.kiwisaver_govt_max_minor,
            kiwisaver_govt_income_cap_minor: (s.kiwisaver_govt_income_cap_minor != i64::MAX)
                .then_some(s.kiwisaver_govt_income_cap_minor),
        }
    }
}

/// A resolved scale the arithmetic can run against, owning its flattened bracket tables.
///
/// [`paye`] and friends take `&TaxScale`, which borrows `&'static [(i64, i64)]`. A stored scale's
/// brackets are neither static nor pre-flattened, so this holds them alive for the borrow.
pub struct ResolvedScale {
    brackets: Vec<(i64, i64)>,
    esct: Vec<(i64, i64)>,
    effective_from: String,
    acc_levy_bps: i64,
    acc_income_cap_minor: i64,
    student_loan_threshold_minor: i64,
    student_loan_rate_bps: i64,
    kiwisaver_employer_min_bps: i64,
    kiwisaver_govt_match_bps: i64,
    kiwisaver_govt_max_minor: i64,
    kiwisaver_govt_income_cap_minor: i64,
}

impl ResolvedScale {
    pub fn new(owned: &OwnedTaxScale) -> Self {
        ResolvedScale {
            brackets: OwnedTaxScale::bounded(&owned.brackets),
            esct: OwnedTaxScale::bounded(&owned.esct_brackets),
            effective_from: owned.effective_from.clone(),
            acc_levy_bps: owned.acc_levy_bps,
            acc_income_cap_minor: owned.acc_income_cap_minor,
            student_loan_threshold_minor: owned.student_loan_threshold_minor,
            student_loan_rate_bps: owned.student_loan_rate_bps,
            kiwisaver_employer_min_bps: owned.kiwisaver_employer_min_bps,
            kiwisaver_govt_match_bps: owned.kiwisaver_govt_match_bps,
            kiwisaver_govt_max_minor: owned.kiwisaver_govt_max_minor,
            kiwisaver_govt_income_cap_minor: owned
                .kiwisaver_govt_income_cap_minor
                .unwrap_or(i64::MAX),
        }
    }

    /// A borrowed view for the arithmetic. Lives as long as `self`.
    pub fn as_scale(&self) -> TaxScale<'_> {
        TaxScale {
            // A leaked-free borrow is impossible through `&'static str`, so the date — which no
            // calculation reads — is the one field that has to be static. It is only ever used for
            // ordering, which `scale_for` does before this point.
            effective_from: "",
            brackets: &self.brackets,
            acc_levy_bps: self.acc_levy_bps,
            acc_income_cap_minor: self.acc_income_cap_minor,
            student_loan_threshold_minor: self.student_loan_threshold_minor,
            student_loan_rate_bps: self.student_loan_rate_bps,
            esct_brackets: &self.esct,
            kiwisaver_employer_min_bps: self.kiwisaver_employer_min_bps,
            kiwisaver_govt_match_bps: self.kiwisaver_govt_match_bps,
            kiwisaver_govt_max_minor: self.kiwisaver_govt_max_minor,
            kiwisaver_govt_income_cap_minor: self.kiwisaver_govt_income_cap_minor,
        }
    }

    pub fn effective_from(&self) -> &str {
        &self.effective_from
    }
}

/// A stored scale on the wire: the rules, plus the row identity needed to edit them.
///
/// Here rather than in `sure-dal` because it is wire vocabulary — the DAL has no `utoipa`, and that
/// missing dependency is the layering rule making itself felt rather than an oversight to paper over.
#[derive(Debug, Serialize, ToSchema, PartialEq, Eq)]
pub struct StoredTaxScale {
    pub id: i64,
    pub scale_id: TaxScaleId,
    /// Where these figures came from, so a future reader can check them rather than trust them.
    pub source_note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(flatten)]
    pub scale: OwnedTaxScale,
}

/// Write body for a tax scale.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveTaxScale {
    #[serde(flatten)]
    pub scale: OwnedTaxScale,
    #[serde(default)]
    pub source_note: Option<String>,
}

/// The built-in scales as editable values — what the DAL seeds an empty table with.
pub fn builtin_scales(id: TaxScaleId) -> Vec<OwnedTaxScale> {
    match id {
        TaxScaleId::None => Vec::new(),
        TaxScaleId::NzPaye => NZ_TAX_SCALES.iter().map(OwnedTaxScale::from).collect(),
    }
}

/// KiwiSaver employee contribution rates an employee may elect, in basis points.
///
/// 3.5% became the default on 1 April 2026 (rising to 4% on 1 April 2028); 3% remains available
/// on application. Offered as a list because these are the only legal values — a free-text
/// percentage would let someone model a contribution their payroll would refuse.
pub const KIWISAVER_EMPLOYEE_RATES_BPS: &[i64] = &[300, 350, 400, 600, 800, 1_000];

/// The default *employee* rate from 1 April 2026 — what someone is enrolled at absent an election.
///
/// The employer's compulsory minimum happens to be the same 3.5% today, and steps to 4% on the same
/// date in 2028, but it is a separate statutory rate that could move on its own. It lives on the
/// dated scale as [`TaxScale::kiwisaver_employer_min_bps`]; do not reach for this constant for it.
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
    /// horizon is most of the balance. The legal floor is
    /// [`TaxScale::kiwisaver_employer_min_bps`], but this is not clamped to it: a total-remuneration
    /// package, a member under 18 or over 65, and a contractor all legitimately sit below.
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
    /// The government's annual contribution, matched against the *member's* own contributions only.
    pub govt_contribution_minor: i64,
    /// What actually reaches the KiwiSaver account: the employee's contribution, the employer's net
    /// of ESCT, and the government's. This is the figure a projection should credit, and the reason the three
    /// components above are itemised rather than collapsed into it.
    pub kiwisaver_credited_minor: i64,
}

/// The scale in force on `on`, or `None` if the date precedes every scale on record.
///
/// `None` rather than the earliest scale, deliberately: a date before the first entry is a
/// question this table cannot answer, and answering it with rules that had not been written yet
/// would be a guess dressed as a fact.
pub fn scale_for(id: TaxScaleId, on: NaiveDate) -> Option<&'static TaxScale<'static>> {
    match id {
        TaxScaleId::None => None,
        TaxScaleId::NzPaye => NZ_TAX_SCALES.iter().rev().find(|s| {
            NaiveDate::parse_from_str(s.effective_from, "%Y-%m-%d").is_ok_and(|d| d <= on)
        }),
    }
}

/// The most recent scale on record, for a caller with no particular date in mind.
pub fn latest_scale(id: TaxScaleId) -> Option<&'static TaxScale<'static>> {
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
fn income_tax_minor(scale: &TaxScale<'_>, annual_gross_minor: i64) -> i64 {
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
pub fn paye(scale: &TaxScale<'_>, input: PayeInput) -> PayeBreakdown {
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
    let govt = govt_contribution_minor(scale, gross, kiwisaver);
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
        govt_contribution_minor: govt,
        kiwisaver_credited_minor: kiwisaver + employer_ks - esct + govt,
    }
}

/// `value / divisor`, rounded half away from zero — the division counterpart of [`bps_of`].
///
/// Same rationale: payroll's published per-period figures are rounded, and truncation would bias
/// every per-period deduction downward, overstating take-home in the one direction a
/// reconstruction should not err.
fn div_round(value: i64, divisor: i64) -> i64 {
    let divisor = divisor.max(1);
    if value >= 0 {
        (value + divisor / 2) / divisor
    } else {
        (value - divisor / 2) / divisor
    }
}

/// One pay period of a regular salary: what that single payslip is subject to.
///
/// The per-period counterpart of [`PayeInput`]. IRD's formula method annualises the period's
/// gross (× the pay frequency's divisor), prices the whole year at that level, and divides back —
/// which is what makes a mid-year pay rise tax correctly from its first payslip instead of being
/// smeared across the year, and why this takes a *period* gross rather than an annual one.
#[derive(Debug, Clone, Copy)]
pub struct PeriodPayeInput {
    pub period_gross_minor: i64,
    /// Payroll's divisor — 24 for semi-monthly, 26 for fortnightly. These are
    /// [`crate::income::PayFrequency::periods_per_year`]'s figures, and the same argument
    /// applies: they are what payroll divides by, not calendar arithmetic.
    pub periods_per_year: i64,
    /// Employee KiwiSaver contribution, in basis points of gross. 0 for someone not enrolled.
    pub kiwisaver_bps: i64,
    /// The employer's contribution — never part of take-home, see
    /// [`PayeInput::employer_kiwisaver_bps`].
    pub employer_kiwisaver_bps: i64,
    /// Whether IR deducts student loan repayments from this income.
    pub student_loan: bool,
}

/// Every deduction applied to one regular payslip — [`paye`], at payroll's own granularity.
///
/// Two per-period rules differ from a naive `paye(...) / periods`:
///   * the ACC cap and the student-loan threshold apply per period (`annual ÷ periods`), which is
///     how payroll actually administers them — a fortnight over the fortnightly threshold repays,
///     even in a year that ends up under the annual one;
///   * `govt_contribution_minor` is **zero**: the government's KiwiSaver contribution is an
///     explicitly annual credit paid once by IR (see [`govt_contribution_minor`]), not a payslip
///     line, and dividing it across 24 payslips would double-count it the moment anything summed
///     a year of breakdowns against the annual figure. `kiwisaver_credited_minor` here is
///     therefore the payslip's own flows only. ESCT's *rate* stays annualised — it is a flat rate
///     chosen by the annual total, like [`paye`] says.
pub fn paye_period(scale: &TaxScale<'_>, input: PeriodPayeInput) -> PayeBreakdown {
    let gross = input.period_gross_minor;
    if gross <= 0 {
        return PayeBreakdown::default();
    }
    let periods = input.periods_per_year.max(1);
    let annualised = gross.saturating_mul(periods);
    let income_tax = div_round(income_tax_minor(scale, annualised), periods);
    let acc = bps_of(
        gross.min(div_round(scale.acc_income_cap_minor, periods)),
        scale.acc_levy_bps,
    );
    let kiwisaver = bps_of(gross, input.kiwisaver_bps.max(0));
    let employer_ks = bps_of(gross, input.employer_kiwisaver_bps.max(0));
    let esct = bps_of(
        employer_ks,
        esct_rate_bps(
            scale,
            annualised.saturating_add(employer_ks.saturating_mul(periods)),
        ),
    );
    let student_loan = if input.student_loan {
        bps_of(
            (gross - div_round(scale.student_loan_threshold_minor, periods)).max(0),
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
        net_minor: gross - income_tax - acc - kiwisaver - student_loan,
        employer_kiwisaver_minor: employer_ks,
        esct_minor: esct,
        govt_contribution_minor: 0,
        kiwisaver_credited_minor: kiwisaver + employer_ks - esct,
    }
}

/// An extra pay — IRD's term for a lump sum landing inside a regular payslip: a bonus, a back
/// payment, a redundancy.
#[derive(Debug, Clone, Copy)]
pub struct ExtraPayInput {
    /// The regular income the extra pay sits on top of, annualised — for a stable salary, the
    /// annual gross. IRD grosses up the last four weeks' pay; a configured stream's annual level
    /// is the same number for anyone whose pay is not changing and the closest available one for
    /// anyone whose is (the same stand-in [`paye`] makes for ESCT).
    pub annualised_regular_minor: i64,
    /// The lump sum itself.
    pub extra_minor: i64,
    pub kiwisaver_bps: i64,
    pub employer_kiwisaver_bps: i64,
    pub student_loan: bool,
}

/// Every deduction applied to an extra pay. `gross_minor` is the lump sum alone; each figure is
/// the *increment* the lump sum adds on top of the regular pays.
///
/// Income tax is the difference of the progressive function at `regular + extra` and at
/// `regular` — i.e. the extra sliced across whichever brackets it actually spans. (IRD's basic
/// method taxes the whole lump at the single rate of the bracket the total lands in; the sliced
/// calculation is the alternative IR also accepts, modern payroll software's default, and the
/// only one of the two that is monotone — which the net→gross inversion below depends on.)
/// Student loan is 12% of the whole lump with **no** threshold — the threshold is consumed by
/// the regular pays. ACC applies only to the slice still under the annual cap. The government
/// KiwiSaver contribution is zero for the same reason as [`paye_period`].
pub fn extra_pay(scale: &TaxScale<'_>, input: ExtraPayInput) -> PayeBreakdown {
    let extra = input.extra_minor;
    if extra <= 0 {
        return PayeBreakdown::default();
    }
    let base = input.annualised_regular_minor.max(0);
    let total = base.saturating_add(extra);
    let income_tax = income_tax_minor(scale, total) - income_tax_minor(scale, base);
    let acc = bps_of(
        total.min(scale.acc_income_cap_minor) - base.min(scale.acc_income_cap_minor),
        scale.acc_levy_bps,
    );
    let kiwisaver = bps_of(extra, input.kiwisaver_bps.max(0));
    let employer_ks = bps_of(extra, input.employer_kiwisaver_bps.max(0));
    let esct = bps_of(employer_ks, esct_rate_bps(scale, total));
    let student_loan = if input.student_loan {
        bps_of(extra, scale.student_loan_rate_bps)
    } else {
        0
    };
    PayeBreakdown {
        gross_minor: extra,
        income_tax_minor: income_tax,
        acc_levy_minor: acc,
        kiwisaver_minor: kiwisaver,
        student_loan_minor: student_loan,
        net_minor: extra - income_tax - acc - kiwisaver - student_loan,
        employer_kiwisaver_minor: employer_ks,
        esct_minor: esct,
        govt_contribution_minor: 0,
        kiwisaver_credited_minor: kiwisaver + employer_ks - esct,
    }
}

/// The smallest gross whose net under `net_of` reaches `target`, found by binary search.
///
/// Sound because every net function here is non-decreasing in gross: the combined marginal
/// deduction rate of any legal NZ scale is well under 100% (39% top tax + 1.75% ACC + 12% student
/// loan + 10% KiwiSaver at the highest electable rate), so net rises with gross everywhere except
/// integer-rounding plateaus. A user-edited scale whose rates sum past 100% breaks that premise;
/// the search then converges on *a* gross and the caller's residual absorption still reconciles,
/// so the failure is a strange-looking breakdown rather than a wrong total.
fn gross_reaching(target: i64, net_of: impl Fn(i64) -> i64) -> i64 {
    let mut hi = target.max(1);
    // Net is at least ~35% of gross under any sane scale, so a handful of doublings suffice; the
    // bound is a guard against a pathological stored scale, not a tuning parameter.
    for _ in 0..64 {
        if net_of(hi) >= target {
            break;
        }
        hi = hi.saturating_mul(2);
    }
    let mut lo = target.max(0); // deductions are non-negative, so gross ≥ net always
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if net_of(mid) >= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// The breakdown behind an observed take-home amount — [`paye_period`] run backwards.
///
/// For matching a real bank deposit: the deposit is ground truth, so the reconstruction must
/// reconcile to it exactly. The search finds the smallest gross whose modelled net reaches
/// `observed_net_minor`; integer rounding can leave that net a few cents high, and the residual
/// is absorbed into `income_tax_minor` — the largest line, and the only one with no published
/// per-period figure to contradict — so that
/// `gross − tax − acc − kiwisaver − student_loan == observed_net_minor`, always.
///
/// `input.period_gross_minor` is ignored: the gross is the answer, not an argument.
pub fn reconstruct_period(
    scale: &TaxScale<'_>,
    observed_net_minor: i64,
    input: PeriodPayeInput,
) -> PayeBreakdown {
    if observed_net_minor <= 0 {
        return PayeBreakdown::default();
    }
    let gross = gross_reaching(observed_net_minor, |g| {
        paye_period(
            scale,
            PeriodPayeInput {
                period_gross_minor: g,
                ..input
            },
        )
        .net_minor
    });
    let mut b = paye_period(
        scale,
        PeriodPayeInput {
            period_gross_minor: gross,
            ..input
        },
    );
    b.income_tax_minor += b.net_minor - observed_net_minor;
    b.net_minor = observed_net_minor;
    b
}

/// The breakdown behind an observed extra-pay slice of a deposit — [`extra_pay`] run backwards,
/// with the same residual-into-income-tax reconciliation as [`reconstruct_period`].
///
/// `input.extra_minor` is ignored: the lump sum is the answer, not an argument.
pub fn reconstruct_extra_pay(
    scale: &TaxScale<'_>,
    observed_net_minor: i64,
    input: ExtraPayInput,
) -> PayeBreakdown {
    if observed_net_minor <= 0 {
        return PayeBreakdown::default();
    }
    let extra = gross_reaching(observed_net_minor, |e| {
        extra_pay(
            scale,
            ExtraPayInput {
                extra_minor: e,
                ..input
            },
        )
        .net_minor
    });
    let mut b = extra_pay(
        scale,
        ExtraPayInput {
            extra_minor: extra,
            ..input
        },
    );
    b.income_tax_minor += b.net_minor - observed_net_minor;
    b.net_minor = observed_net_minor;
    b
}

/// The government's annual KiwiSaver contribution.
///
/// Matched against the **member's** contributions only — an employer's do not count toward it, which
/// is easy to miss and would overstate the credit for anyone whose employer matches them. Capped
/// twice over: at a flat annual maximum, and by an income test above which nothing is paid.
///
/// Not prorated for a partial membership year or for someone under 18 or over 65. A projection is
/// about whole years ahead, and the eligibility rules that would need a birthday are the sort of
/// thing this model does not carry.
pub fn govt_contribution_minor(
    scale: &TaxScale<'_>,
    annual_gross_minor: i64,
    member_contribution_minor: i64,
) -> i64 {
    if annual_gross_minor > scale.kiwisaver_govt_income_cap_minor {
        return 0;
    }
    bps_of(
        member_contribution_minor.max(0),
        scale.kiwisaver_govt_match_bps,
    )
    .min(scale.kiwisaver_govt_max_minor)
}

/// The flat ESCT rate for an employee whose salary plus employer contributions total
/// `esct_income_minor`.
pub fn esct_rate_bps(scale: &TaxScale<'_>, esct_income_minor: i64) -> i64 {
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
pub fn average_take_home_bps(scale: &TaxScale<'_>, input: PayeInput) -> i64 {
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
pub fn marginal_take_home_bps(scale: &TaxScale<'_>, input: PayeInput) -> i64 {
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

    fn scale() -> &'static TaxScale<'static> {
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
        // The account receives both contributions, less the tax on the employer's, plus the
        // government's — which is capped, and matched against the member's half alone.
        assert_eq!(b.govt_contribution_minor, 260_72);
        assert_eq!(
            b.kiwisaver_credited_minor,
            3_500_00 + 3_500_00 - 1_155_00 + 260_72
        );
        // ESCT still costs more than the government contributes back, at this income.
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
        // With no employer contribution the account still gets the member's plus the government's.
        let member = 90_000_00 * 300 / 10_000;
        assert_eq!(
            mk(0).kiwisaver_credited_minor,
            member + mk(0).govt_contribution_minor
        );
    }

    /// Matched against the member's own contributions only — an employer's do not count, which is
    /// the detail most likely to be got wrong and would overstate the credit for anyone matched.
    #[test]
    fn the_government_matches_only_the_members_own_contributions() {
        let s = scale();
        let b = paye(
            s,
            PayeInput {
                annual_gross_minor: 100_000_00,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 350,
                student_loan: false,
            },
        );
        // $3,500 of member contributions is far past the $1,042.86 needed to max it out, so the
        // credit is the cap — and the employer's $3,500 does not raise it.
        assert_eq!(b.govt_contribution_minor, 260_72);

        // A small contributor gets 25c per dollar rather than the cap.
        assert_eq!(govt_contribution_minor(s, 30_000_00, 400_00), 100_00);
        // …and $1,042.86 is exactly the point where the cap starts binding.
        assert_eq!(govt_contribution_minor(s, 30_000_00, 1_042_86), 260_72);
        assert!(govt_contribution_minor(s, 30_000_00, 1_042_00) < 260_72);
    }

    /// Above the income test nothing is paid at all — not a reduced amount.
    #[test]
    fn the_government_contribution_stops_above_the_income_cap() {
        let s = scale();
        assert_eq!(govt_contribution_minor(s, 180_000_00, 5_000_00), 260_72);
        assert_eq!(govt_contribution_minor(s, 180_000_01, 5_000_00), 0);
    }

    /// Budget 2025 halved the rate and added the income test from 1 July 2025 — mid-tax-year. The
    /// dated scales have to reflect that, or a projection prices a year at rules it never had.
    #[test]
    fn the_budget_2025_change_lands_on_its_own_date_not_the_tax_year_boundary() {
        let before = scale_for(TaxScaleId::NzPaye, d("2025-05-01")).unwrap();
        let after = scale_for(TaxScaleId::NzPaye, d("2025-08-01")).unwrap();
        assert_eq!(before.kiwisaver_govt_match_bps, 5_000);
        assert_eq!(before.kiwisaver_govt_max_minor, 521_43);
        assert_eq!(before.kiwisaver_govt_income_cap_minor, i64::MAX);
        assert_eq!(after.kiwisaver_govt_match_bps, 2_500);
        assert_eq!(after.kiwisaver_govt_max_minor, 260_72);
        assert_eq!(after.kiwisaver_govt_income_cap_minor, 180_000_00);
    }

    /// The compulsory employer contribution steps 3% -> 3.5% -> 4%, each on its own date, and it is
    /// dated so that a projection spanning a change prices each side at the rate that really
    /// applied. Both steps are inside the horizon of any projection run today, so getting the
    /// boundaries wrong by a day is not a theoretical error.
    #[test]
    fn the_compulsory_employer_contribution_steps_up_on_its_own_dates() {
        let at = |s: &str| {
            scale_for(TaxScaleId::NzPaye, d(s))
                .unwrap()
                .kiwisaver_employer_min_bps
        };
        assert_eq!(at("2026-03-31"), 300);
        assert_eq!(at("2026-04-01"), 350);
        assert_eq!(at("2028-03-31"), 350);
        assert_eq!(at("2028-04-01"), 400);
    }

    /// The 2028 scale is the legislated KiwiSaver step plus a carry-forward of everything else,
    /// which is a weaker claim than the published years make. Pinned so that a real 2028 ACC or
    /// bracket figure, when it is published, replaces the carry-forward rather than landing beside
    /// it — this test failing is the reminder.
    #[test]
    fn the_2028_scale_carries_this_years_non_kiwisaver_figures_forward() {
        let published = scale_for(TaxScaleId::NzPaye, d("2026-04-01")).unwrap();
        let projected = scale_for(TaxScaleId::NzPaye, d("2028-04-01")).unwrap();
        assert_eq!(projected.acc_levy_bps, published.acc_levy_bps);
        assert_eq!(
            projected.acc_income_cap_minor,
            published.acc_income_cap_minor
        );
        assert_eq!(
            projected.student_loan_threshold_minor,
            published.student_loan_threshold_minor
        );
        assert_eq!(projected.brackets, published.brackets);
        assert_eq!(projected.esct_brackets, published.esct_brackets);
        // The one figure that is genuinely a 2028 fact.
        assert_ne!(
            projected.kiwisaver_employer_min_bps,
            published.kiwisaver_employer_min_bps
        );
    }

    /// It is a reference figure, not a floor the arithmetic imposes: a total-remuneration package
    /// and a member under 18 both legitimately sit below it, so clamping would overstate their
    /// balances by an employer contribution they never receive.
    #[test]
    fn the_employer_minimum_does_not_clamp_what_an_employer_actually_pays() {
        let s = scale();
        // A real minimum is on the scale — and `paye` still reports nothing, because what the
        // employer pays is `PayeInput`'s business. The dated figures are pinned separately.
        assert!(s.kiwisaver_employer_min_bps > 0);
        let b = paye(
            s,
            PayeInput {
                annual_gross_minor: 100_000_00,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        assert_eq!(b.employer_kiwisaver_minor, 0);
    }

    /// The government's contribution reaches the account, so it belongs in the credited figure —
    /// but it is not income, so it must not touch take-home.
    #[test]
    fn the_government_contribution_is_credited_but_is_not_take_home() {
        let s = scale();
        let with = paye(
            s,
            PayeInput {
                annual_gross_minor: 60_000_00,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        let member = 60_000_00 * 350 / 10_000;
        assert_eq!(
            with.kiwisaver_credited_minor,
            member + with.govt_contribution_minor
        );
        assert_eq!(
            with.net_minor,
            60_000_00 - with.income_tax_minor - with.acc_levy_minor - member
        );
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

    /// A semi-monthly payslip on an invented $96k salary, every line hand-computed. The figures
    /// that differ from `paye(annual) / 24` are the point: the student-loan threshold and the ACC
    /// cap divide per period *before* they apply, because that is how payroll administers them.
    #[test]
    fn a_period_payslip_prices_the_year_and_divides_back() {
        let s = scale();
        let b = paye_period(
            s,
            PeriodPayeInput {
                period_gross_minor: 4_000_00, // $96k over 24 pays
                periods_per_year: 24,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 0,
                student_loan: true,
            },
        );
        // Annual tax on $96k is $21,557.50 (see the progressive test above); ÷24 rounded.
        assert_eq!(b.income_tax_minor, 898_23);
        // Under the per-period ACC cap, so 1.75% of the whole payslip.
        assert_eq!(b.acc_levy_minor, 70_00);
        // 12% of the excess over $24,128/24 = $1,005.33.
        assert_eq!(b.student_loan_minor, 359_36);
        assert_eq!(b.kiwisaver_minor, 140_00);
        assert_eq!(
            b.net_minor,
            4_000_00 - 898_23 - 70_00 - 359_36 - 140_00,
            "the itemised lines must reconcile"
        );
        // The annual credit is not a payslip line — summing 24 of these against the annual
        // breakdown must not count it twice.
        assert_eq!(b.govt_contribution_minor, 0);
    }

    /// Twenty-four payslips are the year, to within accumulated rounding — the per-period method
    /// must not quietly re-price the salary.
    #[test]
    fn a_year_of_payslips_reconciles_with_the_annual_breakdown() {
        let s = scale();
        let annual = paye(
            s,
            PayeInput {
                annual_gross_minor: 96_000_00,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 0,
                student_loan: true,
            },
        );
        let period = paye_period(
            s,
            PeriodPayeInput {
                period_gross_minor: 4_000_00,
                periods_per_year: 24,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 0,
                student_loan: true,
            },
        );
        // Each period rounds by at most half a cent per line; 24 periods × 4 lines bounds the
        // drift well under a dollar.
        assert!((24 * period.income_tax_minor - annual.income_tax_minor).abs() <= 24);
        assert!((24 * period.net_minor - annual.net_minor).abs() <= 96);
    }

    /// An extra pay inside one bracket is taxed at that bracket's rate; one that straddles a
    /// threshold is sliced across it — the difference-of-progressive-functions definition.
    #[test]
    fn an_extra_pay_is_sliced_across_the_brackets_it_spans() {
        let s = scale();
        // $5,000 on top of $96k: entirely inside the 33% bracket.
        let inside = extra_pay(
            s,
            ExtraPayInput {
                annualised_regular_minor: 96_000_00,
                extra_minor: 5_000_00,
                kiwisaver_bps: 350,
                employer_kiwisaver_bps: 0,
                student_loan: true,
            },
        );
        assert_eq!(inside.income_tax_minor, 1_650_00);
        assert_eq!(inside.acc_levy_minor, 87_50);
        // No threshold on an extra pay — the regular pays consumed it. 12% of the whole lump.
        assert_eq!(inside.student_loan_minor, 600_00);
        assert_eq!(inside.kiwisaver_minor, 175_00);
        assert_eq!(
            inside.net_minor,
            5_000_00 - 1_650_00 - 87_50 - 600_00 - 175_00
        );

        // $4,000 on top of $178k: half at 33%, half at 39%.
        let straddling = extra_pay(
            s,
            ExtraPayInput {
                annualised_regular_minor: 178_000_00,
                extra_minor: 4_000_00,
                kiwisaver_bps: 0,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        assert_eq!(straddling.income_tax_minor, 660_00 + 780_00);
    }

    /// ACC on an extra pay stops at whatever headroom the regular pays left under the cap.
    #[test]
    fn an_extra_pay_pays_acc_only_into_the_caps_headroom() {
        let s = scale();
        let b = extra_pay(
            s,
            ExtraPayInput {
                annualised_regular_minor: 155_000_00,
                extra_minor: 5_000_00,
                kiwisaver_bps: 0,
                employer_kiwisaver_bps: 0,
                student_loan: false,
            },
        );
        // Only $1,641 of the $5,000 is still under the $156,641 cap.
        assert_eq!(b.acc_levy_minor, 28_72);
    }

    /// The inversion contract: a reconstructed breakdown reconciles to the observed net exactly,
    /// and lands on (or within rounding-plateau distance of) the gross that produced it. Probed
    /// at the awkward points — the per-period student-loan threshold, the per-period ACC cap, a
    /// bracket boundary — because a binary search is exactly the kind of code that works
    /// everywhere except at edges.
    #[test]
    fn reconstruction_inverts_a_period_payslip_exactly() {
        let s = scale();
        let input = PeriodPayeInput {
            period_gross_minor: 0, // ignored by reconstruct_period
            periods_per_year: 24,
            kiwisaver_bps: 350,
            employer_kiwisaver_bps: 0,
            student_loan: true,
        };
        for gross in [
            80_000i64,       // under the per-period student-loan threshold
            100_533,         // exactly at it
            100_534,         // one cent over
            400_000,         // an ordinary payslip
            652_671,         // at the per-period ACC cap
            652_672,         // one cent over it
            750_000_00 / 24, // annualises into the top bracket
        ] {
            let forward = paye_period(
                s,
                PeriodPayeInput {
                    period_gross_minor: gross,
                    ..input
                },
            );
            let back = reconstruct_period(s, forward.net_minor, input);
            assert_eq!(back.net_minor, forward.net_minor);
            assert_eq!(
                back.gross_minor
                    - back.income_tax_minor
                    - back.acc_levy_minor
                    - back.kiwisaver_minor
                    - back.student_loan_minor,
                back.net_minor,
                "reconstructed lines must reconcile at gross {gross}"
            );
            // The search returns the *smallest* gross on the plateau of grosses sharing this
            // net. Four lines each rounding by up to half a cent, over a combined marginal rate
            // near 50%, makes that plateau up to ~5 cents wide (widest right at the ACC cap,
            // where the levy's marginal contribution drops to zero).
            assert!(
                (back.gross_minor - gross).abs() <= 5,
                "gross {gross} came back as {} — outside rounding-plateau distance",
                back.gross_minor
            );
        }
    }

    #[test]
    fn reconstruction_inverts_an_extra_pay_exactly() {
        let s = scale();
        let input = ExtraPayInput {
            annualised_regular_minor: 96_000_00,
            extra_minor: 0, // ignored by reconstruct_extra_pay
            kiwisaver_bps: 350,
            employer_kiwisaver_bps: 0,
            student_loan: true,
        };
        for extra in [50_00i64, 2_500_00, 5_000_00, 90_000_00] {
            let forward = extra_pay(
                s,
                ExtraPayInput {
                    extra_minor: extra,
                    ..input
                },
            );
            let back = reconstruct_extra_pay(s, forward.net_minor, input);
            assert_eq!(back.net_minor, forward.net_minor);
            assert!(
                (back.gross_minor - extra).abs() <= 3,
                "extra {extra} came back as {}",
                back.gross_minor
            );
        }
    }

    /// A non-positive observed net is a question with no payslip behind it.
    #[test]
    fn reconstruction_of_nothing_is_nothing() {
        let s = scale();
        let input = PeriodPayeInput {
            period_gross_minor: 0,
            periods_per_year: 24,
            kiwisaver_bps: 350,
            employer_kiwisaver_bps: 0,
            student_loan: true,
        };
        assert_eq!(reconstruct_period(s, 0, input), PayeBreakdown::default());
        assert_eq!(
            reconstruct_period(s, -5_00, input),
            PayeBreakdown::default()
        );
    }
}
