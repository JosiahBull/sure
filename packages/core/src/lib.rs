//! Shared domain types and the workspace error type. No web framework, no database
//! connection management — just the vocabulary every other crate speaks.

// Money is held in minor units and written with a `dollars_cents` digit grouping
// (e.g. `1_000_000_000_000_00` == $1,000,000,000,000.00 — see `money::MAX_MONEY_MINOR`);
// clippy's grouping lint fights that convention, which `sure-api` and `sure-dal` already
// allow crate-wide for the same reason.
#![allow(clippy::inconsistent_digit_grouping)]

pub mod brokerage;
pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod error;
pub mod forecast;
pub mod income;
pub mod iso_date;
pub mod life_events;
pub mod merchants;
pub mod money;
pub mod people;
pub mod providers;
pub mod rules;
pub mod settings;
pub mod stock_prices;
pub mod tax;
pub mod transactions;
pub mod types;
pub mod valuations;

pub use brokerage::{
    BrokerageActivity30d, BrokerageImportResult, BrokerageSnapshot, Dividend, DividendDetail,
    DividendWithholding, HoldingLot, LotKind, Position, SaveHoldingLot, WalletBalance,
};
pub use categories::{Category, CategoryKind, CategoryNode, SaveCategory, MAX_CATEGORY_DEPTH};
pub use crons::{Cron, CronKind, CronRun, CronRunResult, SaveCron};
pub use currencies::{Currency, NewCurrency};
pub use equity::{
    AccountEquity, EquityExercise, EquityGrant, SaveExercise, SaveGrant, VestingStatus,
};
pub use error::{AppError, AppResult, ErrorBody, ErrorDetail};
pub use forecast::{ForecastAssumption, ForecastTargetType, SaveForecastAssumption};
pub use income::{
    IncomeBasis, IncomeStream, IncomeStreamStep, PayFrequency, PayStep, SaveIncomeStream,
    SaveIncomeStreamStep, TakeHome, TakeHomeSource,
};
pub use iso_date::IsoDate;
pub use life_events::{
    effect_amounts_in_range, EffectColumns, EffectTarget, ForecastEvent, ForecastEventEffect,
    ForecastEventRelation, LifeEffectKind, LifeEffectSpec, LifeEventKind, RelationKind,
    SaveForecastEvent, SaveForecastEventRelation, StepAmount,
};
pub use merchants::{Merchant, SaveMerchant};
pub use money::{Money, MAX_MONEY_MINOR};
pub use people::{
    effective_ownership, Ownership, Person, SavePerson, SetOwnership, SetOwnershipBulk,
};
pub use providers::{
    AsbImportResult, AsbMatch, AsbUndoResult, AsbUploadResult, LinkGroupMember,
    LinkProviderAccount, LinkProviderGroup, Provider, ProviderAccount, ProviderKind, ProviderSync,
    SaveProvider, StudentLoanImportResult, SyncOutcome, SyncRequest,
};
pub use rules::{
    PreviewMatch, PreviewRequest, Rule, RuleApplicationDetail, RulePreview, RuleRun, RuleRunKind,
    RunResult, SaveRule,
};
pub use settings::{Settings, UpdateSettings};
pub use stock_prices::StockPrice;
pub use tax::{
    average_take_home_bps, latest_scale, marginal_take_home_bps, paye, scale_for, PayeBreakdown,
    PayeInput, TaxScale, TaxScaleId, KIWISAVER_DEFAULT_BPS, KIWISAVER_EMPLOYEE_RATES_BPS,
    NZ_TAX_SCALES,
};
pub use transactions::{
    BulkDelete, BulkResult, BulkUpdate, LinkRequest, SaveTransaction, Transaction, TransferRequest,
    TxQuery,
};
pub use types::{
    Account, AccountClass, AccountKind, AccountMetadata, AreaUnit, BrokerageMeta, CryptoMeta,
    DepositoryMeta, GenericMeta, Interval, LoanMeta, MileageUnit, MortgageMeta, PropertyMeta,
    RateType, RepaymentFrequency, SaveAccount, SetSecuredBy, SharesMeta, StudentLoanMeta,
    TaxTreatment, ValidationMode, VehicleMeta,
};
pub use valuations::{NewValuation, Valuation, ValuationSource};
