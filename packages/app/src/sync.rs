//! Provider sync orchestration: fetch upstream transactions for one provider, import the
//! new ones (dedupe on external id), and durably record the outcome. Shared by the manual
//! sync route (`sure-api`'s `routes::providers::sync`) and the background
//! [`crate::tasks::provider_poll::ProviderPollTask`].

use std::sync::Arc;

use sure_core::{AppError, AppResult, Provider, ProviderSync, SyncOutcome};

use crate::ports::{
    AccountRepo, Clock, ImportRow, ProviderRegistry, ProviderRepo, SyncContext, ValuationRepo,
};

pub struct SyncService {
    providers: Arc<dyn ProviderRepo>,
    accounts: Arc<dyn AccountRepo>,
    valuations: Arc<dyn ValuationRepo>,
    registry: Arc<dyn ProviderRegistry>,
    clock: Arc<dyn Clock>,
}

impl SyncService {
    pub fn new(
        providers: Arc<dyn ProviderRepo>,
        accounts: Arc<dyn AccountRepo>,
        valuations: Arc<dyn ValuationRepo>,
        registry: Arc<dyn ProviderRegistry>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            providers,
            accounts,
            valuations,
            registry,
            clock,
        }
    }

    /// Fetch upstream transactions for one provider, import the new ones (dedupe on
    /// external id), and durably record the outcome. A fetch failure is still recorded
    /// (as an "error" sync row) before the error is propagated.
    pub async fn sync_provider(
        &self,
        provider: Provider,
        payload: Option<&str>,
    ) -> AppResult<ProviderSync> {
        let id = provider.id;
        let account_ccy = self.providers.account_currency(provider.account_id).await?;

        let p = self.registry.get(&provider.kind).ok_or_else(|| {
            AppError::validation(format!("unknown provider kind '{}'", provider.kind))
        })?;
        let ctx = SyncContext {
            config: &provider.config,
            account_currency: &account_ccy,
            payload,
            last_synced_at: provider.last_synced_at.as_deref(),
        };

        let fetched = match p.fetch(ctx).await {
            Ok(txns) => txns,
            Err(e) => {
                let _ = self
                    .providers
                    .record_sync(id, 0, 0, SyncOutcome::Error, Some(&e.to_string()))
                    .await?;
                return Err(AppError::validation(format!("sync failed: {e}")));
            }
        };

        let provider_tag = format!("{}#{}", provider.kind, id);
        let rows: Vec<ImportRow> = fetched
            .into_iter()
            .map(|t| ImportRow {
                external_id: t.external_id,
                posted_at: t.posted_at,
                amount_minor: t.amount_minor,
                currency_code: t.currency_code,
                description: t.description,
                merchant: t.merchant,
                category_name: t.category.as_ref().map(|c| c.name.clone()),
                category_kind: t.category.as_ref().and_then(|c| c.kind),
                category_group: t.category.and_then(|c| c.group),
            })
            .collect();

        let (imported, skipped) = self
            .providers
            .import_transactions(provider.account_id, &account_ccy, &provider_tag, &rows)
            .await?;

        // Best-effort: a provider's transaction history often doesn't reach back to when
        // the account was opened (a mortgage's full term, say), so the imported
        // transactions alone would leave the displayed balance drifting from reality.
        // Refresh a same-day provider-sourced valuation from the upstream's live balance
        // where the provider can report one, so the balance stays accurate regardless of
        // transaction completeness. Never lets a balance-fetch problem fail the sync —
        // the transaction import already succeeded, which is the part
        // `status`/`imported`/`skipped` below describe.
        match p.current_balance(ctx).await {
            Ok(Some(bal)) => {
                // A brokerage account's value is computed from its own holdings + wallet
                // ledger (source='brokerage'); a provider's balance-only figure must not
                // compete with it on the net-worth line — Akahu can't sync Sharesies
                // transactions and often reports the balance as $0, which would otherwise
                // clobber the real computed value on a same-day tie. Skip the provider
                // valuation for that kind (the limit/principal/institution backfills below
                // are no-ops for a brokerage account anyway).
                let is_brokerage = self
                    .accounts
                    .get(provider.account_id)
                    .await
                    .map(|a| a.kind == sure_core::AccountKind::Brokerage)
                    .unwrap_or(false);
                let today = self.clock.today().to_string();
                if !is_brokerage {
                    if let Err(e) = self
                        .valuations
                        .upsert_from_provider(
                            provider.account_id,
                            &today,
                            bal.minor,
                            &bal.currency_code,
                        )
                        .await
                    {
                        tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider balance valuation");
                    }
                }
                // Also best-effort: lets a credit_card/revolving_credit account show
                // "remaining borrowing" (the web UI computes limit minus what's owed). A
                // no-op for any account kind with no such concept.
                if let Some(limit_minor) = bal.limit_minor {
                    if let Err(e) = self
                        .accounts
                        .set_credit_limit(provider.account_id, limit_minor)
                        .await
                    {
                        tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider credit limit");
                    }
                }
                // Same idea for a mortgage/loan's original borrowed amount, so the web UI
                // can show how much of it has been paid down. A no-op for any other kind.
                if let Some(initial_principal_minor) = bal.initial_principal_minor {
                    if let Err(e) = self
                        .accounts
                        .set_original_amount(provider.account_id, initial_principal_minor)
                        .await
                    {
                        tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider original loan amount");
                    }
                }
                // Backfill the institution name too, but only if unset — see
                // `set_institution_if_unset`'s doc comment for why this one never
                // overwrites.
                if let Some(institution) = bal.institution {
                    if let Err(e) = self
                        .accounts
                        .set_institution_if_unset(provider.account_id, &institution)
                        .await
                    {
                        tracing::warn!(account_id = provider.account_id, error = %e, "could not record provider institution");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(provider_id = id, error = %e, "could not fetch provider balance");
            }
        }

        self.providers.update_last_synced(id).await?;
        self.providers
            .record_sync(id, imported, skipped, SyncOutcome::Ok, None)
            .await
    }
}
