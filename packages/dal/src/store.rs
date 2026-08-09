//! `SqliteStore` implements every `sure_app::ports` repo trait by delegating to this
//! crate's existing per-entity modules — the SQL itself is untouched. Each method maps
//! this crate's row-shaped types into the port's plain equivalents (see
//! `sure_app::ports`'s module doc for why they're distinct types rather than shared
//! ones): `sure-app` cannot depend back on `sure-dal` (this crate depends on it to see
//! the port traits), so the two sides can't share a struct definition.

use std::collections::HashSet;

use async_trait::async_trait;
use chrono::NaiveDate;
use sure_app::ports::{
    AccountCurrency, AccountRepo, ActiveAccount, Activity30dRow, AssetAccount, BrokerageRepo,
    CategoryRepo, CostLotRow, CronRepo, CurrencyDecimals, CurrencyRepo, DividendImport, EquityRepo,
    ExchangeRateRepo, ExchangeRateRow, ForecastRepo, FxRatesRepo, HoldingImport, HoldingRow,
    ImportCounts, ImportHistoryRepo, ImportRow, LedgerTx, LedgerValuation, MerchantRepo,
    PersonRepo, PlannedApplication, ProviderRepo, ReportCategory, ReportRepo, RuleRepo,
    SecuredLiabilityAccount, SettingsRepo, SharesTicker, SnapshotRepo, StockPriceCacheRepo,
    TransactionRepo, TransferRepo, TxCtx, ValuationRepo, WalletRow,
};
use sure_core::{
    Account, AccountEquity, AppError, AppResult, BulkUpdate, Category, CategoryNode, Cron, CronRun,
    CronRunResult, Currency, DividendDetail, EquityExercise, EquityGrant, ForecastAssumption,
    ForecastEvent, ForecastTargetType, HoldingLot, ImportRecord, IncomeStream, LinkProviderAccount,
    LinkProviderGroup, LinkRequest, Merchant, NewCurrency, NewValuation, Ownership, Person,
    Provider, ProviderSync, Rule, RuleApplicationDetail, RuleRun, RuleRunKind, RunResult,
    SaveAccount, SaveCategory, SaveCron, SaveExercise, SaveForecastAssumption, SaveForecastEvent,
    SaveGrant, SaveHoldingLot, SaveIncomeStream, SaveMerchant, SavePerson, SaveProvider, SaveRule,
    SaveTaxScale, SaveTransaction, Settings, StockPrice, StoredTaxScale, SyncOutcome, TaxScaleId,
    Transaction, TransferRequest, TxQuery, UpdateSettings, Valuation, VestingStatus,
};

use crate::Db;

/// One struct wrapping the pool implements every repo port a service needs, so the
/// composition root can hand out `store.clone()` for each of a service's dependencies.
#[derive(Clone)]
pub struct SqliteStore {
    pub db: Db,
}

impl SqliteStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AccountRepo for SqliteStore {
    async fn list(&self, include_archived: bool) -> AppResult<Vec<Account>> {
        crate::accounts::list(&self.db, include_archived).await
    }

    async fn get(&self, id: i64) -> AppResult<Account> {
        crate::accounts::get(&self.db, id).await
    }

    async fn create(&self, input: SaveAccount) -> AppResult<Account> {
        crate::accounts::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveAccount) -> AppResult<Account> {
        crate::accounts::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::accounts::delete(&self.db, id).await
    }

    async fn set_secured_by(&self, id: i64, target: Option<i64>) -> AppResult<Account> {
        crate::accounts::set_secured_by(&self.db, id, target).await
    }

    async fn set_ownership(&self, id: i64, ownership: Ownership) -> AppResult<Account> {
        crate::accounts::set_ownership(&self.db, id, ownership).await
    }

    async fn set_ownership_bulk(&self, ids: &[i64], ownership: Ownership) -> AppResult<u64> {
        crate::accounts::set_ownership_bulk(&self.db, ids, ownership).await
    }

    async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>> {
        Ok(crate::accounts::list_shares_tickers(&self.db)
            .await?
            .into_iter()
            .map(|t| SharesTicker {
                ticker: t.ticker,
                exchange: t.exchange,
            })
            .collect())
    }

    async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>> {
        Ok(crate::accounts::list_brokerage_tickers(&self.db)
            .await?
            .into_iter()
            .map(|t| SharesTicker {
                ticker: t.ticker,
                exchange: t.exchange,
            })
            .collect())
    }

    async fn set_credit_limit(&self, account_id: i64, credit_limit_minor: i64) -> AppResult<()> {
        crate::accounts::set_credit_limit(&self.db, account_id, credit_limit_minor).await
    }

    async fn set_original_amount(
        &self,
        account_id: i64,
        original_amount_minor: i64,
    ) -> AppResult<()> {
        crate::accounts::set_original_amount(&self.db, account_id, original_amount_minor).await
    }

    async fn set_account_number_if_unset(
        &self,
        account_id: i64,
        account_number: &str,
    ) -> AppResult<()> {
        crate::accounts::set_account_number_if_unset(&self.db, account_id, account_number).await
    }

    async fn set_institution_if_unset(&self, account_id: i64, institution: &str) -> AppResult<()> {
        crate::accounts::set_institution_if_unset(&self.db, account_id, institution).await
    }
}

#[async_trait]
impl PersonRepo for SqliteStore {
    async fn list(&self) -> AppResult<Vec<Person>> {
        crate::people::list(&self.db).await
    }

    async fn get(&self, id: i64) -> AppResult<Person> {
        crate::people::get(&self.db, id).await
    }

    async fn create(&self, input: SavePerson) -> AppResult<Person> {
        crate::people::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SavePerson) -> AppResult<Person> {
        crate::people::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::people::delete(&self.db, id).await
    }
}

#[async_trait]
impl BrokerageRepo for SqliteStore {
    async fn positions_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<HoldingRow>> {
        Ok(crate::brokerage::positions_at(&self.db, account_id, as_of)
            .await?
            .into_iter()
            .map(|p| HoldingRow {
                ticker: p.ticker,
                exchange: p.exchange,
                currency_code: p.currency_code,
                name: p.name,
                quantity: p.quantity,
            })
            .collect())
    }

    async fn wallet_balances_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<WalletRow>> {
        Ok(
            crate::brokerage::wallet_balances_at(&self.db, account_id, as_of)
                .await?
                .into_iter()
                .map(|w| WalletRow {
                    currency_code: w.currency_code,
                    amount_minor: w.amount_minor,
                })
                .collect(),
        )
    }

    async fn lots_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<CostLotRow>> {
        Ok(crate::brokerage::lots_at(&self.db, account_id, as_of)
            .await?
            .into_iter()
            .map(|l| CostLotRow {
                ticker: l.ticker,
                exchange: l.exchange,
                currency_code: l.currency_code,
                quantity: l.quantity,
                unit_price: l.unit_price,
                fee_minor: l.fee_minor,
                kind: l.kind, // already `sure_core::LotKind`, parsed in the DAL
            })
            .collect())
    }

    async fn activity_30d(&self, account_id: i64, as_of: &str) -> AppResult<Activity30dRow> {
        let a = crate::brokerage::activity_30d(&self.db, account_id, as_of).await?;
        Ok(Activity30dRow {
            contributions_minor: a.contributions_minor,
            withdrawals_minor: a.withdrawals_minor,
            trades: a.trades,
        })
    }

    async fn account_tickers(&self, account_id: i64) -> AppResult<Vec<(String, String)>> {
        crate::brokerage::account_tickers(&self.db, account_id).await
    }

    async fn earliest_activity_date(&self, account_id: i64) -> AppResult<Option<String>> {
        crate::brokerage::earliest_activity_date(&self.db, account_id).await
    }

    async fn list_holdings(&self, account_id: i64) -> AppResult<Vec<HoldingLot>> {
        crate::brokerage::list_holdings(&self.db, account_id).await
    }

    async fn create_holding(
        &self,
        account_id: i64,
        input: SaveHoldingLot,
    ) -> AppResult<HoldingLot> {
        crate::brokerage::create_holding(&self.db, account_id, input).await
    }

    async fn delete_holding(&self, id: i64) -> AppResult<()> {
        crate::brokerage::delete_holding(&self.db, id).await
    }

    async fn list_dividends(&self, account_id: i64) -> AppResult<Vec<DividendDetail>> {
        crate::brokerage::list_dividends(&self.db, account_id).await
    }

    async fn import_export(
        &self,
        account_id: i64,
        account_currency: &str,
        provider_tag: &str,
        wallet_rows: &[ImportRow],
        holdings: &[HoldingImport],
        dividends: &[DividendImport],
    ) -> AppResult<ImportCounts> {
        let wallet_rows: Vec<crate::providers::ImportRow> = wallet_rows
            .iter()
            .map(|r| crate::providers::ImportRow {
                external_id: r.external_id.clone(),
                posted_at: r.posted_at.clone(),
                amount_minor: r.amount_minor,
                currency_code: r.currency_code.clone(),
                description: r.description.clone(),
                merchant: r.merchant.clone(),
                category_name: r.category_name.clone(),
                category_group: r.category_group.clone(),
                category_kind: r.category_kind,
                is_one_off: r.is_one_off,
            })
            .collect();
        let holdings: Vec<crate::brokerage::HoldingImport> = holdings
            .iter()
            .map(|h| crate::brokerage::HoldingImport {
                ticker: h.ticker.clone(),
                exchange: h.exchange.clone(),
                name: h.name.clone(),
                currency_code: h.currency_code.clone(),
                trade_date: h.trade_date.clone(),
                quantity: h.quantity,
                unit_price: h.unit_price,
                fee_minor: h.fee_minor,
                kind: h.kind,
                external_id: h.external_id.clone(),
            })
            .collect();
        let dividends: Vec<crate::brokerage::DividendImport> = dividends
            .iter()
            .map(|d| crate::brokerage::DividendImport {
                ticker: d.ticker.clone(),
                exchange: d.exchange.clone(),
                record_date: d.record_date.clone(),
                paid_date: d.paid_date.clone(),
                shares_held: d.shares_held,
                gross_amount_minor: d.gross_amount_minor,
                net_amount_minor: d.net_amount_minor,
                currency_code: d.currency_code.clone(),
                external_id: d.external_id.clone(),
                withholdings: d
                    .withholdings
                    .iter()
                    .map(|w| crate::brokerage::WithholdingImport {
                        owed_to: w.owed_to.clone(),
                        tax_amount_minor: w.tax_amount_minor,
                        tax_credit_minor: w.tax_credit_minor,
                        currency_code: w.currency_code.clone(),
                    })
                    .collect(),
            })
            .collect();
        let counts = crate::brokerage::import_export(
            &self.db,
            account_id,
            account_currency,
            provider_tag,
            &wallet_rows,
            &holdings,
            &dividends,
        )
        .await?;
        Ok(ImportCounts {
            transactions_imported: counts.transactions_imported,
            transactions_skipped: counts.transactions_skipped,
            holdings_imported: counts.holdings_imported,
            holdings_skipped: counts.holdings_skipped,
            dividends_imported: counts.dividends_imported,
            dividends_skipped: counts.dividends_skipped,
        })
    }

    async fn delete_holdings_by_provider(
        &self,
        account_id: i64,
        provider_tag: &str,
    ) -> AppResult<i64> {
        crate::brokerage::delete_holdings_by_provider(&self.db, account_id, provider_tag).await
    }

    async fn delete_dividends_by_provider(
        &self,
        account_id: i64,
        provider_tag: &str,
    ) -> AppResult<i64> {
        crate::brokerage::delete_dividends_by_provider(&self.db, account_id, provider_tag).await
    }
}

#[async_trait]
impl StockPriceCacheRepo for SqliteStore {
    async fn get_at(
        &self,
        ticker: &str,
        exchange: &str,
        as_of: &str,
    ) -> AppResult<Option<StockPrice>> {
        crate::stock_prices::get_at(&self.db, ticker, exchange, as_of).await
    }

    async fn upsert(
        &self,
        ticker: &str,
        exchange: &str,
        as_of: &str,
        close: &str,
        ccy: &str,
    ) -> AppResult<()> {
        crate::stock_prices::upsert(&self.db, ticker, exchange, as_of, close, ccy)
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl ValuationRepo for SqliteStore {
    async fn list_for_account(&self, account_id: i64) -> AppResult<Vec<Valuation>> {
        crate::valuations::list_for_account(&self.db, account_id).await
    }

    async fn create(&self, account_id: i64, input: NewValuation) -> AppResult<Valuation> {
        crate::valuations::create(&self.db, account_id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::valuations::delete(&self.db, id).await
    }

    async fn upsert_from_brokerage(
        &self,
        account_id: i64,
        as_of: &str,
        value_minor: i64,
        ccy: &str,
    ) -> AppResult<()> {
        crate::valuations::upsert_from_brokerage(&self.db, account_id, as_of, value_minor, ccy)
            .await
            .map(|_| ())
    }

    async fn upsert_from_provider(
        &self,
        account_id: i64,
        as_of: &str,
        value_minor: i64,
        ccy: &str,
    ) -> AppResult<()> {
        crate::valuations::upsert_from_provider(&self.db, account_id, as_of, value_minor, ccy)
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl FxRatesRepo for SqliteStore {
    async fn currency_decimals(&self) -> AppResult<Vec<CurrencyDecimals>> {
        Ok(crate::reports::currency_decimals(&self.db)
            .await?
            .into_iter()
            .map(|c| CurrencyDecimals {
                code: c.code,
                decimal_places: c.decimal_places as i32,
            })
            .collect())
    }

    async fn exchange_rates(&self) -> AppResult<Vec<ExchangeRateRow>> {
        Ok(crate::exchange_rates::latest_per_pair(&self.db)
            .await?
            .into_iter()
            .map(|r| ExchangeRateRow {
                base_code: r.base_code,
                quote_code: r.quote_code,
                rate: r.rate,
                as_of: r.as_of,
            })
            .collect())
    }
}

/// The DAL's row shape for a rule evaluation context, as the port's. One function for both
/// loaders, so a field added to `TxCtx` cannot reach one of them and not the other.
fn tx_ctx(r: crate::rules::TxCtx) -> TxCtx {
    TxCtx {
        id: r.id,
        account_id: r.account_id,
        posted_at: r.posted_at,
        amount_minor: r.amount_minor,
        currency_code: r.currency_code,
        decimal_places: r.decimal_places,
        description: r.description,
        merchant: r.merchant,
        merchant_id: r.merchant_id,
        notes: r.notes,
        category_id: r.category_id,
        is_one_off: r.is_one_off,
        categorized_by_rule_id: r.categorized_by_rule_id,
        account_name: r.account_name,
        account_kind: r.account_kind, // already `sure_core::AccountKind`, parsed in the DAL
    }
}

#[async_trait]
impl RuleRepo for SqliteStore {
    async fn load_contexts(&self) -> AppResult<Vec<TxCtx>> {
        Ok(crate::rules::load_contexts(&self.db)
            .await?
            .into_iter()
            .map(tx_ctx)
            .collect())
    }

    async fn load_uncategorized_contexts(&self) -> AppResult<Vec<TxCtx>> {
        Ok(crate::rules::load_uncategorized_contexts(&self.db)
            .await?
            .into_iter()
            .map(tx_ctx)
            .collect())
    }

    async fn persist_run(
        &self,
        rule_id: Option<i64>,
        kind: RuleRunKind,
        matched: i64,
        applications: Vec<PlannedApplication>,
    ) -> AppResult<RunResult> {
        let applications = applications
            .into_iter()
            .map(|a| crate::rules::PlannedApplication {
                rule_id: a.rule_id,
                transaction_id: a.transaction_id,
                prev_category_id: a.prev_category_id,
                new_category_id: a.new_category_id,
                prev_categorized_by_rule_id: a.prev_categorized_by_rule_id,
                new_categorized_by_rule_id: a.new_categorized_by_rule_id,
                prev_one_off: a.prev_one_off,
                new_one_off: a.new_one_off,
                prev_merchant_id: a.prev_merchant_id,
                new_merchant_id: a.new_merchant_id,
            })
            .collect();
        crate::rules::persist_run(&self.db, rule_id, kind, matched, applications).await
    }

    async fn list(&self) -> AppResult<Vec<Rule>> {
        crate::rules::list(&self.db).await
    }

    async fn enabled_rules(&self) -> AppResult<Vec<Rule>> {
        crate::rules::enabled_rules(&self.db).await
    }

    async fn get(&self, id: i64) -> AppResult<Rule> {
        crate::rules::get(&self.db, id).await
    }

    async fn create(&self, input: SaveRule) -> AppResult<Rule> {
        crate::rules::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveRule) -> AppResult<Rule> {
        crate::rules::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::rules::delete(&self.db, id).await
    }

    async fn list_runs(&self) -> AppResult<Vec<RuleRun>> {
        crate::rules::list_runs(&self.db).await
    }

    async fn run_applications(&self, run_id: i64) -> AppResult<Vec<RuleApplicationDetail>> {
        crate::rules::run_applications(&self.db, run_id).await
    }

    async fn undo_run(&self, run_id: i64) -> AppResult<RunResult> {
        crate::rules::undo_run(&self.db, run_id).await
    }
}

#[async_trait]
impl ReportRepo for SqliteStore {
    async fn base_currency(&self) -> AppResult<String> {
        crate::settings::base_currency(&self.db).await
    }

    async fn account_currencies(&self) -> AppResult<Vec<AccountCurrency>> {
        Ok(crate::reports::account_currencies(&self.db)
            .await?
            .into_iter()
            .map(|a| AccountCurrency {
                id: a.id,
                currency_code: a.currency_code,
                ownership: a.ownership,
            })
            .collect())
    }

    async fn transactions(&self, from: Option<NaiveDate>) -> AppResult<Vec<LedgerTx>> {
        Ok(crate::reports::transactions(&self.db, from)
            .await?
            .into_iter()
            .map(|t| LedgerTx {
                account_id: t.account_id,
                posted_at: t.posted_at,
                amount_minor: t.amount_minor,
                currency_code: t.currency_code,
            })
            .collect())
    }

    async fn valuations(&self, from: Option<NaiveDate>) -> AppResult<Vec<LedgerValuation>> {
        Ok(crate::reports::valuations(&self.db, from)
            .await?
            .into_iter()
            .map(|v| LedgerValuation {
                account_id: v.account_id,
                as_of: v.as_of,
                value_minor: v.value_minor,
                currency_code: v.currency_code,
            })
            .collect())
    }

    async fn categories(&self) -> AppResult<Vec<ReportCategory>> {
        Ok(crate::reports::categories(&self.db)
            .await?
            .into_iter()
            .map(|c| ReportCategory {
                id: c.id,
                parent_id: c.parent_id,
                name: c.name,
                color: c.color,
                kind: c.kind,
            })
            .collect())
    }

    async fn spend_transactions(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> AppResult<Vec<sure_app::ports::SpendTransaction>> {
        Ok(crate::reports::spend_transactions(&self.db, from, to)
            .await?
            .into_iter()
            .map(|t| sure_app::ports::SpendTransaction {
                attribution: t.attribution,
                posted_at: t.posted_at,
                amount_minor: t.amount_minor,
                currency_code: t.currency_code,
                category_id: t.category_id,
                is_one_off: t.is_one_off,
                linked_transaction_id: t.linked_transaction_id,
                account_kind: t.account_kind, // already `sure_core::AccountKind`, parsed in the DAL
            })
            .collect())
    }

    async fn earliest_transaction_date(&self) -> AppResult<Option<String>> {
        crate::reports::earliest_transaction_date(&self.db).await
    }

    async fn earliest_valuation_date(&self) -> AppResult<Option<String>> {
        crate::reports::earliest_valuation_date(&self.db).await
    }

    async fn active_accounts(&self) -> AppResult<Vec<ActiveAccount>> {
        Ok(crate::reports::active_accounts(&self.db)
            .await?
            .into_iter()
            .map(|a| ActiveAccount {
                ownership: a.ownership,
                id: a.id,
                name: a.name,
                kind: a.kind, // already `sure_core::AccountKind`, parsed in the DAL
                currency_code: a.currency_code,
            })
            .collect())
    }

    async fn account(&self, id: i64) -> AppResult<AssetAccount> {
        let a = crate::reports::account(&self.db, id).await?;
        Ok(AssetAccount {
            id: a.id,
            name: a.name,
            currency_code: a.currency_code,
        })
    }

    async fn secured_liabilities(&self, asset_id: i64) -> AppResult<Vec<SecuredLiabilityAccount>> {
        Ok(crate::reports::secured_liabilities(&self.db, asset_id)
            .await?
            .into_iter()
            .map(|l| SecuredLiabilityAccount {
                id: l.id,
                name: l.name,
                kind: l.kind, // already `sure_core::AccountKind`, parsed in the DAL
                currency_code: l.currency_code,
            })
            .collect())
    }
}

#[async_trait]
impl ProviderRepo for SqliteStore {
    async fn list(&self) -> AppResult<Vec<Provider>> {
        crate::providers::list(&self.db).await
    }

    async fn get(&self, id: i64) -> AppResult<Provider> {
        crate::providers::get(&self.db, id).await
    }

    async fn create(&self, input: SaveProvider) -> AppResult<Provider> {
        crate::providers::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveProvider) -> AppResult<Provider> {
        crate::providers::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::providers::delete(&self.db, id).await
    }

    async fn link(&self, input: LinkProviderAccount) -> AppResult<Provider> {
        crate::providers::link(&self.db, input).await
    }

    async fn link_group(&self, input: LinkProviderGroup) -> AppResult<Vec<Provider>> {
        crate::providers::link_group(&self.db, input).await
    }

    async fn list_syncs(&self, provider_id: i64) -> AppResult<Vec<ProviderSync>> {
        crate::providers::list_syncs(&self.db, provider_id).await
    }

    async fn account_currency(&self, account_id: i64) -> AppResult<String> {
        crate::providers::account_currency(&self.db, account_id).await
    }

    async fn import_transactions(
        &self,
        account_id: i64,
        account_currency: &str,
        provider_tag: &str,
        rows: &[ImportRow],
    ) -> AppResult<(i64, i64)> {
        let rows: Vec<crate::providers::ImportRow> = rows
            .iter()
            .map(|r| crate::providers::ImportRow {
                external_id: r.external_id.clone(),
                posted_at: r.posted_at.clone(),
                amount_minor: r.amount_minor,
                currency_code: r.currency_code.clone(),
                description: r.description.clone(),
                merchant: r.merchant.clone(),
                category_name: r.category_name.clone(),
                category_group: r.category_group.clone(),
                category_kind: r.category_kind,
                is_one_off: r.is_one_off,
            })
            .collect();
        crate::providers::import_transactions(
            &self.db,
            account_id,
            account_currency,
            provider_tag,
            &rows,
        )
        .await
    }

    async fn update_last_synced(&self, id: i64) -> AppResult<()> {
        crate::providers::update_last_synced(&self.db, id).await
    }

    async fn record_sync(
        &self,
        provider_id: i64,
        imported: i64,
        skipped: i64,
        status: SyncOutcome,
        detail: Option<&str>,
    ) -> AppResult<ProviderSync> {
        crate::providers::record_sync(&self.db, provider_id, imported, skipped, status, detail)
            .await
    }
}

#[async_trait]
impl ExchangeRateRepo for SqliteStore {
    async fn base_currency(&self) -> AppResult<String> {
        crate::settings::base_currency(&self.db).await
    }

    async fn known_currency_codes(&self) -> AppResult<HashSet<String>> {
        Ok(crate::currencies::list(&self.db)
            .await?
            .into_iter()
            .map(|c| c.code)
            .collect())
    }

    async fn upsert_rate(
        &self,
        base_code: &str,
        quote_code: &str,
        as_of: &str,
        rate: &str,
    ) -> AppResult<()> {
        crate::exchange_rates::upsert(&self.db, base_code, quote_code, as_of, rate)
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl TransferRepo for SqliteStore {
    async fn link_transfers(&self, window_days: i64) -> AppResult<i64> {
        crate::transactions::link_transfers(&self.db, window_days).await
    }
}

#[async_trait]
impl TransactionRepo for SqliteStore {
    async fn list(&self, q: TxQuery) -> AppResult<Vec<Transaction>> {
        crate::transactions::list(&self.db, q).await
    }

    async fn get(&self, id: i64) -> AppResult<Transaction> {
        crate::transactions::get(&self.db, id).await
    }

    async fn create(&self, input: SaveTransaction) -> AppResult<Transaction> {
        crate::transactions::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveTransaction) -> AppResult<Transaction> {
        crate::transactions::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::transactions::delete(&self.db, id).await
    }

    async fn bulk_update(&self, input: BulkUpdate) -> AppResult<i64> {
        crate::transactions::bulk_update(&self.db, input).await
    }

    async fn bulk_delete(&self, ids: &[i64]) -> AppResult<i64> {
        crate::transactions::bulk_delete(&self.db, ids).await
    }

    async fn earliest_posted_at_from_other_feed(
        &self,
        account_id: i64,
        exclude_provider: &str,
    ) -> AppResult<Option<String>> {
        crate::transactions::earliest_posted_at_from_other_feed(
            &self.db,
            account_id,
            exclude_provider,
        )
        .await
    }

    async fn delete_by_provider(&self, account_id: i64, provider_tag: &str) -> AppResult<i64> {
        crate::transactions::delete_by_provider(&self.db, account_id, provider_tag).await
    }

    async fn sample_external_ids(&self, provider_prefix: &str) -> AppResult<Vec<(i64, String)>> {
        crate::transactions::sample_external_ids(&self.db, provider_prefix).await
    }

    async fn amounts_for_matching(
        &self,
        account_ids: &[i64],
        limit: i64,
    ) -> AppResult<Vec<(i64, String, i64)>> {
        crate::transactions::amounts_for_matching(&self.db, account_ids, limit).await
    }

    async fn earliest_posted_at(&self, account_id: i64) -> AppResult<Option<String>> {
        crate::transactions::earliest_posted_at(&self.db, account_id).await
    }

    async fn sum_amount_minor(&self, account_id: i64) -> AppResult<i64> {
        crate::transactions::sum_amount_minor(&self.db, account_id).await
    }

    async fn link(&self, id: i64, req: LinkRequest) -> AppResult<Transaction> {
        crate::transactions::link(&self.db, id, req).await
    }

    async fn unlink(&self, id: i64) -> AppResult<Transaction> {
        crate::transactions::unlink(&self.db, id).await
    }

    async fn create_transfer(&self, req: TransferRequest) -> AppResult<Vec<Transaction>> {
        crate::transactions::create_transfer(&self.db, req).await
    }
}

#[async_trait]
impl CategoryRepo for SqliteStore {
    async fn list(&self) -> AppResult<Vec<Category>> {
        crate::categories::list(&self.db).await
    }

    async fn tree(&self) -> AppResult<Vec<CategoryNode>> {
        crate::categories::tree(&self.db).await
    }

    async fn create(&self, input: SaveCategory) -> AppResult<Category> {
        crate::categories::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveCategory) -> AppResult<Category> {
        crate::categories::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::categories::delete(&self.db, id).await
    }
}

#[async_trait]
impl MerchantRepo for SqliteStore {
    async fn list(&self) -> AppResult<Vec<Merchant>> {
        crate::merchants::list(&self.db).await
    }

    async fn create(&self, input: SaveMerchant) -> AppResult<Merchant> {
        crate::merchants::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveMerchant) -> AppResult<Merchant> {
        crate::merchants::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::merchants::delete(&self.db, id).await
    }
}

#[async_trait]
impl CurrencyRepo for SqliteStore {
    async fn list(&self) -> AppResult<Vec<Currency>> {
        crate::currencies::list(&self.db).await
    }

    async fn upsert(&self, input: NewCurrency) -> AppResult<Currency> {
        crate::currencies::upsert(&self.db, input).await
    }

    async fn delete(&self, code: &str) -> AppResult<()> {
        crate::currencies::delete(&self.db, code).await
    }
}

#[async_trait]
impl SettingsRepo for SqliteStore {
    async fn get(&self) -> AppResult<Settings> {
        crate::settings::get(&self.db).await
    }

    async fn update(&self, input: UpdateSettings) -> AppResult<Settings> {
        crate::settings::update(&self.db, input).await
    }
}

#[async_trait]
impl EquityRepo for SqliteStore {
    async fn list_grants(&self, account_id: i64) -> AppResult<Vec<EquityGrant>> {
        crate::equity::list_grants(&self.db, account_id).await
    }

    async fn create_grant(&self, account_id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
        crate::equity::create_grant(&self.db, account_id, input).await
    }

    async fn update_grant(&self, id: i64, input: SaveGrant) -> AppResult<EquityGrant> {
        crate::equity::update_grant(&self.db, id, input).await
    }

    async fn delete_grant(&self, id: i64) -> AppResult<()> {
        crate::equity::delete_grant(&self.db, id).await
    }

    async fn list_exercises(&self, grant_id: i64) -> AppResult<Vec<EquityExercise>> {
        crate::equity::list_exercises(&self.db, grant_id).await
    }

    async fn create_exercise(
        &self,
        grant_id: i64,
        input: SaveExercise,
    ) -> AppResult<EquityExercise> {
        crate::equity::create_exercise(&self.db, grant_id, input).await
    }

    async fn delete_exercise(&self, id: i64) -> AppResult<()> {
        crate::equity::delete_exercise(&self.db, id).await
    }

    async fn grant_vesting(&self, id: i64, as_of: Option<&str>) -> AppResult<VestingStatus> {
        crate::equity::grant_vesting(&self.db, id, as_of).await
    }

    async fn account_equity(&self, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
        crate::equity::account_equity(&self.db, id, as_of).await
    }

    async fn revalue(&self, id: i64, as_of: Option<&str>) -> AppResult<AccountEquity> {
        crate::equity::revalue(&self.db, id, as_of).await
    }
}

#[async_trait]
impl CronRepo for SqliteStore {
    async fn list(&self) -> AppResult<Vec<Cron>> {
        crate::crons::list(&self.db).await
    }

    async fn create(&self, input: SaveCron) -> AppResult<Cron> {
        crate::crons::create(&self.db, input).await
    }

    async fn update(&self, id: i64, input: SaveCron) -> AppResult<Cron> {
        crate::crons::update(&self.db, id, input).await
    }

    async fn delete(&self, id: i64) -> AppResult<()> {
        crate::crons::delete(&self.db, id).await
    }

    async fn list_runs(&self, cron_id: i64) -> AppResult<Vec<CronRun>> {
        crate::crons::list_runs(&self.db, cron_id).await
    }

    async fn run_one(&self, id: i64, to: Option<&str>) -> AppResult<CronRunResult> {
        crate::crons::run_one(&self.db, id, to).await
    }

    async fn run_all(&self, to: Option<&str>) -> AppResult<CronRunResult> {
        crate::crons::run_all(&self.db, to).await
    }

    async fn undo_run(&self, run_id: i64) -> AppResult<()> {
        crate::crons::undo_run(&self.db, run_id).await
    }
}

#[async_trait]
impl ForecastRepo for SqliteStore {
    async fn list_assumptions(&self) -> AppResult<Vec<ForecastAssumption>> {
        crate::forecast::list_assumptions(&self.db).await
    }

    async fn upsert_assumption(
        &self,
        input: SaveForecastAssumption,
    ) -> AppResult<ForecastAssumption> {
        crate::forecast::upsert_assumption(&self.db, input).await
    }

    async fn clear_assumption(
        &self,
        target_type: ForecastTargetType,
        target_id: i64,
    ) -> AppResult<()> {
        crate::forecast::clear_assumption(&self.db, target_type, target_id).await
    }

    async fn trailing_dividends_minor(&self, account_id: i64, since: &str) -> AppResult<i64> {
        crate::forecast::trailing_dividends_minor(&self.db, account_id, since).await
    }

    async fn list_events(&self) -> AppResult<Vec<ForecastEvent>> {
        crate::forecast::list_events(&self.db).await
    }

    async fn create_event(&self, input: SaveForecastEvent) -> AppResult<ForecastEvent> {
        crate::forecast::create_event(&self.db, input).await
    }

    async fn get_event(&self, id: i64) -> AppResult<ForecastEvent> {
        crate::forecast::get_event(&self.db, id).await
    }

    async fn update_event(&self, id: i64, input: SaveForecastEvent) -> AppResult<ForecastEvent> {
        crate::forecast::update_event(&self.db, id, input).await
    }

    async fn delete_event(&self, id: i64) -> AppResult<()> {
        crate::forecast::delete_event(&self.db, id).await
    }

    async fn list_income_streams(&self) -> AppResult<Vec<IncomeStream>> {
        crate::income::list(&self.db).await
    }

    async fn get_income_stream(&self, id: i64) -> AppResult<IncomeStream> {
        crate::income::get(&self.db, id).await
    }

    async fn create_income_stream(
        &self,
        person_id: i64,
        input: SaveIncomeStream,
    ) -> AppResult<IncomeStream> {
        crate::income::create(&self.db, person_id, input).await
    }

    async fn update_income_stream(
        &self,
        id: i64,
        input: SaveIncomeStream,
    ) -> AppResult<IncomeStream> {
        crate::income::update(&self.db, id, input).await
    }

    async fn delete_income_stream(&self, id: i64) -> AppResult<()> {
        crate::income::delete(&self.db, id).await
    }

    async fn list_tax_scales(&self) -> AppResult<Vec<StoredTaxScale>> {
        crate::tax_scales::list(&self.db).await
    }

    async fn create_tax_scale(
        &self,
        scale_id: TaxScaleId,
        input: SaveTaxScale,
    ) -> AppResult<StoredTaxScale> {
        crate::tax_scales::create(&self.db, scale_id, input).await
    }

    async fn update_tax_scale(&self, id: i64, input: SaveTaxScale) -> AppResult<StoredTaxScale> {
        crate::tax_scales::update(&self.db, id, input).await
    }

    async fn delete_tax_scale(&self, id: i64) -> AppResult<()> {
        crate::tax_scales::delete(&self.db, id).await
    }

    async fn restore_tax_scales(&self) -> AppResult<Vec<StoredTaxScale>> {
        crate::tax_scales::restore_defaults(&self.db).await
    }

    async fn income_transactions(
        &self,
        from: &str,
        account_id: Option<i64>,
    ) -> AppResult<Vec<Transaction>> {
        crate::transactions::list(
            &self.db,
            TxQuery {
                from: Some(from.to_string()),
                account_id,
                ..Default::default()
            },
        )
        .await
    }
}

#[async_trait]
impl ImportHistoryRepo for SqliteStore {
    async fn record(&self, entry: sure_app::ports::NewImport) -> AppResult<()> {
        crate::imports::record(
            &self.db,
            crate::imports::NewImport {
                account_id: entry.account_id,
                source: entry.source,
                provider_tag: entry.provider_tag,
                source_account: entry.source_account,
                filenames: entry.filenames,
                imported: entry.imported,
                skipped: entry.skipped,
                held_back: entry.held_back,
                covered_from: entry.covered_from,
                covered_to: entry.covered_to,
                cutover: entry.cutover,
            },
        )
        .await
    }

    async fn list(&self, account_id: Option<i64>) -> AppResult<Vec<ImportRecord>> {
        crate::imports::list(&self.db, account_id).await
    }
}

#[async_trait]
impl SnapshotRepo for SqliteStore {
    async fn export(&self) -> AppResult<Vec<u8>> {
        // Serialised straight from the rows: no intermediate `serde_json::Value` copy of the
        // whole database (see `crate::snapshot::export_bytes`).
        crate::snapshot::export_bytes(&self.db).await
    }

    async fn import(&self, snapshot: serde_json::Value) -> AppResult<serde_json::Value> {
        let snap = serde_json::from_value(snapshot)
            .map_err(|e| AppError::validation(format!("invalid snapshot: {e}")))?;
        crate::snapshot::import(&self.db, snap).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::sqlite::SqlitePoolOptions;
    use sure_app::reports::{ReportQuery, ReportService};
    use sure_app::SystemClock;
    use sure_core::{AccountKind, SaveAccount};

    use super::*;

    /// The end-to-end half of the money-magnitude guard: a balance report over rows that were
    /// **not** written through the wire type.
    ///
    /// `sure_core::Money` bounds `POST /api/transactions` from now on, but it cannot reach a row
    /// written before it existed — and two paths still bypass it by design (provider import,
    /// snapshot restore). So this test inserts straight into the table with `sqlx`, which is the
    /// only honest way to reproduce what is already on disk, and then asks the real
    /// `ReportService` (over the real `SqliteStore`) for a balance sheet.
    ///
    /// Before the checked aggregation, this call was `[i64::MAX, i64::MAX].iter().sum()`:
    /// `attempt to add with overflow` in debug — a scrubbed 500 on the balance sheet, net worth,
    /// equity and forecast at once, with the offending rows unfindable because the pages that
    /// would list them were the 500ing ones — and a wrap to a small negative in release, which
    /// printed a plausible, wrong balance with no error anywhere.
    #[tokio::test]
    async fn a_balance_over_pre_existing_over_ceiling_rows_answers_instead_of_panicking() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&db).await.unwrap();

        let account = crate::accounts::create(
            &db,
            SaveAccount {
                name: "Legacy bank".to_string(),
                kind: AccountKind::Bank,
                institution: Some("ANZ".to_string()),
                currency_code: "NZD".to_string(),
                // Zero seeds no opening-balance row, so the only transactions are the two
                // hostile ones inserted below.
                opening_balance_minor: Some(0),
                opening_balance_date: Some("2020-01-01".to_string()),
                metadata: None,
                archived: false,
                sort_order: 0,
                ownership: Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id;

        // Straight into the table: no `SaveTransaction`, so no ceiling. This is the shape of a
        // row that predates the type.
        for posted_at in ["2026-01-05", "2026-01-06"] {
            let huge = i64::MAX;
            sqlx::query!(
                "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code, description)
                 VALUES (?1, ?2, ?3, 'NZD', 'legacy')",
                account,
                posted_at,
                huge
            )
            .execute(&db)
            .await
            .unwrap();
        }

        let store = Arc::new(SqliteStore::new(db.clone()));
        let reports = ReportService::new(store.clone(), store.clone(), Arc::new(SystemClock));

        let report = reports
            .balances(&ReportQuery::default())
            .await
            .expect("a balance sheet must still answer over unbounded legacy rows");

        let row = report
            .accounts
            .iter()
            .find(|a| a.account_id == account)
            .expect("the account is still listed");
        assert_eq!(
            row.value_minor,
            i64::MAX,
            "the balance saturates at the i64 ceiling — obviously wrong on screen, with a WARN \
             naming the account — rather than wrapping to a plausible small negative"
        );
    }
}
