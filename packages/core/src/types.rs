use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::people::Ownership;

/// The kinds of financial account Sure understands. `kind` selects type-specific
/// behaviour (how balance and net-worth contribution are computed); free-form
/// per-kind configuration lives in an account's `metadata`.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Cash,
    Bank,
    Savings,
    CreditCard,
    RevolvingCredit,
    Mortgage,
    StudentLoan,
    Loan,
    Vehicle,
    RealEstate,
    SharesNz,
    SharesUs,
    SharesPrivate,
    /// Multi-holding brokerage/investment platform (e.g. Sharesies): many ticker
    /// positions plus per-currency cash wallets under one account — see
    /// [`crate::brokerage`] for the lots ledger and computed snapshot. Distinct from
    /// `Shares*`, which is a single manually-tracked holding.
    Brokerage,
    /// Cryptocurrency held in a wallet or on an exchange.
    Crypto,
    Asset,
    Liability,
}

/// Broad grouping used by net-worth and balance logic.
#[derive(Serialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AccountClass {
    /// Spendable cash; balance = sum of transactions.
    Cash,
    /// Valued holding (property, vehicle); net worth = latest valuation.
    Asset,
    /// Valued investment (shares); net worth = latest valuation.
    Investment,
    /// Owed money; contributes negatively to net worth.
    Liability,
}

impl AccountKind {
    pub fn class(self) -> AccountClass {
        use AccountKind::*;
        match self {
            Cash | Bank | Savings => AccountClass::Cash,
            CreditCard | RevolvingCredit | Mortgage | StudentLoan | Loan | Liability => {
                AccountClass::Liability
            }
            Vehicle | RealEstate | Asset => AccountClass::Asset,
            SharesNz | SharesUs | SharesPrivate | Brokerage | Crypto => AccountClass::Investment,
        }
    }

    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind/read this as a
    /// plain `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        use AccountKind::*;
        match self {
            Cash => "cash",
            Bank => "bank",
            Savings => "savings",
            CreditCard => "credit_card",
            RevolvingCredit => "revolving_credit",
            Mortgage => "mortgage",
            StudentLoan => "student_loan",
            Loan => "loan",
            Vehicle => "vehicle",
            RealEstate => "real_estate",
            SharesNz => "shares_nz",
            SharesUs => "shares_us",
            SharesPrivate => "shares_private",
            Brokerage => "brokerage",
            Crypto => "crypto",
            Asset => "asset",
            Liability => "liability",
        }
    }
}

impl std::str::FromStr for AccountKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use AccountKind::*;
        Ok(match s {
            "cash" => Cash,
            "bank" => Bank,
            "savings" => Savings,
            "credit_card" => CreditCard,
            "revolving_credit" => RevolvingCredit,
            "mortgage" => Mortgage,
            "student_loan" => StudentLoan,
            "loan" => Loan,
            "vehicle" => Vehicle,
            "real_estate" => RealEstate,
            "shares_nz" => SharesNz,
            "shares_us" => SharesUs,
            "shares_private" => SharesPrivate,
            "brokerage" => Brokerage,
            "crypto" => Crypto,
            "asset" => Asset,
            "liability" => Liability,
            other => return Err(format!("unknown account kind '{other}'")),
        })
    }
}

// ---------------------------------------------------------------------------
// Typed, per-kind account metadata.
//
// Every account already stores a JSON `metadata` blob; these types give it a
// shape that varies by account kind. The active variant is chosen by the
// account's `kind` and serialised with a `profile` discriminant, so the stored
// JSON is self-describing and round-trips through the generated typed client as
// a discriminated union. Money is minor units and rates are basis points, to
// match the rest of the app (see `crons.rate_bps`, transactions' `*_minor`).
// ---------------------------------------------------------------------------

/// How a mortgage's interest rate is structured.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RateType {
    Fixed,
    Floating,
    Split,
}

/// How often a loan's contractual repayment is actually made. Weekly and fortnightly are
/// the NZ norm; the forecast annualises them (×52/12, ×26/12) rather than treating them as
/// ×4/×2 — the extra payments a year are exactly why paying weekly clears a loan sooner.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RepaymentFrequency {
    Weekly,
    Fortnightly,
    Monthly,
}

/// The unit a property's floor/land area is recorded in.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AreaUnit {
    Sqft,
    Sqm,
}

/// The unit a vehicle's odometer reading is recorded in.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MileageUnit {
    Mi,
    Km,
}

/// A report's date-sampling granularity (e.g. `sure_app::reports::NetWorthQuery.interval`).
/// Parsed at the HTTP edge from a query-string value; an unrecognised value is a 400, not
/// a silent default to `Month`.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Interval {
    Day,
    Week,
    Month,
}

impl Interval {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`.
    pub fn as_str(self) -> &'static str {
        match self {
            Interval::Day => "day",
            Interval::Week => "week",
            Interval::Month => "month",
        }
    }
}

impl std::str::FromStr for Interval {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "day" => Interval::Day,
            "week" => Interval::Week,
            "month" => Interval::Month,
            other => return Err(format!("unknown interval '{other}'")),
        })
    }
}

/// How gains on a holding are taxed. Not stored for share/brokerage accounts, where it
/// is derived from the account's `subtype` instead.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaxTreatment {
    Taxable,
    TaxDeferred,
    TaxExempt,
}

/// Bank / cash / savings / card accounts.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct DepositoryMeta {
    /// Finer-grained classification within the kind (e.g. `checking`, `savings`, `hsa`);
    /// the curated option list lives in the web layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// Account or card number (store a masked value if you like, e.g. `••4321`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_number: Option<String>,
    /// Credit limit in minor units, for a `credit_card`/`revolving_credit` account —
    /// lets "remaining borrowing" (limit minus what's owed) be shown. Meaningless for
    /// other depository kinds, so left unset there. Auto-populated on sync for
    /// providers that report a live limit (e.g. Akahu); editable manually otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_limit_minor: Option<i64>,
    /// The smallest amount payable each statement cycle, in minor units — for a
    /// `credit_card`/`revolving_credit` account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_payment_minor: Option<i64>,
    /// Annual percentage rate in basis points (e.g. 15.99% = 1599) — for a
    /// `credit_card`/`revolving_credit` account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apr_bps: Option<i64>,
    /// ISO-8601 date the card expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiration_date: Option<String>,
    /// The yearly fee charged for holding the card, in minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annual_fee_minor: Option<i64>,
    /// A link to online banking or the statement portal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Real estate.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyMeta {
    /// Finer-grained classification (e.g. `single_family_home`, `condominium`); the
    /// curated option list lives in the web layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// Street address. Aliased to the legacy `address` key so rows written before the
    /// address was broken into components keep deserialising.
    #[serde(default, alias = "address", skip_serializing_if = "Option::is_none")]
    pub address_line1: Option<String>,
    /// Unit / apartment / floor, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address_line2: Option<String>,
    /// Town or city.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// State, province or region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Postal / ZIP code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    /// Country.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// The year the dwelling was built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_built: Option<i64>,
    /// Floor area, expressed in `area_unit`s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_value: Option<i64>,
    /// The unit `area_value` is measured in (defaults to square feet when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area_unit: Option<AreaUnit>,
    /// ISO-8601 date the property was purchased.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_date: Option<String>,
    /// Purchase price in minor units of the account currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_price_minor: Option<i64>,
    /// A link to the listing, council valuation, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A mortgage secured against a property (link it with `secured_by_account_id`).
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct MortgageMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lender: Option<String>,
    /// The original amount borrowed, in minor units — lets a paid-down percentage be
    /// derived from the current balance. Auto-populated on sync for providers that
    /// report it (e.g. Akahu's `loan_details.initial_principal`); editable manually
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_amount_minor: Option<i64>,
    /// Annual interest rate in basis points (e.g. 5.49% = 549).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interest_rate_bps: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_type: Option<RateType>,
    /// ISO-8601 date the current fixed rate expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_until: Option<String>,
    /// Length of the current fixed-rate period, in months.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_term_months: Option<i64>,
    /// Overall loan term, in months.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term_months: Option<i64>,
    /// ISO-8601 date the loan started (used to derive time remaining).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    /// The rate to assume once the current fixed period ends, in basis points. The
    /// forecast draws each simulated path's post-refix rate around this, which is what
    /// gives a fixed-rate mortgage an honest band instead of a single confident line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refix_rate_bps: Option<i64>,
    /// One standard deviation of uncertainty around `refix_rate_bps`, in basis points
    /// (e.g. 150 = "±1.5% would be an unremarkable miss"). Zero makes the refix a
    /// certainty; every path then gets the same rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refix_rate_uncertainty_bps: Option<i64>,
    /// The actual contractual repayment, in minor units per `repayment_frequency`. Used
    /// in preference to a payment derived from the terms, so a deliberate overpayment (or
    /// the lender's own rounding) is projected as it really is rather than idealised.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repayment_minor: Option<i64>,
    /// How often `repayment_minor` is paid. Absent means monthly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repayment_frequency: Option<RepaymentFrequency>,
    /// Interest paid so far, in minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interest_paid_minor: Option<i64>,
    /// Capital (principal) paid so far, in minor units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capital_paid_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A table loan that amortises on a schedule (personal loan, vehicle financing, a private
/// or overseas student loan with real terms, ...). An income-contingent student loan is
/// [`StudentLoanMeta`] instead — see that type for why the two cannot share this one.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct LoanMeta {
    /// Finer-grained classification (e.g. `mortgage`, `student`, `auto`); the curated
    /// option list lives in the web layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lender: Option<String>,
    /// The original amount borrowed, in minor units — lets a paid-down percentage be
    /// derived from the current balance. Auto-populated on sync for providers that
    /// report it (e.g. Akahu's `loan_details.initial_principal`); editable manually
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_amount_minor: Option<i64>,
    /// Annual interest rate in basis points (e.g. 8.90% = 890).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interest_rate_bps: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_type: Option<RateType>,
    /// ISO-8601 date the current fixed rate expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_until: Option<String>,
    /// Length of the current fixed-rate period, in months.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_term_months: Option<i64>,
    /// Overall loan term, in months.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term_months: Option<i64>,
    /// ISO-8601 date the loan started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    /// The rate to assume once the current fixed period ends, in basis points. See
    /// [`MortgageMeta::refix_rate_bps`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refix_rate_bps: Option<i64>,
    /// One standard deviation of uncertainty around `refix_rate_bps`, in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refix_rate_uncertainty_bps: Option<i64>,
    /// The actual contractual repayment, in minor units per `repayment_frequency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repayment_minor: Option<i64>,
    /// How often `repayment_minor` is paid. Absent means monthly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repayment_frequency: Option<RepaymentFrequency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// An income-contingent student loan: the IR/StudyLink shape, and its own profile rather
/// than a [`LoanMeta`] with most of the fields left blank.
///
/// The distinction is not tidiness. Such a loan has **no original principal** — it is drawn
/// down over years of study in as many tranches as there were semesters (course fees,
/// course-related costs, living costs; see `sure_providers::myir`), so the balance climbs
/// for years before it ever starts falling, and no single figure is "the amount borrowed".
/// It has no term and no repayment schedule either: it is repaid as a percentage of income
/// through PAYE until it is gone, which is a function of a salary this app does not model,
/// not of a table. Asking for those numbers gets placeholders, and every figure derived
/// from a placeholder looks exactly as trustworthy as one derived from an answer — a
/// paid-down percentage against an invented principal, or worse, `sure_app::forecast`
/// projecting a fabricated amortisation line over the real balance. So the fields do not
/// exist here, and the forecast falls back to fitting the balance's own trend the way it
/// does for any other liability it has no schedule for.
///
/// A student loan that genuinely *does* amortise — a private or overseas one with a
/// principal, a rate and a term — is a `loan` account with `subtype = "student"`, which is
/// what that subtype is for.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct StudentLoanMeta {
    /// Who the loan is with (e.g. `Inland Revenue`, `StudyLink`). Loan-shaped accounts have
    /// no account-level institution, so this stands in for one in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lender: Option<String>,
    /// Annual interest rate in basis points. `0` is the ordinary answer, and a real one: an
    /// NZ-based borrower's loan is interest-free. An overseas-based borrower's accrues
    /// interest, which is why this is asked rather than assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interest_rate_bps: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A vehicle. Attach financing as a separate loan account secured against it.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct VehicleMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    /// Odometer reading, expressed in `mileage_unit`s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mileage_value: Option<i64>,
    /// The unit `mileage_value` is measured in (defaults to miles when unset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mileage_unit: Option<MileageUnit>,
    /// Registration / licence plate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate: Option<String>,
    /// A friendly name, e.g. "the wagon".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purchase_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sale_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Share / equity holdings (NZ, US, or private).
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct SharesMeta {
    /// Finer-grained classification (e.g. `401k`, `kiwisaver`, `roth_ira`); the curated
    /// option list — and the tax treatment derived from it — lives in the web layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// Broker or platform (e.g. Sharesies, Hatch, IBKR).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticker: Option<String>,
    /// Exchange the holding trades on (e.g. NZX, NASDAQ).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// A multi-holding brokerage/investment platform account (e.g. Sharesies). Unlike
/// [`SharesMeta`], there's no single `ticker`/`exchange` here — positions live in the
/// `holdings` ledger (see `crate::brokerage`), keyed per-lot, so one account can hold
/// many tickers across many currencies plus cash wallets.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct BrokerageMeta {
    /// Finer-grained classification (e.g. `401k`, `kiwisaver`, `roth_ira`); the curated
    /// option list — and the tax treatment derived from it — lives in the web layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// Broker or platform (e.g. Sharesies, Hatch, IBKR).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Cryptocurrency held in a wallet or on an exchange. Unlike shares, the tax treatment
/// isn't implied by the subtype, so it's recorded explicitly.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct CryptoMeta {
    /// Where the coins are held: `wallet` or `exchange`; the curated option list lives in
    /// the web layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    /// How gains are taxed (most crypto is held in a taxable account).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_treatment: Option<TaxTreatment>,
    /// A link to the exchange or block explorer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Any other asset or liability.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct GenericMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Typed configuration for an account. The variant (`profile`) is determined by the
/// account's `kind`; see [`AccountMetadata::profile_for`].
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum AccountMetadata {
    Depository(DepositoryMeta),
    Property(PropertyMeta),
    Mortgage(MortgageMeta),
    Loan(LoanMeta),
    StudentLoan(StudentLoanMeta),
    Vehicle(VehicleMeta),
    Shares(SharesMeta),
    Brokerage(BrokerageMeta),
    Crypto(CryptoMeta),
    Generic(GenericMeta),
}

/// Which [`AccountMetadata`] variant a kind or value uses. The single source of truth for
/// the kind→profile grouping that `profile_for`, `profile`, and `default_for` used to
/// hand-list three times over (once as a kind→string match, once as a variant→string
/// match, once as a kind→default-value match) — each now delegates to one of the three
/// methods below instead of restating the grouping.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Profile {
    Depository,
    Property,
    Mortgage,
    Loan,
    StudentLoan,
    Vehicle,
    Shares,
    Brokerage,
    Crypto,
    Generic,
}

impl Profile {
    /// The wire discriminant (matches `#[serde(tag = "profile", rename_all = "snake_case")]`).
    fn as_str(self) -> &'static str {
        match self {
            Profile::Depository => "depository",
            Profile::Property => "property",
            Profile::Mortgage => "mortgage",
            Profile::Loan => "loan",
            Profile::StudentLoan => "student_loan",
            Profile::Vehicle => "vehicle",
            Profile::Shares => "shares",
            Profile::Brokerage => "brokerage",
            Profile::Crypto => "crypto",
            Profile::Generic => "generic",
        }
    }

    /// The profile a given account `kind` uses.
    fn for_kind(kind: AccountKind) -> Self {
        use AccountKind::*;
        match kind {
            Cash | Bank | Savings | CreditCard | RevolvingCredit => Profile::Depository,
            RealEstate => Profile::Property,
            Mortgage => Profile::Mortgage,
            Loan => Profile::Loan,
            StudentLoan => Profile::StudentLoan,
            Vehicle => Profile::Vehicle,
            SharesNz | SharesUs | SharesPrivate => Profile::Shares,
            Brokerage => Profile::Brokerage,
            Crypto => Profile::Crypto,
            Asset | Liability => Profile::Generic,
        }
    }

    /// An empty [`AccountMetadata`] value for this profile.
    fn empty_metadata(self) -> AccountMetadata {
        match self {
            Profile::Depository => AccountMetadata::Depository(DepositoryMeta::default()),
            Profile::Property => AccountMetadata::Property(PropertyMeta::default()),
            Profile::Mortgage => AccountMetadata::Mortgage(MortgageMeta::default()),
            Profile::Loan => AccountMetadata::Loan(LoanMeta::default()),
            Profile::StudentLoan => AccountMetadata::StudentLoan(StudentLoanMeta::default()),
            Profile::Vehicle => AccountMetadata::Vehicle(VehicleMeta::default()),
            Profile::Shares => AccountMetadata::Shares(SharesMeta::default()),
            Profile::Brokerage => AccountMetadata::Brokerage(BrokerageMeta::default()),
            Profile::Crypto => AccountMetadata::Crypto(CryptoMeta::default()),
            Profile::Generic => AccountMetadata::Generic(GenericMeta::default()),
        }
    }
}

impl AccountMetadata {
    /// The metadata profile discriminant expected for a given account kind.
    pub fn profile_for(kind: AccountKind) -> &'static str {
        Profile::for_kind(kind).as_str()
    }

    /// The discriminant of this value.
    pub fn profile(&self) -> &'static str {
        match self {
            AccountMetadata::Depository(_) => Profile::Depository,
            AccountMetadata::Property(_) => Profile::Property,
            AccountMetadata::Mortgage(_) => Profile::Mortgage,
            AccountMetadata::Loan(_) => Profile::Loan,
            AccountMetadata::StudentLoan(_) => Profile::StudentLoan,
            AccountMetadata::Vehicle(_) => Profile::Vehicle,
            AccountMetadata::Shares(_) => Profile::Shares,
            AccountMetadata::Brokerage(_) => Profile::Brokerage,
            AccountMetadata::Crypto(_) => Profile::Crypto,
            AccountMetadata::Generic(_) => Profile::Generic,
        }
        .as_str()
    }

    /// An empty metadata value with the right variant for `kind`.
    pub fn default_for(kind: AccountKind) -> Self {
        Profile::for_kind(kind).empty_metadata()
    }
}

// ---------------------------------------------------------------------------
// Required-field validation.
//
// Every metadata field above stays `Option<T>` even though many of them are now required,
// because `None` has to keep meaning "nobody has told us yet". A provider-linked mortgage
// genuinely doesn't know its original principal until a sync fills it in, and rows written
// before a field became required have to keep deserialising; typing those fields as
// `String`/`i64` would force a placeholder (`rate_type = Fixed`, `original_amount = 0`)
// into both cases, and every figure derived from them (a loan's paid-off %) would then
// compute confidently from the lie.
//
// Enforcement therefore lives on the *write* path, here: reading an incomplete account
// always works, and the requirement surfaces as a 422 the first time it is saved through
// the form — which is exactly the "you must fill this in" prompt we want. The requirements
// are spelled out as data below so the tables read as the spec they are.
// ---------------------------------------------------------------------------

/// Which write path a metadata value is being validated for.
///
/// The asymmetry is deliberate: a bank feed knows far less about an account than the
/// person who owns it. Akahu can tell us a mortgage exists but not its original principal,
/// and no feed knows which city a house is in — sync fills in whatever the upstream does
/// report later (see `sure_app::sync`). Demanding the full set on the link path would
/// simply make linking impossible, so [`ValidationMode::Linked`] enforces only the
/// structural rules (the profile has to suit the kind, and a `subtype` that *is* present
/// has to be a legal value), while [`ValidationMode::Manual`] — a human at the account
/// form — enforces everything.
///
/// One exception, added with the mortgage forecast: a mortgage or loan is asked for its
/// amortisation terms on both paths (`AMORTISING_REQUIRED`). Linking is not an unattended
/// import — the connect dialog is a form with a person in front of it — and a feed reports
/// a loan's balance but essentially never its terms, so exempting the link path would mean
/// the usual way to create a mortgage is the one that leaves it unforecastable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValidationMode {
    Manual,
    Linked,
}

/// A metadata field that must be filled in, plus how its value is checked once present.
/// The `&'static str` is the wire key, so a problem report names the exact JSON key the
/// caller sent (or left out).
#[derive(Clone, Copy, Debug)]
enum Required {
    /// A string, which must be non-blank once trimmed.
    Text(&'static str),
    /// A money amount in minor units, which must be strictly positive — a required amount
    /// of zero is a placeholder, not an answer.
    Amount(&'static str),
    /// A rate in basis points. 0% is a legitimate rate (an interest-free family loan), so
    /// only a negative one is rejected.
    Bps(&'static str),
    /// A whole count, e.g. a model year, which must be positive.
    Count(&'static str),
    /// A field whose type is an enum: serde has already rejected anything outside the
    /// enum, so only presence is checked.
    Choice(&'static str),
}

impl Required {
    fn key(self) -> &'static str {
        match self {
            Required::Text(k)
            | Required::Amount(k)
            | Required::Bps(k)
            | Required::Count(k)
            | Required::Choice(k) => k,
        }
    }

    /// The problem with this field, given its serialised value (`None` when the key is
    /// absent, which for these types means the option was `None`).
    fn problem(self, value: Option<&Value>) -> Option<String> {
        let key = self.key();
        let missing = || Some(format!("{key} is required"));
        let Some(value) = value.filter(|v| !v.is_null()) else {
            return missing();
        };
        match self {
            Required::Text(_) => match value.as_str() {
                Some(s) if !s.trim().is_empty() => None,
                _ => missing(),
            },
            Required::Amount(_) | Required::Count(_) => match value.as_i64() {
                Some(n) if n > 0 => None,
                Some(_) => Some(format!("{key} must be greater than zero")),
                None => missing(),
            },
            Required::Bps(_) => match value.as_i64() {
                Some(n) if n >= 0 => None,
                Some(_) => Some(format!("{key} cannot be negative")),
                None => missing(),
            },
            Required::Choice(_) => None,
        }
    }
}

/// Fields required of *every* account whose kind uses the profile.
///
/// `depository` and `generic` are absent on purpose: our `kind` already distinguishes
/// cash/bank/savings (so a subtype adds nothing), and "other asset"/"other liability" are
/// deliberately free-form catch-alls.
const PROFILE_REQUIRED: &[(&str, &[Required])] = &[
    (
        "property",
        &[
            Required::Text("subtype"),
            Required::Text("address_line1"),
            Required::Text("city"),
            Required::Text("country"),
        ],
    ),
    (
        "vehicle",
        &[
            Required::Text("make"),
            Required::Text("model"),
            Required::Count("year"),
        ],
    ),
    // `term_months` + `start_date` are what turn a mortgage from a balance into a
    // schedule: together with the principal and the rate they let the forecast project
    // the exact payoff instead of extrapolating a trend from a few months of history
    // (`sure_app::forecast`). Without them the projection is meaningless for a debt.
    (
        "mortgage",
        &[
            Required::Text("lender"),
            Required::Amount("original_amount_minor"),
            Required::Bps("interest_rate_bps"),
            Required::Choice("rate_type"),
            Required::Count("term_months"),
            Required::Text("start_date"),
        ],
    ),
    // A table loan needs the same schedule a mortgage does, and for the same reason. These
    // used to sit in `KIND_REQUIRED` because `student_loan` shared this profile and is
    // exempt from all of it; now that it has its own profile, `loan` is the only kind here
    // and the requirement belongs with the rest of them.
    (
        "loan",
        &[
            Required::Text("subtype"),
            Required::Text("lender"),
            Required::Amount("original_amount_minor"),
            Required::Bps("interest_rate_bps"),
            Required::Choice("rate_type"),
            Required::Count("term_months"),
            Required::Text("start_date"),
        ],
    ),
    // An income-contingent student loan has no principal, term or schedule to ask for — see
    // [`StudentLoanMeta`], where those fields deliberately do not exist. What is left is
    // answerable: the lender, and a rate that is `0` for an NZ-based borrower and real for
    // an overseas-based one.
    (
        "student_loan",
        &[Required::Text("lender"), Required::Bps("interest_rate_bps")],
    ),
    ("brokerage", &[Required::Text("broker")]),
    ("shares", &[Required::Text("broker")]),
    (
        "crypto",
        &[Required::Text("subtype"), Required::Choice("tax_treatment")],
    ),
];

/// Fields required by particular kinds rather than by their whole profile.
const KIND_REQUIRED: &[(AccountKind, &[Required])] = &[
    // Only a revolving facility has a limit; "remaining borrowing" is meaningless without
    // it, and every other depository kind leaves it unset.
    (
        AccountKind::CreditCard,
        &[Required::Amount("credit_limit_minor")],
    ),
    (
        AccountKind::RevolvingCredit,
        &[Required::Amount("credit_limit_minor")],
    ),
    // A listed holding is priced by (ticker, exchange) — see `list_shares_tickers` and the
    // stock-price poller. `shares_private` is excluded: an unlisted holding has neither.
    (
        AccountKind::SharesNz,
        &[Required::Text("ticker"), Required::Text("exchange")],
    ),
    (
        AccountKind::SharesUs,
        &[Required::Text("ticker"), Required::Text("exchange")],
    ),
];

/// The terms that turn a debt into a schedule, demanded of a mortgage/loan on *every*
/// write path — see the note in [`AccountMetadata::validate_for`] for why this one case
/// overrides the manual-vs-linked split. Deliberately not `lender`/`subtype`: those are
/// labels, and a feed can supply the institution on its own.
const AMORTISING_REQUIRED: &[Required] = &[
    Required::Amount("original_amount_minor"),
    Required::Bps("interest_rate_bps"),
    Required::Choice("rate_type"),
    Required::Count("term_months"),
    Required::Text("start_date"),
];

/// Amortising-debt kinds whose rate can roll off, and the fields that roll-off needs.
///
/// Conditional on `rate_type`, which is why these can't live in the flat tables above: a
/// floating loan has no expiry and nothing to refix to, so demanding a refix rate for one
/// would be asking for a number that doesn't exist. A fixed rate (and the fixed leg of a
/// split) does expire, and what happens next is the single largest uncertainty in a
/// long-horizon projection — so the forecast insists on being told, rather than quietly
/// assuming today's rate runs for the next thirty years.
const REFIX_REQUIRED: &[Required] = &[
    Required::Text("fixed_until"),
    Required::Bps("refix_rate_bps"),
    Required::Bps("refix_rate_uncertainty_bps"),
];

/// Whether `kind` is a table loan whose terms include a rate that can roll off.
fn amortises(kind: AccountKind) -> bool {
    matches!(kind, AccountKind::Mortgage | AccountKind::Loan)
}

/// Legal `subtype` values per profile.
///
/// The investment profiles (`shares`, `brokerage`) are deliberately absent. Their curated
/// 43-entry list — transcribed from the reference app, with the tax treatment each one
/// implies — lives in the web layer (`packages/web/src/lib/accountSubtypes.ts`), and
/// duplicating it here would only give it a second place to drift from.
const SUBTYPE_VALUES: &[(&str, &[&str])] = &[
    (
        "depository",
        &["checking", "savings", "hsa", "cd", "money_market"],
    ),
    (
        "property",
        &[
            "single_family_home",
            "multi_family_home",
            "condominium",
            "townhouse",
            "investment_property",
            "second_home",
        ],
    ),
    ("loan", &["mortgage", "student", "auto", "other"]),
    ("crypto", &["wallet", "exchange"]),
];

impl AccountMetadata {
    /// Check this metadata against the account `kind`, collecting **every** problem so the
    /// caller can answer with a single 422 naming all of them — filling in a form should
    /// not be a game of whack-a-mole. `Ok(())` means the value is complete enough to
    /// store.
    ///
    /// Call it only once the profile is known to suit the kind (the DAL checks that first
    /// and reports a mismatch on its own): the kind-conditional table below is looked up
    /// by `kind`, so a mismatched pair would be asked for fields it cannot have.
    pub fn validate_for(&self, kind: AccountKind, mode: ValidationMode) -> Result<(), Vec<String>> {
        let profile = self.profile();
        // Both checks read the serialised form: `skip_serializing_if` means an unset field
        // is simply an absent key, so the tables' wire keys are the only names in play —
        // the same ones the caller sent, and the ones the problems name back.
        let json = serde_json::to_value(self).unwrap_or(Value::Null);
        let field = |key: &str| json.get(key);

        let mut problems = Vec::new();

        // A subtype that *is* set must be one of the curated values, in both modes: an
        // unrecognised one is a typo or a stale client, and it would silently lose its
        // human label everywhere the UI looks it up.
        if let Some((_, legal)) = SUBTYPE_VALUES.iter().find(|(p, _)| *p == profile) {
            if let Some(subtype) = field("subtype").and_then(Value::as_str) {
                let subtype = subtype.trim();
                if !subtype.is_empty() && !legal.contains(&subtype) {
                    problems.push(format!(
                        "subtype '{subtype}' is not one of: {}",
                        legal.join(", ")
                    ));
                }
            }
        }

        if mode == ValidationMode::Manual {
            let profile_wide = PROFILE_REQUIRED
                .iter()
                .find(|(p, _)| *p == profile)
                .map_or(&[][..], |(_, fields)| fields);
            let kind_specific = KIND_REQUIRED
                .iter()
                .find(|(k, _)| *k == kind)
                .map_or(&[][..], |(_, fields)| fields);
            for required in profile_wide.iter().chain(kind_specific) {
                problems.extend(required.problem(field(required.key())));
            }
        }

        // Amortising debt is the exception to the mode split above, and asked for on both
        // paths. The exemption exists because a *feed* knows less than a person — but a
        // link is not an unattended import: there is a human in the connect dialog, and it
        // asks for these. Meanwhile a bank feed reports a mortgage's balance and almost
        // never its terms (Akahu's `loan_details` is optional throughout, and ASB supplies
        // none of it), so letting a mortgage in without them means the commonest way to
        // create one is the one that leaves it unprojectable — falling back to fitting a
        // trend to a debt, silently, forever.
        if amortises(kind) {
            // In `Manual` the tables above already cover these; this is what closes the
            // gap on the link path without asking for them twice.
            if mode == ValidationMode::Linked {
                for required in AMORTISING_REQUIRED {
                    problems.extend(required.problem(field(required.key())));
                }
            }
            // Only a rate that expires needs somewhere to go when it does.
            if matches!(
                field("rate_type").and_then(Value::as_str),
                Some("fixed") | Some("split")
            ) {
                for required in REFIX_REQUIRED {
                    problems.extend(required.problem(field(required.key())));
                }
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

// ---------------------------------------------------------------------------
// Account wire/domain shape. The DAL owns `AccountRow` (the raw SQLite row) and
// converts it into `Account` below; the API crate uses `Account`/`SaveAccount`
// directly as its request/response bodies.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    /// Derived grouping (cash / asset / investment / liability).
    pub class: AccountClass,
    pub currency_code: String,
    pub institution: Option<String>,
    /// Typed, kind-specific configuration (discriminated by `profile`).
    pub metadata: AccountMetadata,
    pub archived: bool,
    /// Keep this account's balance out of the household's net worth without hiding it.
    ///
    /// The flag is about the *pot*, not the movements: an excluded account leaves the
    /// net-worth series, the balances roll-up and the forecast's projection, but its
    /// transactions still count in the spend and category reports, because money you spent
    /// is money you spent whoever the balance belongs to. Distinct from [`Self::archived`],
    /// which removes the account from the app altogether.
    pub excluded_from_net_worth: bool,
    pub sort_order: i64,
    /// For a liability, the asset account it is secured against (e.g. a mortgage's home).
    pub secured_by_account_id: Option<i64>,
    /// Which household member this belongs to (or `joint`, or nobody has said yet).
    pub ownership: Ownership,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveAccount {
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
    #[serde(default)]
    pub institution: Option<String>,
    /// Typed, kind-specific config. Its `profile` must match the account `kind`; when
    /// omitted, an empty value for the kind is stored.
    #[serde(default)]
    pub metadata: Option<AccountMetadata>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub sort_order: i64,
    /// The balance the account starts with, in minor units, signed with the app's usual
    /// convention (a liability is negative). Seeded into the ledger as part of creating the
    /// account — the balance can never go missing because a follow-up request failed, which
    /// is what the previous create-then-seed flow risked.
    ///
    /// Optional on the DTO only because this is the PUT body too; the create path requires
    /// it (paired with `opening_balance_date`) for every kind but `brokerage`, whose value
    /// comes from its holdings ledger, and the update path refuses it — afterwards the
    /// balance is edited through transactions/valuations.
    #[serde(default)]
    pub opening_balance_minor: Option<i64>,
    /// The date `opening_balance_minor` applies from (ISO-8601).
    #[serde(default)]
    pub opening_balance_date: Option<String>,
    /// Which household member the account belongs to, or that it's joint.
    ///
    /// Required, on create *and* on the full-replace update — deliberately not
    /// `#[serde(default)]` like the optional fields around it. An account with no owner is
    /// the state this feature exists to eliminate, so the refusal lives at the outermost
    /// edge: a body without it fails to deserialise and never reaches a handler that could
    /// pick a default. The cost is that every caller must answer the question, which is the
    /// point.
    pub ownership: Ownership,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetSecuredBy {
    /// The asset account this (liability) account is secured against; `null` to unlink.
    pub secured_by_account_id: Option<i64>,
}

/// Body of `PUT /api/accounts/{id}/excluded-from-net-worth`.
///
/// Its own endpoint rather than a field on [`SaveAccount`], deliberately: that DTO is a
/// full replace sent by the account form, the seed script, the api-tests helper and the
/// provider-link path, and any one of them omitting the field would silently clear the
/// user's setting. Kept off it, the flag survives a form save by construction — the same
/// arrangement [`SetSecuredBy`] has.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetExcludedFromNetWorth {
    pub excluded_from_net_worth: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The terms that make a loan projectable, as the account form would send them.
    fn fixed_mortgage() -> MortgageMeta {
        MortgageMeta {
            lender: Some("ASB".into()),
            original_amount_minor: Some(48_500_000),
            interest_rate_bps: Some(512),
            rate_type: Some(RateType::Fixed),
            fixed_until: Some("2027-01-11".into()),
            term_months: Some(324),
            start_date: Some("2025-12-11".into()),
            refix_rate_bps: Some(512),
            refix_rate_uncertainty_bps: Some(150),
            ..Default::default()
        }
    }

    fn problems(meta: &AccountMetadata, kind: AccountKind) -> Vec<String> {
        meta.validate_for(kind, ValidationMode::Manual)
            .err()
            .unwrap_or_default()
    }

    #[test]
    fn a_complete_fixed_mortgage_validates() {
        let meta = AccountMetadata::Mortgage(fixed_mortgage());
        assert_eq!(problems(&meta, AccountKind::Mortgage), Vec::<String>::new());
    }

    /// Every problem at once: filling in a form should not be whack-a-mole.
    #[test]
    fn an_empty_mortgage_reports_every_missing_term_together() {
        let meta = AccountMetadata::Mortgage(MortgageMeta::default());
        let problems = problems(&meta, AccountKind::Mortgage);
        for key in [
            "lender",
            "original_amount_minor",
            "interest_rate_bps",
            "rate_type",
            "term_months",
            "start_date",
        ] {
            assert!(
                problems.iter().any(|p| p.starts_with(key)),
                "expected {key} in {problems:?}"
            );
        }
        // The refix trio is conditional on a fixed rate, and no rate type is set yet.
        assert!(!problems.iter().any(|p| p.starts_with("refix_rate_bps")));
    }

    /// A floating rate has no expiry and nothing to refix to, so demanding either would be
    /// asking for a number that does not exist.
    #[test]
    fn a_floating_loan_is_not_asked_for_a_refix() {
        let meta = AccountMetadata::Mortgage(MortgageMeta {
            rate_type: Some(RateType::Floating),
            fixed_until: None,
            refix_rate_bps: None,
            refix_rate_uncertainty_bps: None,
            ..fixed_mortgage()
        });
        assert_eq!(problems(&meta, AccountKind::Mortgage), Vec::<String>::new());
    }

    #[test]
    fn a_fixed_rate_must_say_what_happens_when_it_expires() {
        for rate_type in [RateType::Fixed, RateType::Split] {
            let meta = AccountMetadata::Mortgage(MortgageMeta {
                rate_type: Some(rate_type),
                fixed_until: None,
                refix_rate_bps: None,
                refix_rate_uncertainty_bps: None,
                ..fixed_mortgage()
            });
            let problems = problems(&meta, AccountKind::Mortgage);
            assert_eq!(problems.len(), 3, "{rate_type:?}: {problems:?}");
            for key in [
                "fixed_until",
                "refix_rate_bps",
                "refix_rate_uncertainty_bps",
            ] {
                assert!(problems.iter().any(|p| p.starts_with(key)), "{problems:?}");
            }
        }
    }

    /// Zero uncertainty is a real answer ("I'm confident"), unlike a zero term. Only a
    /// negative one is nonsense.
    #[test]
    fn zero_refix_uncertainty_is_accepted_but_a_zero_term_is_not() {
        let confident = AccountMetadata::Mortgage(MortgageMeta {
            refix_rate_uncertainty_bps: Some(0),
            ..fixed_mortgage()
        });
        assert_eq!(
            problems(&confident, AccountKind::Mortgage),
            Vec::<String>::new()
        );

        let no_term = AccountMetadata::Mortgage(MortgageMeta {
            term_months: Some(0),
            ..fixed_mortgage()
        });
        assert!(problems(&no_term, AccountKind::Mortgage)
            .iter()
            .any(|p| p.contains("term_months") && p.contains("greater than zero")));
    }

    /// An income-contingent student loan is asked for the two things it can answer, and
    /// nothing else. The principal, term and schedule a table loan must supply are not
    /// merely optional here — [`StudentLoanMeta`] has no such fields, so there is nowhere to
    /// put the placeholder that requiring them would produce.
    #[test]
    fn a_student_loan_asks_only_for_what_it_can_answer() {
        let complete = AccountMetadata::StudentLoan(StudentLoanMeta {
            lender: Some("Inland Revenue".into()),
            // Interest-free while the borrower is NZ-based: a real answer, not a placeholder,
            // which is why `Required::Bps` accepts it.
            interest_rate_bps: Some(0),
            ..Default::default()
        });
        assert_eq!(
            problems(&complete, AccountKind::StudentLoan),
            Vec::<String>::new()
        );

        let bare = AccountMetadata::StudentLoan(StudentLoanMeta::default());
        let problems = problems(&bare, AccountKind::StudentLoan);
        assert_eq!(problems.len(), 2, "{problems:?}");
        for key in ["lender", "interest_rate_bps"] {
            assert!(
                problems.iter().any(|p| p.starts_with(key)),
                "expected {key} in {problems:?}"
            );
        }
    }

    /// The kind→profile pairing that makes the above true, pinned: a student loan gets its
    /// own profile, and the `loan` profile is a table loan's alone. A regression here would
    /// silently put the amortisation fields back within reach of a student loan.
    #[test]
    fn a_student_loan_and_a_table_loan_use_different_profiles() {
        assert_eq!(
            AccountMetadata::profile_for(AccountKind::StudentLoan),
            "student_loan"
        );
        assert_eq!(AccountMetadata::profile_for(AccountKind::Loan), "loan");
        assert_eq!(
            AccountMetadata::default_for(AccountKind::StudentLoan),
            AccountMetadata::StudentLoan(StudentLoanMeta::default())
        );
    }

    /// A table loan still needs its whole schedule, which is what moving those requirements
    /// off `KIND_REQUIRED` and onto the `loan` profile has to preserve.
    #[test]
    fn a_table_loan_still_demands_its_schedule() {
        let bare = AccountMetadata::Loan(LoanMeta {
            subtype: Some("auto".into()),
            lender: Some("MTF Finance".into()),
            original_amount_minor: Some(1_500_000),
            interest_rate_bps: Some(890),
            ..Default::default()
        });
        let problems = problems(&bare, AccountKind::Loan);
        for key in ["rate_type", "term_months", "start_date"] {
            assert!(
                problems.iter().any(|p| p.starts_with(key)),
                "expected {key} in {problems:?}"
            );
        }
    }

    /// Linking is still the lenient path for everything a feed can't know — a house's
    /// address, a card's subtype — so an ordinary account links with nothing filled in.
    #[test]
    fn linking_stays_lenient_for_what_a_feed_cannot_know() {
        let meta = AccountMetadata::Property(PropertyMeta::default());
        assert!(meta
            .validate_for(AccountKind::RealEstate, ValidationMode::Linked)
            .is_ok());
        // …and the same value is refused at the account form, where a person is asked.
        assert!(meta
            .validate_for(AccountKind::RealEstate, ValidationMode::Manual)
            .is_err());
    }

    /// The exception: a mortgage's terms are asked for even when linking, because Akahu
    /// reports a balance and no schedule, and a mortgage without one silently degrades to
    /// a trend fitted to a debt.
    #[test]
    fn linking_still_demands_a_mortgages_terms() {
        let bare = AccountMetadata::Mortgage(MortgageMeta::default());
        let problems = bare
            .validate_for(AccountKind::Mortgage, ValidationMode::Linked)
            .expect_err("a mortgage needs its schedule on every path");
        for key in [
            "original_amount_minor",
            "interest_rate_bps",
            "rate_type",
            "term_months",
            "start_date",
        ] {
            assert!(
                problems.iter().any(|p| p.starts_with(key)),
                "expected {key} in {problems:?}"
            );
        }
        // A lender is a label, not a term — the feed supplies the institution anyway.
        assert!(!problems.iter().any(|p| p.starts_with("lender")));

        assert!(AccountMetadata::Mortgage(fixed_mortgage())
            .validate_for(AccountKind::Mortgage, ValidationMode::Linked)
            .is_ok());
    }

    /// And a student loan links with none of it, on either path — it has no schedule for
    /// `AMORTISING_REQUIRED` to insist on, so the linked path stays lenient for it.
    #[test]
    fn linking_a_student_loan_demands_nothing() {
        let meta = AccountMetadata::StudentLoan(StudentLoanMeta::default());
        assert!(meta
            .validate_for(AccountKind::StudentLoan, ValidationMode::Linked)
            .is_ok());
    }
}
