//! Shared domain types and the workspace error type. No web framework, no database
//! connection management — just the vocabulary every other crate speaks.

pub mod categories;
pub mod crons;
pub mod currencies;
pub mod equity;
pub mod error;
pub mod merchants;
pub mod providers;
pub mod rules;
pub mod settings;
pub mod stock_prices;
pub mod transactions;
pub mod types;
pub mod valuations;

pub use categories::{Category, CategoryNode, SaveCategory};
pub use crons::{Cron, CronRun, CronRunResult, SaveCron};
pub use currencies::{Currency, NewCurrency};
pub use equity::{AccountEquity, EquityExercise, EquityGrant, SaveExercise, SaveGrant, VestingStatus};
pub use error::{AppError, AppResult, ErrorBody, ErrorDetail};
pub use merchants::{Merchant, SaveMerchant};
pub use providers::{LinkProviderAccount, Provider, ProviderSync, SaveProvider, SyncRequest};
pub use rules::{
    PreviewMatch, PreviewRequest, Rule, RuleApplicationDetail, RulePreview, RuleRun, RunResult,
    SaveRule,
};
pub use settings::{Settings, UpdateSettings};
pub use stock_prices::StockPrice;
pub use transactions::{LinkRequest, SaveTransaction, Transaction, TransferRequest, TxQuery};
pub use types::{
    class_of, Account, AccountClass, AccountKind, AccountMetadata, DepositoryMeta, GenericMeta,
    LoanMeta, MortgageMeta, PropertyMeta, RateType, SaveAccount, SetSecuredBy, SharesMeta,
    VehicleMeta,
};
pub use valuations::{NewValuation, Valuation};
