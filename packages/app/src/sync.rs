//! Provider sync orchestration: fetch upstream transactions for one provider, import the
//! new ones (dedupe on external id), and durably record the outcome. Shared by the manual
//! sync route (`sure-api`'s `routes::providers::sync`) and the background
//! [`crate::tasks::provider_poll::ProviderPollTask`].

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use sure_core::{AppError, AppResult, Provider, ProviderSync, SyncOutcome};

use crate::ports::{
    AccountRepo, Clock, ImportRow, ProviderRegistry, ProviderRepo, SyncContext, ValuationRepo,
};

/// How much of a provider's error text [`sync_detail`] keeps.
///
/// Aliased rather than restated: `AppError::upstream` bounds the same kind of text on its way
/// into a log, and the two used to be independent 500s that nothing tied together. The name
/// stays because this crate's callers and tests are about the sync path specifically; the
/// number, and the reasoning for it, live in one place.
pub const MAX_SYNC_DETAIL_CHARS: usize = sure_core::error::MAX_UPSTREAM_DETAIL_CHARS;

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
/// provider's `Display` to be terse. The bounding itself is
/// [`sure_core::error::truncate_for_wire`], shared with the request-body extractor, which has
/// the same problem from the other direction: `serde`'s error quotes the offending value out
/// of a body nothing has bounded.
pub fn sync_detail(e: &anyhow::Error) -> String {
    sure_core::error::truncate_for_wire(&e.to_string(), MAX_SYNC_DETAIL_CHARS)
}

/// Holds one provider's single-flight slot for the duration of a sync and gives it back on
/// `Drop` — on *every* exit path: the early `return Err` a failed fetch takes, the `?` on
/// any repo call, and a panic inside a provider adapter (the unwind drops locals). A plain
/// `remove` at the end of [`SyncService::sync_provider`] would leak the id on all three, and
/// a single failed sync would then wedge that provider as "already syncing" until the
/// process restarted.
struct SyncSlot<'a> {
    provider_id: i64,
    in_flight: &'a Mutex<HashSet<i64>>,
}

impl<'a> SyncSlot<'a> {
    /// Claim `provider_id`, or `None` when a sync of that provider is already running.
    fn claim(in_flight: &'a Mutex<HashSet<i64>>, provider_id: i64) -> Option<Self> {
        let mut ids = in_flight.lock().unwrap_or_else(PoisonError::into_inner);
        ids.insert(provider_id).then(|| Self {
            provider_id,
            in_flight,
        })
    }
}

impl Drop for SyncSlot<'_> {
    fn drop(&mut self) {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.provider_id);
    }
}

pub struct SyncService {
    providers: Arc<dyn ProviderRepo>,
    accounts: Arc<dyn AccountRepo>,
    valuations: Arc<dyn ValuationRepo>,
    registry: Arc<dyn ProviderRegistry>,
    clock: Arc<dyn Clock>,
    /// Provider ids with a sync in flight right now — the single-flight set
    /// [`Self::sync_provider`] claims from. Deliberately a `std::sync::Mutex`: every
    /// critical section is one `HashSet` insert or remove with no `.await` inside, so an
    /// async mutex would buy nothing and would make releasing the slot from [`SyncSlot`]'s
    /// `Drop` (the part that has to be infallible) impossible.
    ///
    /// The set lives on the *service*, not on the HTTP route, so it covers every caller of
    /// `sync_provider`: the manual `POST /api/providers/{id}/sync`, the initial sync after
    /// linking, and `tasks::provider_poll`'s 6-hourly batch. The scheduler firing while a
    /// human is watching a manual sync is exactly the collision worth preventing — two runs
    /// of one provider mean double the outbound calls (upstream rate limits and bans are
    /// per-household), two write transactions contending for SQLite's single writer, and
    /// duplicate work everywhere in between.
    in_flight: Mutex<HashSet<i64>>,
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
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    /// Fetch upstream transactions for one provider, import the new ones (dedupe on
    /// external id), and durably record the outcome. A fetch failure is still recorded
    /// (as an "error" sync row) before the error is propagated.
    ///
    /// **Single-flight per provider.** A second concurrent sync of the *same* provider is
    /// refused outright with [`AppError::Conflict`] (a 409 at the HTTP edge) rather than
    /// started: it would double the outbound requests to an upstream that rate-limits per
    /// household, contend with the first run for SQLite's single writer while both hold the
    /// route's 300s deadline open, and re-do work whose only outcome is `skipped` counts.
    /// Different providers are unaffected and still sync concurrently. See
    /// [`Self::in_flight`] for why the guard sits here rather than on the route.
    pub async fn sync_provider(
        &self,
        provider: Provider,
        payload: Option<&str>,
    ) -> AppResult<ProviderSync> {
        let id = provider.id;
        // Claimed before any I/O — and released by `Drop`, so no exit path leaks it.
        let _slot = SyncSlot::claim(&self.in_flight, id).ok_or_else(|| {
            AppError::conflict(format!(
                "a sync of provider {id} is already running — refusing to start a second one"
            ))
        })?;
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

        // Anything the balance handling below made us refuse, carried onto the sync row so
        // the refusal is visible in `GET /api/providers/{id}/syncs` and in the sync-now
        // response instead of living only in a server log line nobody reads.
        let mut refused: Option<String> = None;

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
                // Every minor-unit figure on a `ProviderBalance` is denominated in the
                // upstream's currency, and nothing downstream re-checks: `valuations` rows
                // are read as being in the account's currency, and `credit_limit_minor` /
                // `original_amount_minor` are rendered next to the account's own balance.
                // So a feed reporting in another currency — a wallet re-denominated upstream,
                // a multi-currency card, a plain mapping mistake at the adapter — would store
                // a number wrong by an exchange rate with no signal at all. Refuse the
                // amount-carrying writes instead, the same way `brokerage::revalue` refuses
                // to persist a total it could not convert: a day left unvalued is
                // recoverable, a silently wrong stored figure is not. Converting here is
                // deliberately *not* the fix — the account's currency is the account's, and a
                // feed that changed currency is a link to re-point, not a rate to apply.
                let currency_matches = bal.currency_code.eq_ignore_ascii_case(&account_ccy);
                if !currency_matches {
                    tracing::warn!(
                        provider_id = id,
                        account_id = provider.account_id,
                        provider_currency = %bal.currency_code,
                        account_currency = %account_ccy,
                        "provider reported a balance in a different currency; refused to record it"
                    );
                    refused = Some(format!(
                        "provider reported a balance in {} but the account is {} — refused to \
                         record the balance, credit limit and original amount, which would \
                         otherwise be stored as if they were {}",
                        bal.currency_code, account_ccy, account_ccy,
                    ));
                }

                // Only the currency-free backfill (institution, below) survives a mismatch;
                // every write in here carries a minor-unit amount.
                if currency_matches {
                    // A brokerage account's value is computed from its own holdings + wallet
                    // ledger (source='brokerage'); a provider's balance-only figure must not
                    // compete with it on the net-worth line — Akahu can't sync Sharesies
                    // transactions and often reports the balance as $0, which would otherwise
                    // clobber the real computed value on a same-day tie. Skip the provider
                    // valuation for that kind (the limit/principal/institution backfills are
                    // no-ops for a brokerage account anyway).
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
                }
                // Backfill the institution name too, but only if unset — see
                // `set_institution_if_unset`'s doc comment for why this one never
                // overwrites. Carries no amount, so a currency mismatch doesn't taint it.
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
        // Still `Ok`: the transaction import that `imported`/`skipped` describe did succeed,
        // and a currency-mismatched balance is not a failed sync. `detail` is what makes the
        // refusal visible without overstating it.
        self.providers
            .record_sync(id, imported, skipped, SyncOutcome::Ok, refused.as_deref())
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

    // ---- balance currency + single-flight ------------------------------------------
    //
    // Fakes for the four ports `SyncService` needs. Every fixture below is synthetic —
    // "Fake Bank", account ids in the 100s — because a fixture carries real data's shape,
    // never its identifiers.

    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use chrono::NaiveDate;
    use tokio::sync::{Barrier, Semaphore};

    use crate::ports::{
        AccountRepo, ProviderBalance, ProviderRegistry, ProviderRepo, ProviderTransaction,
        SharesTicker, TransactionProvider, ValuationRepo,
    };
    use crate::test_clock::FixedClock;
    use sure_core::{
        Account, AccountClass, AccountKind, AccountMetadata, DepositoryMeta, LinkProviderAccount,
        LinkProviderGroup, NewValuation, Ownership, ProviderKind, SaveAccount, SaveProvider,
        Valuation,
    };

    const TODAY: &str = "2026-07-01";

    fn today() -> NaiveDate {
        NaiveDate::parse_from_str(TODAY, "%Y-%m-%d").expect("fixture date parses")
    }

    fn provider_row(id: i64) -> Provider {
        Provider {
            id,
            name: format!("Fake Bank feed {id}"),
            kind: "fake".to_string(),
            account_id: 100 + id,
            config: serde_json::json!({}),
            enabled: true,
            last_synced_at: None,
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        }
    }

    /// One `record_sync` call, as durably stored — where the currency refusal has to surface.
    #[derive(Debug, Clone)]
    struct RecordedSync {
        provider_id: i64,
        status: SyncOutcome,
        detail: Option<String>,
    }

    struct FakeProviders {
        account_currency: String,
        recorded: Mutex<Vec<RecordedSync>>,
    }

    #[async_trait]
    impl ProviderRepo for FakeProviders {
        async fn list(&self) -> AppResult<Vec<Provider>> {
            unreachable!("SyncService never lists providers")
        }
        async fn get(&self, _id: i64) -> AppResult<Provider> {
            unreachable!("SyncService never re-reads a provider")
        }
        async fn create(&self, _input: SaveProvider) -> AppResult<Provider> {
            unreachable!("SyncService never creates a provider")
        }
        async fn update(&self, _id: i64, _input: SaveProvider) -> AppResult<Provider> {
            unreachable!("SyncService never updates a provider")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("SyncService never deletes a provider")
        }
        async fn link(&self, _input: LinkProviderAccount) -> AppResult<Provider> {
            unreachable!("SyncService never links")
        }
        async fn link_group(&self, _input: LinkProviderGroup) -> AppResult<Vec<Provider>> {
            unreachable!("SyncService never links")
        }
        async fn list_syncs(&self, _provider_id: i64) -> AppResult<Vec<ProviderSync>> {
            unreachable!("SyncService never lists sync history")
        }
        async fn account_currency(&self, _account_id: i64) -> AppResult<String> {
            Ok(self.account_currency.clone())
        }
        async fn import_transactions(
            &self,
            _account_id: i64,
            _account_currency: &str,
            _provider_tag: &str,
            rows: &[ImportRow],
        ) -> AppResult<(i64, i64)> {
            Ok((rows.len() as i64, 0))
        }
        async fn update_last_synced(&self, _id: i64) -> AppResult<()> {
            Ok(())
        }
        async fn record_sync(
            &self,
            provider_id: i64,
            imported: i64,
            skipped: i64,
            status: SyncOutcome,
            detail: Option<&str>,
        ) -> AppResult<ProviderSync> {
            let detail = detail.map(str::to_string);
            self.recorded
                .lock()
                .expect("test mutex")
                .push(RecordedSync {
                    provider_id,
                    status,
                    detail: detail.clone(),
                });
            Ok(ProviderSync {
                id: 1,
                provider_id,
                imported,
                skipped,
                status,
                detail,
                created_at: format!("{TODAY}T00:00:00.000Z"),
            })
        }
    }

    #[derive(Default)]
    struct FakeAccounts {
        credit_limits: Mutex<Vec<(i64, i64)>>,
        original_amounts: Mutex<Vec<(i64, i64)>>,
        institutions: Mutex<Vec<(i64, String)>>,
    }

    #[async_trait]
    impl AccountRepo for FakeAccounts {
        async fn list(&self, _include_archived: bool) -> AppResult<Vec<Account>> {
            unreachable!("SyncService never lists accounts")
        }
        async fn get(&self, id: i64) -> AppResult<Account> {
            Ok(Account {
                id,
                name: "Everyday".to_string(),
                kind: AccountKind::Bank,
                class: AccountClass::Cash,
                currency_code: "NZD".to_string(),
                institution: None,
                metadata: AccountMetadata::Depository(DepositoryMeta::default()),
                archived: false,
                sort_order: 0,
                secured_by_account_id: None,
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
                updated_at: "2026-01-01T00:00:00.000Z".to_string(),
                ownership: Ownership::Joint,
            })
        }
        async fn create(&self, _input: SaveAccount) -> AppResult<Account> {
            unreachable!("SyncService never creates an account")
        }
        async fn update(&self, _id: i64, _input: SaveAccount) -> AppResult<Account> {
            unreachable!("SyncService never updates an account")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("SyncService never deletes an account")
        }
        async fn set_secured_by(&self, _id: i64, _target: Option<i64>) -> AppResult<Account> {
            unreachable!("SyncService never re-secures an account")
        }
        async fn set_ownership(&self, _id: i64, _ownership: Ownership) -> AppResult<Account> {
            unreachable!("SyncService never attributes accounts")
        }
        async fn set_ownership_bulk(&self, _ids: &[i64], _ownership: Ownership) -> AppResult<u64> {
            unreachable!("SyncService never attributes accounts")
        }
        async fn list_shares_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            unreachable!("SyncService never lists tickers")
        }
        async fn list_brokerage_tickers(&self) -> AppResult<Vec<SharesTicker>> {
            unreachable!("SyncService never lists tickers")
        }
        async fn set_credit_limit(
            &self,
            account_id: i64,
            credit_limit_minor: i64,
        ) -> AppResult<()> {
            self.credit_limits
                .lock()
                .expect("test mutex")
                .push((account_id, credit_limit_minor));
            Ok(())
        }
        async fn set_original_amount(
            &self,
            account_id: i64,
            original_amount_minor: i64,
        ) -> AppResult<()> {
            self.original_amounts
                .lock()
                .expect("test mutex")
                .push((account_id, original_amount_minor));
            Ok(())
        }
        async fn set_institution_if_unset(
            &self,
            account_id: i64,
            institution: &str,
        ) -> AppResult<()> {
            self.institutions
                .lock()
                .expect("test mutex")
                .push((account_id, institution.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeValuations {
        /// `(account_id, as_of, value_minor, currency)` for every provider-sourced write.
        rows: Mutex<Vec<(i64, String, i64, String)>>,
    }

    #[async_trait]
    impl ValuationRepo for FakeValuations {
        async fn list_for_account(&self, _account_id: i64) -> AppResult<Vec<Valuation>> {
            unreachable!("SyncService never lists valuations")
        }
        async fn create(&self, _account_id: i64, _input: NewValuation) -> AppResult<Valuation> {
            unreachable!("SyncService never creates a manual valuation")
        }
        async fn delete(&self, _id: i64) -> AppResult<()> {
            unreachable!("SyncService never deletes a valuation")
        }
        async fn upsert_from_brokerage(
            &self,
            _account_id: i64,
            _as_of: &str,
            _value_minor: i64,
            _ccy: &str,
        ) -> AppResult<()> {
            unreachable!("SyncService never writes a brokerage valuation")
        }
        async fn upsert_from_provider(
            &self,
            account_id: i64,
            as_of: &str,
            value_minor: i64,
            ccy: &str,
        ) -> AppResult<()> {
            self.rows.lock().expect("test mutex").push((
                account_id,
                as_of.to_string(),
                value_minor,
                ccy.to_string(),
            ));
            Ok(())
        }
    }

    /// How long a fake `fetch` stays inside the provider — the lever the single-flight tests
    /// pull to hold one sync open while a second one is attempted.
    enum Gate {
        /// Return immediately.
        Open,
        /// Fail the fetch, exercising `sync_provider`'s early `return Err` — the exit path a
        /// hand-written `remove` at the end of the function would leak the slot on.
        Fail,
        /// Hand out a permit on `entered` (so a test knows the fetch is in flight), then wait
        /// for one on `release`. Semaphore permits are sticky, so neither side can miss the
        /// other's signal however the runtime schedules them.
        Held {
            entered: Arc<Semaphore>,
            release: Arc<Semaphore>,
        },
        /// Wait until `n` fetches are inside at once. Proves two syncs genuinely overlap
        /// rather than one having quietly finished first — and if they can't overlap, the
        /// barrier never releases and the test's timeout fails it.
        Rendezvous(Arc<Barrier>),
    }

    struct FakeProvider {
        fetches: Arc<AtomicUsize>,
        balance: Option<ProviderBalance>,
        gate: Gate,
    }

    #[async_trait]
    impl TransactionProvider for FakeProvider {
        fn kind(&self) -> &'static str {
            "fake"
        }
        fn description(&self) -> &'static str {
            "Synthetic provider for SyncService tests"
        }
        async fn fetch(&self, _ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            match &self.gate {
                Gate::Open => {}
                Gate::Fail => anyhow::bail!("upstream refused the request"),
                Gate::Held { entered, release } => {
                    entered.add_permits(1);
                    release
                        .acquire()
                        .await
                        .expect("release semaphore is never closed")
                        .forget();
                }
                Gate::Rendezvous(barrier) => {
                    barrier.wait().await;
                }
            }
            Ok(Vec::new())
        }
        async fn current_balance(
            &self,
            _ctx: SyncContext<'_>,
        ) -> anyhow::Result<Option<ProviderBalance>> {
            Ok(self.balance.clone())
        }
    }

    struct FakeRegistry {
        provider: FakeProvider,
    }

    impl ProviderRegistry for FakeRegistry {
        fn get(&self, _kind: &str) -> Option<&dyn TransactionProvider> {
            Some(&self.provider)
        }
        fn kinds(&self) -> Vec<ProviderKind> {
            unreachable!("SyncService never enumerates kinds")
        }
    }

    struct Harness {
        service: Arc<SyncService>,
        providers: Arc<FakeProviders>,
        accounts: Arc<FakeAccounts>,
        valuations: Arc<FakeValuations>,
        fetches: Arc<AtomicUsize>,
    }

    impl Harness {
        fn new(account_ccy: &str, balance: Option<ProviderBalance>, gate: Gate) -> Self {
            let fetches = Arc::new(AtomicUsize::new(0));
            let providers = Arc::new(FakeProviders {
                account_currency: account_ccy.to_string(),
                recorded: Mutex::new(Vec::new()),
            });
            let accounts = Arc::new(FakeAccounts::default());
            let valuations = Arc::new(FakeValuations::default());
            let registry = Arc::new(FakeRegistry {
                provider: FakeProvider {
                    fetches: fetches.clone(),
                    balance,
                    gate,
                },
            });
            let service = Arc::new(SyncService::new(
                providers.clone(),
                accounts.clone(),
                valuations.clone(),
                registry,
                Arc::new(FixedClock(today())),
            ));
            Self {
                service,
                providers,
                accounts,
                valuations,
                fetches,
            }
        }

        fn recorded(&self) -> Vec<RecordedSync> {
            self.providers.recorded.lock().expect("test mutex").clone()
        }

        fn fetch_count(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }
    }

    /// A balance carrying every amount-bearing field a feed can report, so the refusal below
    /// is shown to cover all three and not just the valuation.
    fn balance(ccy: &str) -> ProviderBalance {
        ProviderBalance {
            minor: 12_345_67,
            currency_code: ccy.to_string(),
            limit_minor: Some(50_000_00),
            institution: Some("Fake Bank".to_string()),
            initial_principal_minor: Some(400_000_00),
        }
    }

    /// W-22: a provider balance quoted in another currency is wrong by an exchange rate the
    /// moment it lands in `valuations`, and nothing downstream can tell. It must not be
    /// written — and the refusal has to reach the sync row, or it is invisible.
    #[tokio::test]
    async fn refuses_a_balance_in_a_different_currency_and_records_why() {
        let h = Harness::new("NZD", Some(balance("AUD")), Gate::Open);

        let sync = h
            .service
            .sync_provider(provider_row(1), None)
            .await
            .expect("the transaction import still succeeds");

        assert!(
            h.valuations.rows.lock().expect("test mutex").is_empty(),
            "an AUD balance must not be stored against an NZD account"
        );
        assert!(
            h.accounts
                .credit_limits
                .lock()
                .expect("test mutex")
                .is_empty(),
            "the credit limit is an amount in the same wrong currency"
        );
        assert!(
            h.accounts
                .original_amounts
                .lock()
                .expect("test mutex")
                .is_empty(),
            "so is the original borrowed amount"
        );
        // The institution name carries no amount, so it is still backfilled.
        assert_eq!(
            *h.accounts.institutions.lock().expect("test mutex"),
            vec![(101, "Fake Bank".to_string())]
        );

        // Visible, not just logged: the row the UI reads carries the refusal, while the
        // status stays `ok` because the transaction import itself did succeed.
        let detail = sync
            .detail
            .expect("the refusal is recorded on the sync row");
        assert!(
            detail.contains("AUD"),
            "names the provider's currency: {detail}"
        );
        assert!(
            detail.contains("NZD"),
            "names the account's currency: {detail}"
        );
        assert_eq!(sync.status, SyncOutcome::Ok);
        let recorded = h.recorded();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].detail, Some(detail));
    }

    /// The other half of W-22: a matching currency is still written, with no refusal noise.
    #[tokio::test]
    async fn records_a_balance_in_the_accounts_own_currency() {
        let h = Harness::new("NZD", Some(balance("NZD")), Gate::Open);

        let sync = h
            .service
            .sync_provider(provider_row(1), None)
            .await
            .expect("sync succeeds");

        assert_eq!(
            *h.valuations.rows.lock().expect("test mutex"),
            vec![(101, TODAY.to_string(), 12_345_67, "NZD".to_string())]
        );
        assert_eq!(
            *h.accounts.credit_limits.lock().expect("test mutex"),
            vec![(101, 50_000_00)]
        );
        assert_eq!(
            *h.accounts.original_amounts.lock().expect("test mutex"),
            vec![(101, 400_000_00)]
        );
        assert_eq!(sync.detail, None);
        assert_eq!(sync.status, SyncOutcome::Ok);
    }

    /// Currency codes are stored uppercase but arrive from a third party, so the comparison
    /// is case-insensitive — refusing a feed that happens to say "nzd" would be a false
    /// alarm that costs a day's valuation.
    #[tokio::test]
    async fn compares_currency_codes_case_insensitively() {
        let h = Harness::new("NZD", Some(balance("nzd")), Gate::Open);

        let sync = h
            .service
            .sync_provider(provider_row(1), None)
            .await
            .expect("sync succeeds");

        assert_eq!(sync.detail, None);
        assert_eq!(h.valuations.rows.lock().expect("test mutex").len(), 1);
    }

    /// W-26: two concurrent syncs of one provider are N times the outbound load on an
    /// upstream that rate-limits per household, plus two write transactions fighting over
    /// SQLite's single writer for up to the route's 300s deadline. The second must be
    /// refused, and the provider must be fetched exactly once.
    #[tokio::test]
    async fn refuses_a_second_concurrent_sync_of_the_same_provider() {
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let h = Harness::new(
            "NZD",
            None,
            Gate::Held {
                entered: entered.clone(),
                release: release.clone(),
            },
        );

        let first = tokio::spawn({
            let service = h.service.clone();
            async move { service.sync_provider(provider_row(1), None).await }
        });

        // Wait until the first sync is genuinely inside the provider's `fetch`.
        entered
            .acquire()
            .await
            .expect("entered semaphore is never closed")
            .forget();

        let err = h
            .service
            .sync_provider(provider_row(1), None)
            .await
            .expect_err("a second concurrent sync of the same provider is refused");
        assert_eq!(err.code(), "conflict", "409, not a duplicate run: {err}");

        // Two permits: one to let the in-flight sync finish, one for the retry below.
        release.add_permits(2);
        first
            .await
            .expect("the first sync task does not panic")
            .expect("the first sync succeeds");
        assert_eq!(
            h.fetch_count(),
            1,
            "the refused request must not have called the upstream"
        );

        // The slot is released on completion, so a sequential retry is not blocked — a guard
        // that only ever inserted would wedge this provider until restart.
        h.service
            .sync_provider(provider_row(1), None)
            .await
            .expect("a later, non-overlapping sync of the same provider still runs");
        assert_eq!(h.fetch_count(), 2);
    }

    /// The guard is per provider, not global: one household's accounts are independent feeds,
    /// and serialising all of them would turn the 6-hourly poll into a queue. The barrier
    /// only releases if both syncs are inside `fetch` at the same moment.
    #[tokio::test]
    async fn syncs_different_providers_concurrently() {
        let h = Harness::new("NZD", None, Gate::Rendezvous(Arc::new(Barrier::new(2))));

        let both = async {
            tokio::join!(
                h.service.sync_provider(provider_row(1), None),
                h.service.sync_provider(provider_row(2), None),
            )
        };
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(5), both)
            .await
            .expect("two different providers must be able to sync at the same time");

        assert!(a.is_ok(), "provider 1: {a:?}");
        assert!(b.is_ok(), "provider 2: {b:?}");
        assert_eq!(h.fetch_count(), 2);
        assert_eq!(h.recorded().len(), 2);
    }

    /// The leak the `Drop` release exists to prevent: a failed fetch returns early, so a
    /// hand-written `remove` at the end of `sync_provider` would never run and that provider
    /// would answer 409 forever after one upstream outage.
    #[tokio::test]
    async fn releases_the_slot_when_the_fetch_fails() {
        let h = Harness::new("NZD", None, Gate::Fail);

        let first = h
            .service
            .sync_provider(provider_row(1), None)
            .await
            .expect_err("a failed fetch is an error");
        assert_eq!(first.code(), "validation");

        let second = h
            .service
            .sync_provider(provider_row(1), None)
            .await
            .expect_err("still failing, but reached the provider again");
        assert_eq!(
            second.code(),
            "validation",
            "not wedged as 'already syncing'"
        );
        assert_eq!(h.fetch_count(), 2);

        // Both attempts are durably recorded as errors, for the same provider.
        let recorded = h.recorded();
        assert_eq!(recorded.len(), 2);
        assert!(recorded
            .iter()
            .all(|r| r.status == SyncOutcome::Error && r.provider_id == 1));
    }
}
