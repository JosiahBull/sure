use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The kinds of financial account Sure understands. `kind` selects type-specific
/// behaviour (how balance and net-worth contribution are computed); free-form
/// per-kind configuration lives in an account's `metadata`.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
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
            SharesNz | SharesUs | SharesPrivate | Brokerage => AccountClass::Investment,
        }
    }
}

/// Map a stored `kind` string to its class label, for callers that only have the
/// text (e.g. dynamic report queries).
pub fn class_of(kind: &str) -> &'static str {
    match kind {
        "cash" | "bank" | "savings" => "cash",
        "credit_card" | "revolving_credit" | "mortgage" | "student_loan" | "loan"
        | "liability" => "liability",
        "shares_nz" | "shares_us" | "shares_private" | "brokerage" => "investment",
        _ => "asset",
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

/// Bank / cash / savings / card accounts.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct DepositoryMeta {
    /// Account or card number (store a masked value if you like, e.g. `••4321`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_number: Option<String>,
    /// Credit limit in minor units, for a `credit_card`/`revolving_credit` account —
    /// lets "remaining borrowing" (limit minus what's owed) be shown. Meaningless for
    /// other depository kinds, so left unset there. Auto-populated on sync for
    /// providers that report a live limit (e.g. Akahu); editable manually otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_limit_minor: Option<i64>,
    /// A link to online banking or the statement portal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Real estate.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
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

/// A generic loan (personal loan, student loan, vehicle financing, ...).
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct LoanMeta {
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
    /// Overall loan term, in months.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term_months: Option<i64>,
    /// ISO-8601 date the loan started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
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
    /// Broker or platform (e.g. Sharesies, Hatch, IBKR).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker: Option<String>,
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
    Vehicle(VehicleMeta),
    Shares(SharesMeta),
    Brokerage(BrokerageMeta),
    Generic(GenericMeta),
}

impl AccountMetadata {
    /// The metadata profile discriminant expected for a given account kind.
    pub fn profile_for(kind: AccountKind) -> &'static str {
        use AccountKind::*;
        match kind {
            Cash | Bank | Savings | CreditCard | RevolvingCredit => "depository",
            RealEstate => "property",
            Mortgage => "mortgage",
            Loan | StudentLoan => "loan",
            Vehicle => "vehicle",
            SharesNz | SharesUs | SharesPrivate => "shares",
            Brokerage => "brokerage",
            Asset | Liability => "generic",
        }
    }

    /// The discriminant of this value.
    pub fn profile(&self) -> &'static str {
        match self {
            AccountMetadata::Depository(_) => "depository",
            AccountMetadata::Property(_) => "property",
            AccountMetadata::Mortgage(_) => "mortgage",
            AccountMetadata::Loan(_) => "loan",
            AccountMetadata::Vehicle(_) => "vehicle",
            AccountMetadata::Shares(_) => "shares",
            AccountMetadata::Brokerage(_) => "brokerage",
            AccountMetadata::Generic(_) => "generic",
        }
    }

    /// An empty metadata value with the right variant for `kind`.
    pub fn default_for(kind: AccountKind) -> Self {
        use AccountKind::*;
        match kind {
            Cash | Bank | Savings | CreditCard | RevolvingCredit => {
                AccountMetadata::Depository(DepositoryMeta::default())
            }
            RealEstate => AccountMetadata::Property(PropertyMeta::default()),
            Mortgage => AccountMetadata::Mortgage(MortgageMeta::default()),
            Loan | StudentLoan => AccountMetadata::Loan(LoanMeta::default()),
            Vehicle => AccountMetadata::Vehicle(VehicleMeta::default()),
            SharesNz | SharesUs | SharesPrivate => AccountMetadata::Shares(SharesMeta::default()),
            Brokerage => AccountMetadata::Brokerage(BrokerageMeta::default()),
            Asset | Liability => AccountMetadata::Generic(GenericMeta::default()),
        }
    }
}

// ---------------------------------------------------------------------------
// Account wire/domain shape. The DAL owns `AccountRow` (the raw SQLite row) and
// converts it into `Account` below; the API crate uses `Account`/`SaveAccount`
// directly as its request/response bodies.
// ---------------------------------------------------------------------------

#[derive(Serialize, ToSchema)]
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
    pub sort_order: i64,
    /// For a liability, the asset account it is secured against (e.g. a mortgage's home).
    pub secured_by_account_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize, ToSchema)]
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
}

#[derive(Deserialize, ToSchema)]
pub struct SetSecuredBy {
    /// The asset account this (liability) account is secured against; `null` to unlink.
    pub secured_by_account_id: Option<i64>,
}
