//! `SqliteStore` implements every `sure_app::ports` repo trait by delegating to this
//! crate's existing per-entity modules — the SQL itself is untouched. Each method maps
//! this crate's row-shaped types into the port's plain equivalents (see
//! `sure_app::ports`'s module doc for why they're distinct types rather than shared
//! ones): `sure-app` cannot depend back on `sure-dal` (this crate depends on it to see
//! the port traits), so the two sides can't share a struct definition.

use std::collections::HashSet;

use async_trait::async_trait;
use sure_app::ports::{
    AccountCurrency, AccountRepo, ActiveAccount, AssetAccount, BrokerageRepo, CurrencyDecimals,
    ExchangeRateRepo, ExchangeRateRow, FxRatesRepo, HoldingRow, ImportRow, LedgerTx,
    LedgerValuation, PlannedApplication, ProviderRepo, ReportCategory, ReportRepo, RuleRepo,
    SecuredLiabilityAccount, SharesTicker, StockPriceCacheRepo, TransferRepo, TxCtx, ValuationRepo,
    WalletRow,
};
use sure_core::{Account, AppResult, Provider, ProviderSync, RunResult, StockPrice};

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
    async fn get(&self, id: i64) -> AppResult<Account> {
        crate::accounts::get(&self.db, id).await
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

    async fn set_institution_if_unset(&self, account_id: i64, institution: &str) -> AppResult<()> {
        crate::accounts::set_institution_if_unset(&self.db, account_id, institution).await
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

    async fn account_tickers(&self, account_id: i64) -> AppResult<Vec<(String, String)>> {
        crate::brokerage::account_tickers(&self.db, account_id).await
    }

    async fn earliest_activity_date(&self, account_id: i64) -> AppResult<Option<String>> {
        crate::brokerage::earliest_activity_date(&self.db, account_id).await
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
        Ok(crate::reports::exchange_rates(&self.db)
            .await?
            .into_iter()
            .map(|r| ExchangeRateRow {
                base_code: r.base_code,
                quote_code: r.quote_code,
                rate: r.rate,
            })
            .collect())
    }
}

#[async_trait]
impl RuleRepo for SqliteStore {
    async fn load_contexts(&self) -> AppResult<Vec<TxCtx>> {
        Ok(crate::rules::load_contexts(&self.db)
            .await?
            .into_iter()
            .map(|r| TxCtx {
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
                account_kind: r.account_kind,
            })
            .collect())
    }

    async fn persist_run(
        &self,
        rule_id: Option<i64>,
        kind: &str,
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
            })
            .collect())
    }

    async fn transactions(&self) -> AppResult<Vec<LedgerTx>> {
        Ok(crate::reports::transactions(&self.db)
            .await?
            .into_iter()
            .map(|t| LedgerTx {
                account_id: t.account_id,
                posted_at: t.posted_at,
                amount_minor: t.amount_minor,
            })
            .collect())
    }

    async fn valuations(&self) -> AppResult<Vec<LedgerValuation>> {
        Ok(crate::reports::valuations(&self.db)
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

    async fn spend_transactions(&self) -> AppResult<Vec<sure_app::ports::SpendTransaction>> {
        Ok(crate::reports::spend_transactions(&self.db)
            .await?
            .into_iter()
            .map(|t| sure_app::ports::SpendTransaction {
                posted_at: t.posted_at,
                amount_minor: t.amount_minor,
                currency_code: t.currency_code,
                category_id: t.category_id,
                is_one_off: t.is_one_off,
                linked_transaction_id: t.linked_transaction_id,
            })
            .collect())
    }

    async fn active_accounts(&self) -> AppResult<Vec<ActiveAccount>> {
        Ok(crate::reports::active_accounts(&self.db)
            .await?
            .into_iter()
            .map(|a| ActiveAccount {
                id: a.id,
                name: a.name,
                kind: a.kind,
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
                kind: l.kind,
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
                category_kind: r.category_kind.clone(),
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
        status: &str,
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
        rate: &str,
        as_of: &str,
    ) -> AppResult<()> {
        crate::exchange_rate_cache::upsert(&self.db, base_code, quote_code, rate, as_of)
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
