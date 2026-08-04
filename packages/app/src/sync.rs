//! Provider sync orchestration: fetch upstream transactions for one provider, import the
//! new ones (dedupe on external id), and durably record the outcome. Shared by the manual
//! sync route (`sure-api`'s `routes::providers::sync`) and the background
//! [`crate::tasks::provider_poll::ProviderPollTask`].

use std::sync::Arc;

use sure_core::{AppError, AppResult, Provider, ProviderSync, SyncOutcome};

use crate::ports::{
    AccountRepo, Clock, ImportRow, ProviderRegistry, ProviderRepo, SyncContext, ValuationRepo,
};

/// How much of a provider's error text [`sync_detail`] keeps. 500 chars is enough to name
/// the failing field and offset, which is all a diagnosis needs.
pub const MAX_SYNC_DETAIL_CHARS: usize = 500;

/// Bound a provider error's text before it is stored or shown.
///
/// A provider error is third-party text, and some of it has carried the upstream payload
/// with it: `akahu-client`'s `AkahuError::JsonDeserialization` used to interpolate the
/// *entire* response body into its `Display`, so a schema change on Akahu's side turned a
/// successful 200 holding a page of 100 real bank transactions — merchants, amounts,
/// descriptions, external account ids — into the error message. That crate stopped doing it
/// in 0.3 (the body now sits behind a `ResponseBody` newtype absent from `Display` and
/// redacted in `Debug`), which is a fix at *one* provider, not a reason to stop bounding
/// here: every other provider's error text is still someone else's `Display` to write, and
/// this is the one place both copies of it are made. Both places that message lands are
/// exposures: `provider_syncs.detail` is an unbounded `TEXT` column served back by
/// `GET /api/providers/{id}/syncs`, and [`AppError::validation`] is a 4xx, which
/// `sure-core`'s error mapping passes to the client verbatim (only 5xx is scrubbed). The
/// size is unbounded too — a 75 MB body would become a 75 MB row and a 75 MB 422 response
/// from a route whose *inbound* cap is 2 MiB.
///
/// So cap it here, at the one place both copies are made, rather than trusting every
/// provider's `Display` to be terse. Truncation is on a char boundary (the byte at
/// [`MAX_SYNC_DETAIL_CHARS`] may be mid-codepoint in a UTF-8 body, and slicing there
/// panics) and appends a marker so a reader knows the text is not the whole error.
pub fn sync_detail(e: &anyhow::Error) -> String {
    let text = e.to_string();
    let mut out: String = text.chars().take(MAX_SYNC_DETAIL_CHARS).collect();
    if out.len() < text.len() {
        out.push_str("… (truncated)");
    }
    out
}

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
                // One bounded rendering, used for both the durable row and the 422 — see
                // `sync_detail` for what an unbounded one would leak.
                let detail = sync_detail(&e);
                let _ = self
                    .providers
                    .record_sync(id, 0, 0, SyncOutcome::Error, Some(&detail))
                    .await?;
                return Err(AppError::validation(format!("sync failed: {detail}")));
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
                is_one_off: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Stands in for any provider error whose `Display` appends the payload it choked on —
    /// `akahu-client`'s deser error did exactly this before 0.3, and nothing stops the next
    /// adapter's from doing it again. Deliberately synthetic filler, not transaction-shaped
    /// text — this test is about the byte count, and a fixture never carries real data's
    /// identifiers.
    fn oversized_error() -> anyhow::Error {
        anyhow::anyhow!(
            "failed to deserialize response: {}",
            "SYNTHETIC-BODY-FILLER ".repeat(200)
        )
    }

    #[test]
    fn truncates_an_oversized_provider_error() {
        let detail = sync_detail(&oversized_error());
        assert!(detail.ends_with("… (truncated)"));
        // The marker is the only thing past the cap.
        assert_eq!(
            detail.chars().count(),
            MAX_SYNC_DETAIL_CHARS + "… (truncated)".chars().count()
        );
        assert!(detail.starts_with("failed to deserialize response: SYNTHETIC-BODY-FILLER"));
    }

    #[test]
    fn leaves_a_short_provider_error_alone() {
        let detail = sync_detail(&anyhow::anyhow!("upstream timed out"));
        assert_eq!(detail, "upstream timed out");
    }

    /// A UTF-8 body can put a multi-byte codepoint exactly astride the cap; slicing the
    /// `String` by bytes there panics, so the cut has to be on a char boundary.
    #[test]
    fn cuts_multibyte_text_on_a_char_boundary() {
        // Every char here is 3 bytes, so a byte-slice at MAX_SYNC_DETAIL_CHARS lands
        // mid-codepoint.
        let body: String = "ゑ".repeat(MAX_SYNC_DETAIL_CHARS + 10);
        let detail = sync_detail(&anyhow::anyhow!("{body}"));
        assert!(detail.ends_with("… (truncated)"));
        assert_eq!(
            detail.chars().filter(|c| *c == 'ゑ').count(),
            MAX_SYNC_DETAIL_CHARS
        );
    }

    /// The boundary itself: text of exactly the cap is whole, one char more is cut.
    #[test]
    fn marks_only_when_it_actually_cut() {
        let exact: String = "x".repeat(MAX_SYNC_DETAIL_CHARS);
        assert_eq!(sync_detail(&anyhow::anyhow!("{exact}")), exact);

        let one_over: String = "x".repeat(MAX_SYNC_DETAIL_CHARS + 1);
        assert_eq!(
            sync_detail(&anyhow::anyhow!("{one_over}")),
            format!("{exact}… (truncated)")
        );
    }
}
