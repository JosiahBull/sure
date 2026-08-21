//! Per-person income streams: what someone earns, when it lands, and what is left after tax.
//!
//! The wire and domain shapes only. Which take-home rate wins — an override, the statutory scale,
//! or a ratio reconciled against this household's own history — is resolution logic and lives in
//! `sure-app`, exactly as `forecast_assumptions`' precedence does.

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::iso_date::IsoDate;
use crate::money::Money;
use crate::tax::TaxScaleId;

/// How often a stream pays.
///
/// The simulation steps in months, so this plus an anchor date is what puts a quarterly payment in
/// the month it actually lands in and gives a fortnightly payer three paydays in the months that
/// really have three.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayFrequency {
    Weekly,
    Fortnightly,
    FourWeekly,
    /// Twice a month on two fixed days — the 14th and the 28th, say.
    ///
    /// **Not the same as fortnightly**, and the difference is not cosmetic: twice a month is 24
    /// payments a year where every fourteen days is 26. On a $135,000 salary that is $5,625 a
    /// payslip against $5,192, an 8% gap, and the two are constantly conflated in conversation
    /// because they sound alike and land near each other. If someone says "fortnightly, on the 14th
    /// and 28th", they mean this one.
    SemiMonthly,
    Monthly,
    Quarterly,
    Annual,
}

/// How a frequency advances from one payment to the next.
///
/// Day-stepped frequencies drift through the calendar — thirteen four-weekly payments is 364 days,
/// so the anchor moves a day earlier each year — and month-stepped ones do not. Enumerating real
/// dates gets both right; dividing by twelve gets neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayStep {
    Days(i64),
    Months(i64),
}

impl PayFrequency {
    pub fn as_str(self) -> &'static str {
        match self {
            PayFrequency::Weekly => "weekly",
            PayFrequency::Fortnightly => "fortnightly",
            PayFrequency::FourWeekly => "four_weekly",
            PayFrequency::SemiMonthly => "semi_monthly",
            PayFrequency::Monthly => "monthly",
            PayFrequency::Quarterly => "quarterly",
            PayFrequency::Annual => "annual",
        }
    }

    /// The divisor turning a quoted annual figure into one payslip.
    ///
    /// 52/26/13, not 365.25/7 — because that is what payroll does. A fortnightly salary is quoted
    /// annually and divided by 26; the years containing 27 paydays really do pay 27 times, and
    /// that extra payday is a true feature of being paid fortnightly rather than a rounding error
    /// to calibrate away. The same argument `monthly_repayment` makes for x52/12.
    pub fn periods_per_year(self) -> f64 {
        self.periods_per_year_int() as f64
    }

    /// The same divisor as an integer — what [`crate::tax::PeriodPayeInput`] wants, since a
    /// payslip's arithmetic is integer minor units end to end.
    pub fn periods_per_year_int(self) -> i64 {
        match self {
            PayFrequency::Weekly => 52,
            PayFrequency::Fortnightly => 26,
            PayFrequency::FourWeekly => 13,
            PayFrequency::SemiMonthly => 24,
            PayFrequency::Monthly => 12,
            PayFrequency::Quarterly => 4,
            PayFrequency::Annual => 1,
        }
    }

    pub fn step(self) -> PayStep {
        match self {
            PayFrequency::Weekly => PayStep::Days(7),
            PayFrequency::Fortnightly => PayStep::Days(14),
            PayFrequency::FourWeekly => PayStep::Days(28),
            // Two per calendar month, always — so it steps by month and the *count* is what differs.
            // `PayStep` cannot express "twice", which is why `payment_counts` special-cases it.
            PayFrequency::SemiMonthly => PayStep::Months(1),
            PayFrequency::Monthly => PayStep::Months(1),
            PayFrequency::Quarterly => PayStep::Months(3),
            PayFrequency::Annual => PayStep::Months(12),
        }
    }
}

impl FromStr for PayFrequency {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "weekly" => Ok(PayFrequency::Weekly),
            "fortnightly" => Ok(PayFrequency::Fortnightly),
            "four_weekly" => Ok(PayFrequency::FourWeekly),
            "semi_monthly" => Ok(PayFrequency::SemiMonthly),
            "monthly" => Ok(PayFrequency::Monthly),
            "quarterly" => Ok(PayFrequency::Quarterly),
            "annual" => Ok(PayFrequency::Annual),
            other => Err(format!("unknown pay frequency '{other}'")),
        }
    }
}

/// Which direction a stream's recorded figure points, and — when it is before deductions — whose
/// rules apply.
///
/// One enum rather than a `taxable` flag beside a separate `tax_scale`, because those would be two
/// independent encodings of the same fact and free to drift apart (CLAUDE.md rule 1). Adding
/// another jurisdiction is one variant here, and the exhaustive-match lint then finds every site
/// that has to decide what it means.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncomeBasis {
    /// The recorded figure is take-home. Nothing is deducted and nothing is reconciled.
    Net,
    /// The recorded figure is before New Zealand PAYE, ACC, KiwiSaver and student loan.
    GrossNzPaye,
}

impl IncomeBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            IncomeBasis::Net => "net",
            IncomeBasis::GrossNzPaye => "gross_nz_paye",
        }
    }

    /// The statutory scale to apply, or `None` when the figure is already take-home.
    pub fn tax_scale(self) -> Option<TaxScaleId> {
        match self {
            IncomeBasis::Net => None,
            IncomeBasis::GrossNzPaye => Some(TaxScaleId::NzPaye),
        }
    }

    /// Whether a gross→net conversion applies at all.
    pub fn is_gross(self) -> bool {
        match self {
            IncomeBasis::Net => false,
            IncomeBasis::GrossNzPaye => true,
        }
    }
}

impl FromStr for IncomeBasis {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "net" => Ok(IncomeBasis::Net),
            "gross_nz_paye" => Ok(IncomeBasis::GrossNzPaye),
            other => Err(format!("unknown income basis '{other}'")),
        }
    }
}

/// How one arrival of this income is taxed: as an ordinary payslip, or as an IRD "extra pay".
///
/// An extra pay is a lump sum landing inside a regular pay run — a quarterly bonus, a back
/// payment. It shares the regular salary's deposit but not its arithmetic: tax is a slice across
/// the brackets sitting on top of the annualised regular pay, and student loan takes 12% of the
/// whole lump with no threshold (the regular pays consumed it). See [`crate::tax::extra_pay`].
/// A per-stream fact rather than a per-payment flag, because whether income is a bonus is a
/// property of the income.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PayTreatment {
    #[default]
    Regular,
    ExtraPay,
}

impl PayTreatment {
    pub fn as_str(self) -> &'static str {
        match self {
            PayTreatment::Regular => "regular",
            PayTreatment::ExtraPay => "extra_pay",
        }
    }
}

impl FromStr for PayTreatment {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "regular" => Ok(PayTreatment::Regular),
            "extra_pay" => Ok(PayTreatment::ExtraPay),
            other => Err(format!("unknown pay treatment '{other}'")),
        }
    }
}

/// Where one expected payment of a stream stands against the ledger.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IncomePaymentStatus {
    /// Enumerated from the schedule; no deposit claimed yet. Past its `due_on`, this is the
    /// "missed pay" the review surface shows.
    Expected,
    /// The matcher claimed a deposit automatically.
    Matched,
    /// A person agreed with a match — the matcher will never touch the row again.
    Confirmed,
    /// A person said this expected payment is not real (unpaid leave, a contract gap). Kept
    /// rather than deleted so the next matcher run does not resurrect it.
    Dismissed,
}

impl IncomePaymentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            IncomePaymentStatus::Expected => "expected",
            IncomePaymentStatus::Matched => "matched",
            IncomePaymentStatus::Confirmed => "confirmed",
            IncomePaymentStatus::Dismissed => "dismissed",
        }
    }
}

impl FromStr for IncomePaymentStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "expected" => Ok(IncomePaymentStatus::Expected),
            "matched" => Ok(IncomePaymentStatus::Matched),
            "confirmed" => Ok(IncomePaymentStatus::Confirmed),
            "dismissed" => Ok(IncomePaymentStatus::Dismissed),
            other => Err(format!("unknown income payment status '{other}'")),
        }
    }
}

/// Who claimed the deposit — the matcher, or a person overriding it.
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    Auto,
    Manual,
}

impl MatchedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            MatchedBy::Auto => "auto",
            MatchedBy::Manual => "manual",
        }
    }
}

impl FromStr for MatchedBy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auto" => Ok(MatchedBy::Auto),
            "manual" => Ok(MatchedBy::Manual),
            other => Err(format!("unknown matched_by '{other}'")),
        }
    }
}

/// One expected payment of one stream, and — once matched — the deposit it claimed with its
/// reconstructed decomposition.
///
/// The decomposition is materialised at match time from the *observed* net (the deposit is
/// ground truth; see `sure_core::tax::reconstruct_period`), under the tax scale in force on the
/// date it landed. `gross_minor − income_tax − acc − kiwisaver − student_loan ==
/// observed_net_minor`, always — which is what lets a report draw the pre-income graph and still
/// balance to the cent.
#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct IncomePayment {
    pub id: i64,
    pub income_stream_id: i64,
    /// The scheduled date. The claimed deposit may sit a few days earlier — payroll shifts off
    /// weekends and holidays.
    pub due_on: String,
    pub status: IncomePaymentStatus,
    pub transaction_id: Option<i64>,
    pub matched_by: Option<MatchedBy>,
    /// What the configured level predicted for this date, kept beside the observed figure
    /// because their gap is the reconciliation signal.
    pub expected_net_minor: Option<i64>,
    /// This stream's slice of the claimed deposit.
    pub observed_net_minor: Option<i64>,
    pub gross_minor: Option<i64>,
    pub income_tax_minor: Option<i64>,
    pub acc_levy_minor: Option<i64>,
    pub kiwisaver_minor: Option<i64>,
    pub student_loan_minor: Option<i64>,
    pub employer_kiwisaver_minor: Option<i64>,
    pub esct_minor: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Where a stream's gross→net map came from.
///
/// The same precedence shape as `sure_app::forecast::AssumptionSource`: an override wins, else it
/// is computed.
///
/// There is deliberately **no** `Reconciled` variant. The reconciliation — this person's modelled
/// gross against the net actually observed in the linked income category — is reported *beside*
/// the projection as a check on it, not folded into the rate. Two reasons: the statutory scale
/// always resolves, so a reconciled rate would only ever be overriding a known-correct answer with
/// a measured one whose error bars nobody can see; and a diagnostic that silently changes the thing
/// it is diagnosing stops being a diagnostic. A variant that cannot be produced is worse than no
/// variant at all, so it is not carried "for later".
#[derive(Debug, Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TakeHomeSource {
    /// `take_home_bps` is set — the user asserting it, and an assertion wins.
    Override,
    /// `basis` is already net, so take-home is the recorded figure by definition.
    AlreadyNet,
    /// Computed from the statutory scale in [`crate::tax`].
    Statutory,
}

impl TakeHomeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TakeHomeSource::Override => "override",
            TakeHomeSource::AlreadyNet => "already_net",
            TakeHomeSource::Statutory => "statutory",
        }
    }
}

/// A stream's gross→net map.
///
/// Two rates, because PAYE is progressive and one rate cannot be honest about both questions.
/// `average_bps` prices the salary as it stands; `marginal_bps` prices the *increment* a promotion
/// adds, which is the only place the difference shows up in the simulation. A single rate would
/// tax a raise at the household's historical average, which is wrong in a direction that compounds
/// across a thirty-year horizon.
#[derive(Debug, Serialize, ToSchema, Clone, Copy, PartialEq, Eq)]
pub struct TakeHome {
    pub average_bps: i64,
    pub marginal_bps: i64,
    pub source: TakeHomeSource,
}

impl TakeHome {
    /// Take-home is the whole figure — for a stream already recorded net.
    pub fn all_of_it() -> Self {
        TakeHome {
            average_bps: 10_000,
            marginal_bps: 10_000,
            source: TakeHomeSource::AlreadyNet,
        }
    }

    /// Net annual pay at `gross_annual_minor`, given the level this map was calibrated at.
    ///
    /// At or below the calibration point the average rate applies; above it, the marginal rate
    /// prices the difference. This is the only function the Monte Carlo loop calls, which is what
    /// lets the statutory and reconciled producers coexist without the loop knowing either exists.
    pub fn net_annual(self, gross_annual_minor: f64, calibrated_at_minor: f64) -> f64 {
        let base = calibrated_at_minor.min(gross_annual_minor) * self.average_bps as f64 / 10_000.0;
        let excess = (gross_annual_minor - calibrated_at_minor).max(0.0) * self.marginal_bps as f64
            / 10_000.0;
        base + excess
    }
}

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct IncomeStream {
    pub id: i64,
    pub person_id: i64,
    pub label: String,
    pub employer: Option<String>,
    pub currency_code: String,
    pub annual_amount_minor: i64,
    pub basis: IncomeBasis,
    pub pay_frequency: PayFrequency,
    pub first_payment_on: String,
    pub starts_on: String,
    pub ends_on: Option<String>,
    pub annual_increase_bps: i64,
    pub kiwisaver_bps: i64,
    /// The employer's contribution, in basis points of gross. Never part of take-home.
    pub employer_kiwisaver_bps: i64,
    pub student_loan: bool,
    pub take_home_bps: Option<i64>,
    pub linked_category_id: Option<i64>,
    /// The account KiwiSaver contributions land in. Setting it takes that account off its fitted
    /// growth rate — see `0023_income_contribution_targets.sql` for why that is unavoidable.
    pub kiwisaver_account_id: Option<i64>,
    /// The student loan these deductions pay down. Same consequence for that account.
    pub student_loan_account_id: Option<i64>,
    /// Where this stream's deposits land, for the matcher. Matching is on iff this and
    /// `match_pattern` are both set.
    pub match_account_id: Option<i64>,
    /// A case-insensitive substring the deposit's description carries — the payroll memo's
    /// stable token, not the run number after it.
    pub match_pattern: Option<String>,
    pub pay_treatment: PayTreatment,
    pub enabled: bool,
    pub sort_order: i64,
    pub notes: Option<String>,
    /// The dated pay scale, ascending. Loaded with the stream — a schedule is not useful without
    /// the thing it schedules, and the UI edits them together.
    pub steps: Vec<IncomeStreamStep>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema, Clone)]
pub struct IncomeStreamStep {
    pub id: i64,
    pub income_stream_id: i64,
    pub effective_on: String,
    pub annual_amount_minor: i64,
    pub label: Option<String>,
}

/// Write body. `steps` is a **full replace**, like `SaveForecastAssumption` is a full-replace
/// upsert: the steps sent here *are* the schedule after the write, so deleting one is omitting it.
/// One body and one transaction, so a schedule can never be half-saved.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveIncomeStream {
    pub label: String,
    #[serde(default)]
    pub employer: Option<String>,
    pub currency_code: String,
    #[schema(value_type = i64)]
    pub annual_amount_minor: Money,
    pub basis: IncomeBasis,
    pub pay_frequency: PayFrequency,
    #[schema(value_type = String)]
    pub first_payment_on: IsoDate,
    #[schema(value_type = String)]
    pub starts_on: IsoDate,
    #[serde(default)]
    #[schema(value_type = Option<String>)]
    pub ends_on: Option<IsoDate>,
    #[serde(default)]
    pub annual_increase_bps: i64,
    #[serde(default)]
    pub kiwisaver_bps: i64,
    #[serde(default)]
    pub employer_kiwisaver_bps: i64,
    #[serde(default)]
    pub student_loan: bool,
    #[serde(default)]
    pub take_home_bps: Option<i64>,
    #[serde(default)]
    pub linked_category_id: Option<i64>,
    #[serde(default)]
    pub kiwisaver_account_id: Option<i64>,
    #[serde(default)]
    pub student_loan_account_id: Option<i64>,
    #[serde(default)]
    pub match_account_id: Option<i64>,
    #[serde(default)]
    pub match_pattern: Option<String>,
    #[serde(default)]
    pub pay_treatment: PayTreatment,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub steps: Vec<SaveIncomeStreamStep>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, ToSchema, Clone)]
pub struct SaveIncomeStreamStep {
    #[schema(value_type = String)]
    pub effective_on: IsoDate,
    #[schema(value_type = i64)]
    pub annual_amount_minor: Money,
    #[serde(default)]
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_round_trips_through_its_stored_text() {
        for f in [
            PayFrequency::Weekly,
            PayFrequency::Fortnightly,
            PayFrequency::FourWeekly,
            PayFrequency::SemiMonthly,
            PayFrequency::Monthly,
            PayFrequency::Quarterly,
            PayFrequency::Annual,
        ] {
            assert_eq!(PayFrequency::from_str(f.as_str()), Ok(f));
        }
        for b in [IncomeBasis::Net, IncomeBasis::GrossNzPaye] {
            assert_eq!(IncomeBasis::from_str(b.as_str()), Ok(b));
        }
        for t in [PayTreatment::Regular, PayTreatment::ExtraPay] {
            assert_eq!(PayTreatment::from_str(t.as_str()), Ok(t));
        }
        for s in [
            IncomePaymentStatus::Expected,
            IncomePaymentStatus::Matched,
            IncomePaymentStatus::Confirmed,
            IncomePaymentStatus::Dismissed,
        ] {
            assert_eq!(IncomePaymentStatus::from_str(s.as_str()), Ok(s));
        }
        for m in [MatchedBy::Auto, MatchedBy::Manual] {
            assert_eq!(MatchedBy::from_str(m.as_str()), Ok(m));
        }
        assert!(PayFrequency::from_str("biweekly").is_err());
        assert!(IncomeBasis::from_str("gross").is_err());
        assert!(PayTreatment::from_str("bonus").is_err());
        assert!(IncomePaymentStatus::from_str("linked").is_err());
    }

    /// The reason a stream stores an annual figure and a frequency rather than a per-payment
    /// amount: these are the divisors payroll uses, and a 27-payday year is a real feature of
    /// being paid fortnightly.
    #[test]
    fn periods_per_year_are_payrolls_divisors_not_calendar_arithmetic() {
        assert_eq!(PayFrequency::Fortnightly.periods_per_year(), 26.0);
        // The distinction that gets lost in conversation: twice a month is not every fourteen days.
        assert_eq!(PayFrequency::SemiMonthly.periods_per_year(), 24.0);
        assert_ne!(
            PayFrequency::SemiMonthly.periods_per_year(),
            PayFrequency::Fortnightly.periods_per_year()
        );
        assert_eq!(PayFrequency::FourWeekly.periods_per_year(), 13.0);
        // 13 x 28 = 364, which is where four-weekly's calendar drift comes from.
        assert_eq!(PayFrequency::FourWeekly.step(), PayStep::Days(28));
        assert_eq!(PayFrequency::Quarterly.step(), PayStep::Months(3));
    }

    /// A promotion is priced at the marginal rate, and everything at or below the calibration
    /// point at the average one. Getting this backwards would tax a raise too lightly.
    #[test]
    fn net_annual_prices_a_raise_at_the_marginal_rate() {
        let th = TakeHome {
            average_bps: 7_000,
            marginal_bps: 5_000,
            source: TakeHomeSource::Statutory,
        };
        // At the calibration point: the average rate alone.
        assert_eq!(th.net_annual(100_000.0, 100_000.0), 70_000.0);
        // Below it: still the average rate.
        assert_eq!(th.net_annual(50_000.0, 100_000.0), 35_000.0);
        // Above it: the increment is taxed harder — 70,000 + half of the extra 20,000.
        assert_eq!(th.net_annual(120_000.0, 100_000.0), 80_000.0);
    }

    #[test]
    fn an_already_net_stream_keeps_all_of_it() {
        let th = TakeHome::all_of_it();
        assert_eq!(th.source, TakeHomeSource::AlreadyNet);
        assert_eq!(th.net_annual(80_000.0, 80_000.0), 80_000.0);
    }

    #[test]
    fn a_basis_knows_whether_a_scale_applies() {
        assert_eq!(IncomeBasis::Net.tax_scale(), None);
        assert_eq!(
            IncomeBasis::GrossNzPaye.tax_scale(),
            Some(TaxScaleId::NzPaye)
        );
        assert!(!IncomeBasis::Net.is_gross());
        assert!(IncomeBasis::GrossNzPaye.is_gross());
    }
}
