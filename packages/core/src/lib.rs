//! Shared domain types and the workspace error type. No web framework, no database
//! connection management — just the vocabulary every other crate speaks.

pub mod brokerage;
pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod error;
pub mod forecast;
pub mod merchants;
pub mod people;
pub mod providers;
pub mod rules;
pub mod settings;
pub mod stock_prices;
pub mod transactions;
pub mod types;
pub mod valuations;

pub use brokerage::{
    BrokerageActivity30d, BrokerageImportResult, BrokerageSnapshot, Dividend, DividendDetail,
    DividendWithholding, HoldingLot, LotKind, Position, SaveHoldingLot, WalletBalance,
};
pub use categories::{Category, CategoryKind, CategoryNode, SaveCategory};
pub use crons::{Cron, CronKind, CronRun, CronRunResult, SaveCron};
pub use currencies::{Currency, NewCurrency};
pub use equity::{
    AccountEquity, EquityExercise, EquityGrant, SaveExercise, SaveGrant, VestingStatus,
};
pub use error::{AppError, AppResult, ErrorBody, ErrorDetail};
pub use forecast::{
    ForecastAssumption, ForecastEvent, ForecastEventKind, ForecastTargetType,
    SaveForecastAssumption, SaveForecastEvent,
};
pub use merchants::{Merchant, SaveMerchant};
pub use people::{
    effective_ownership, Ownership, Person, SavePerson, SetOwnership, SetOwnershipBulk,
};
pub use providers::{
    LinkGroupMember, LinkProviderAccount, LinkProviderGroup, Provider, ProviderAccount,
    ProviderKind, ProviderSync, SaveProvider, StudentLoanImportResult, SyncOutcome, SyncRequest,
};
pub use rules::{
    PreviewMatch, PreviewRequest, Rule, RuleApplicationDetail, RulePreview, RuleRun, RuleRunKind,
    RunResult, SaveRule,
};
pub use settings::{Settings, UpdateSettings};
pub use stock_prices::StockPrice;
pub use transactions::{
    BulkDelete, BulkResult, BulkUpdate, LinkRequest, SaveTransaction, Transaction, TransferRequest,
    TxQuery,
};
pub use types::{
    Account, AccountClass, AccountKind, AccountMetadata, AreaUnit, BrokerageMeta, CryptoMeta,
    DepositoryMeta, GenericMeta, Interval, LoanMeta, MileageUnit, MortgageMeta, PropertyMeta,
    RateType, RepaymentFrequency, SaveAccount, SetSecuredBy, SharesMeta, TaxTreatment,
    ValidationMode, VehicleMeta,
};
pub use valuations::{NewValuation, Valuation, ValuationSource};
