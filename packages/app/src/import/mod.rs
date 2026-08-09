//! One pipeline for every file import.
//!
//! Four importers grew separately — an ASB transaction export, a myIR student-loan workbook, a
//! Sharesies export zip, a hand-written CSV — and each brought its own endpoint, its own result
//! type, and its own idea of what an import owes the person doing it. Only one of them could
//! show you what it was about to do; only one could be undone.
//!
//! None of that was a difference between the *sources*. It was the order they were written in.
//! So the stages every import genuinely shares live here, once:
//!
//! ```text
//!   sniff → parse → route to an account → derive that account's cutover → hold back
//!         → reconcile → [dry run stops here] → write → report
//! ```
//!
//! and each source's real differences live behind [`ImportAdapter`] (how do I read a blob) and
//! in [`ImportSource`]'s own methods (which account kinds take me, what may hold me back). A
//! fifth importer implements the trait and appears in the registry; nothing here changes.
//!
//! **The preview is the commit.** `dry_run` branches at exactly one place — after everything
//! that could refuse or warn, before the only write — so a preview cannot describe an import
//! that wouldn't happen. Every count on screen is the count the commit produces.
//!
//! Two things deliberately *not* here. Transfer auto-linking: the counterparty is often
//! imported later in the same upload, so `crate::tasks::transfer_link` reconciles both sides
//! regardless of order. And spawning anything: work that outlives the response is returned as a
//! [`FollowUp`] for the transport to spawn through its `Shutdown` handle, because an untracked
//! task is one the shutdown drain cannot wait for.

pub mod routing;

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDate;
use sure_core::{
    Account, AppError, AppResult, BlockingFeed, CutoverRule, ImportBlock, ImportBlockReason,
    ImportExtra, ImportExtraKind, ImportItem, ImportMatch, ImportRecord, ImportResult,
    ImportSource, ImportUndoResult, Provider, Reconciliation,
};

use crate::ports::{
    AccountRepo, BrokerageRepo, ImportHistoryRepo, ImportRegistry, ImportRow, NewImport,
    ParsedExtras, ParsedItem, ParsedUpload, PersonRepo, ProviderRegistry, ProviderRepo,
    TransactionRepo,
};
use crate::reports::{ReportQuery, ReportService};
use crate::rules::AutoCategorize;

/// What one upload asked for.
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Parse, route, reconcile and report — but write nothing.
    pub dry_run: bool,
    /// Whether to also record the opening balance an export implies. On by default at the
    /// transport edge: without it a reconstructed history starts from nothing, because an
    /// account reads as 0 before its earliest transaction. Ignored by sources whose exports
    /// state no closing balance to work back from.
    pub opening_balance: bool,
    /// `<source account>:<account id>` pairs, or `<source account>:skip`. Overrides every other
    /// routing tier, and is how a UI commits exactly what its preview showed — including the
    /// decision to leave one thing in the upload alone.
    pub assign: Option<String>,
    /// `<source account>:<YYYY-MM-DD>` pairs: the date a feed that has never posted owns from,
    /// stated by the person importing. Consulted **only** where the derivation is blocked and
    /// the block is one a stated date can resolve — a feed that has posted still decides its own
    /// window (see [`routing::parse_cutovers`]).
    pub cutover: Option<String>,
    /// The source to read the blob as, when the caller knows better than the sniffer.
    pub source: Option<ImportSource>,
}

/// Work an import starts that outlives the response it returns. Returned rather than spawned:
/// only the transport holds the `Shutdown` handle that makes a task visible to the drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowUp {
    /// Rebuild an account's daily valuation history. One upstream price call per ticker and
    /// then a loop over every day since inception — too slow to hold a response open for, and
    /// idempotent, so a retry button is the repair path.
    BackfillValuations { account_id: i64 },
}

/// The result of an import, plus anything the transport should start afterwards.
pub struct Imported {
    pub result: ImportResult,
    pub follow_up: Option<FollowUp>,
}

pub struct ImportService {
    registry: Arc<dyn ImportRegistry>,
    accounts: Arc<dyn AccountRepo>,
    people: Arc<dyn PersonRepo>,
    transactions: Arc<dyn TransactionRepo>,
    providers: Arc<dyn ProviderRepo>,
    brokerage: Arc<dyn BrokerageRepo>,
    history: Arc<dyn ImportHistoryRepo>,
    provider_registry: Arc<dyn ProviderRegistry>,
    reports: Arc<ReportService>,
    /// Applies the rule set to what the upload just wrote — see the end of [`Self::commit`].
    categorize: Arc<dyn AutoCategorize>,
}

impl ImportService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        registry: Arc<dyn ImportRegistry>,
        accounts: Arc<dyn AccountRepo>,
        people: Arc<dyn PersonRepo>,
        transactions: Arc<dyn TransactionRepo>,
        providers: Arc<dyn ProviderRepo>,
        brokerage: Arc<dyn BrokerageRepo>,
        history: Arc<dyn ImportHistoryRepo>,
        provider_registry: Arc<dyn ProviderRegistry>,
        reports: Arc<ReportService>,
        categorize: Arc<dyn AutoCategorize>,
    ) -> Self {
        Self {
            registry,
            accounts,
            people,
            transactions,
            providers,
            brokerage,
            history,
            provider_registry,
            reports,
            categorize,
        }
    }

    /// What file imports have done, newest first — `account_id` narrows it to one account.
    pub async fn history(&self, account_id: Option<i64>) -> AppResult<Vec<ImportRecord>> {
        self.history.list(account_id).await
    }

    /// Recognise and read one upload: any number of files, already assembled into one blob (a
    /// bare export, or a zip of them).
    ///
    /// Synchronous, and separate from [`Self::commit`], because it is the only CPU-bound half:
    /// unzipping and parsing a few thousand rows has no business on an async runtime's worker
    /// threads, and the transport hands exactly this much to `spawn_blocking`. It also touches
    /// no database, which is what lets a preview and a commit share it.
    pub fn parse(&self, bytes: &[u8], opts: &ImportOptions) -> AppResult<ParsedUpload> {
        if bytes.len() > sure_core::MAX_UPLOAD_BYTES {
            return Err(AppError::validation(format!(
                "this upload is {:.1} MB, and the ceiling is {} MB — import fewer files at once",
                bytes.len() as f64 / (1024.0 * 1024.0),
                sure_core::MAX_UPLOAD_BYTES / (1024 * 1024)
            )));
        }
        let adapter = match opts.source {
            Some(source) => self.registry.get(source).ok_or_else(|| {
                AppError::validation(format!("'{}' is not an import source", source.as_str()))
            })?,
            None => self.registry.detect(bytes).ok_or_else(|| {
                AppError::validation(
                    "this doesn't look like any export Sure can read — a bank transaction CSV, a \
                     myIR .xlsx, a Sharesies export zip, or a zip of those. If it is one, say \
                     which with ?source=",
                )
            })?,
        };
        let source = adapter.source();
        adapter.parse(bytes).map_err(|e| {
            // Naming what it was read *as* is the difference between a message someone can act
            // on and "could not read export": the usual cause is a correct file read as the
            // wrong source, and the fix is to say which.
            AppError::validation(format!(
                "this was read as a {} but could not be understood: {e}. If that's the wrong \
                 source, say which with ?source=",
                source.label()
            ))
        })
    }

    /// Place, reconcile and (unless this is a dry run) write what [`Self::parse`] read.
    pub async fn commit(&self, upload: ParsedUpload, opts: &ImportOptions) -> AppResult<Imported> {
        let source = upload.source;
        let adapter = self.registry.get(source).ok_or_else(|| {
            AppError::validation(format!("'{}' is not an import source", source.as_str()))
        })?;

        let all = self.accounts.list(false).await?;
        let assigned = routing::parse_assignments(opts.assign.as_deref())?;
        // Checked against *every* account, before anything is read, so naming one that doesn't
        // exist or can't take this file is refused rather than quietly falling through to the
        // other tiers — which is what the per-account routes gave for free by having the account
        // in their path.
        routing::check_assignments(source, &assigned, &all)?;
        let accounts: Vec<Account> = all.into_iter().filter(|a| source.accepts(a.kind)).collect();
        let prior = routing::prior_imports(self.transactions.as_ref(), adapter).await?;
        // Only for the items the deterministic tiers leave unmatched, and decided across the
        // whole upload at once so two items can't claim one account — see `match_by_history`.
        let by_history = routing::match_by_history(
            self.transactions.as_ref(),
            &upload.items,
            &accounts,
            &assigned,
            &prior,
        )
        .await?;
        // The household, for the sources that name whose an export is (myIR does; nothing
        // else does yet). Two rows, read once per upload rather than per item.
        let people = self.people.list().await?;
        let sole = routing::only_candidate(source, &upload.items, &accounts, &people);
        let table = routing::Routing {
            assigned,
            prior,
            by_history,
            people,
        };

        let stated = routing::parse_cutovers(opts.cutover.as_deref())?;

        let mut items = Vec::new();
        let mut follow_up = None;
        for item in upload.items {
            // An explicit skip is answered here rather than by handing `resolve` a `None` target,
            // so the report says what happened. "Nothing matched this export" and "you told me
            // not to import this one" are different sentences, and the second one is not a
            // problem to be fixed.
            if table.assigned.get(&item.source_account) == Some(&routing::Assignment::Skip) {
                items.push(describe_skipped(&item));
                continue;
            }
            let target = routing::resolve(&item, &accounts, &table).or(sole);
            let (one, next) = self.import_one(source, item, target, opts, &stated).await?;
            follow_up = follow_up.or(next);
            items.push(one);
        }

        // Classify what the upload just put on the ledger, once for the whole upload rather
        // than per item: an ASB zip is a dozen accounts, and the pass considers the whole
        // uncategorised backlog whichever item triggers it. A dry run has written nothing, and
        // an upload whose every item was skipped or unrouted has nothing new to classify.
        if !opts.dry_run && items.iter().any(|i| i.imported > 0) {
            match self.categorize.categorize_new().await {
                Ok(Some(run)) => tracing::info!(
                    run_id = run.run_id,
                    changed = run.changed,
                    "categorized new transactions after import"
                ),
                Ok(None) => {}
                // Best-effort, exactly as in `SyncService::categorize_imported`: the rows are
                // committed and reported by the time this runs, and an import the caller has
                // to repeat is a worse outcome than a category they can fill in later.
                Err(e) => {
                    tracing::warn!(error = %e, "could not categorize new transactions after import")
                }
            }
        }

        Ok(Imported {
            result: ImportResult {
                dry_run: opts.dry_run,
                source,
                items,
                warnings: upload.warnings,
            },
            follow_up,
        })
    }

    /// Everything that happens to one item once its target account is known: derive that
    /// account's cutover, hold back what a live feed already owns, reconcile against the stated
    /// closing balance, and — unless this is a dry run — write.
    ///
    /// `target` is `None` when nothing identified an account. Then the item is described and
    /// skipped: reporting "we don't know where this goes" is the only honest option, since
    /// putting a savings account's history into a chequing account is not a recoverable
    /// mistake.
    ///
    /// **Every refusal in here is this item's**, never the upload's. An unresolved cutover, a
    /// wrong account, an unreadable ledger date: each is a fact about one target account, and an
    /// upload is routinely a zip of a household's exports bound for a dozen of them. Returning
    /// `Err` for one of these threw away every other item's import to report a problem with
    /// this one — and took the preview with it, so the "skip this one" control that would have
    /// resolved it never rendered. What survives as an `Err` is what genuinely stops the whole
    /// request: a database that won't answer.
    async fn import_one(
        &self,
        source: ImportSource,
        mut item: ParsedItem,
        target: Option<(&Account, ImportMatch)>,
        opts: &ImportOptions,
        stated_cutovers: &HashMap<String, NaiveDate>,
    ) -> AppResult<(ImportItem, Option<FollowUp>)> {
        let mut warnings = std::mem::take(&mut item.warnings);
        let Some((account, matched_by)) = target else {
            warnings.push(
                "no account was matched to this export, so it wasn't imported — choose one and \
                 import again"
                    .to_string(),
            );
            let rows_total = item.rows.len() as i64;
            return Ok((describe(&item, rows_total, None, None, warnings), None));
        };

        // Counted before anything is held back, so "rows in the file" means what it says.
        let rows_total = item.rows.len() as i64;
        let provider_tag = source.provider_tag(account.id);
        let stated = stated_cutovers.get(&item.source_account).copied();
        let cutover = match self
            .cutover_for(source, account, &provider_tag, stated)
            .await?
        {
            Cutover::Whole => None,
            Cutover::From(date) => {
                if let Some(stated) = stated.filter(|s| *s != date) {
                    // The derivation answered, so the account's own feeds decided the window and
                    // the stated date was not used. Silence here would leave someone believing a
                    // date they chose is holding rows back that it isn't.
                    warnings.push(format!(
                        "the cutover you gave ({stated}) was not used: {}'s own feeds already \
                         establish that they own everything from {date}, which is the date this \
                         import held back from",
                        account.name
                    ));
                }
                Some(date)
            }
            Cutover::Blocked(block) => {
                // This item only. Nothing of it is written, the reason travels with it, and the
                // rest of the upload carries on to its own accounts.
                let mut out =
                    describe(&item, rows_total, Some(account), Some(matched_by), warnings);
                out.would_import = 0;
                out.blocked = Some(block);
                return Ok((out, None));
            }
        };
        let held_back = hold_back(&mut item, cutover);
        if let (Some(cutover), true) = (cutover, held_back > 0) {
            // Two different claims, and they must not be confused. A derived cutover is a fact —
            // the feed has posted from there. A stated one is a prediction the person made, and
            // the risk it carries is theirs to see.
            warnings.push(if stated == Some(cutover) {
                format!(
                    "{held_back} row(s) from {cutover} onward were held back, because that is the \
                     date you said the feed owns from. If it turns out to post earlier than that, \
                     the overlap will be counted twice"
                )
            } else {
                format!(
                    "{held_back} row(s) from {cutover} onward were held back: a connected feed \
                     already covers that period, and importing them again would count the same \
                     money twice"
                )
            });
        }

        let account_balance_minor = self.latest_balance(account).await;
        // The strongest signal available that this export belongs to this account and reaches
        // all the way back: the stated closing balance against Sure's own. A warning, not a
        // refusal — a legitimate export can predate the latest valuation, and an account whose
        // history is only now being reconstructed genuinely holds less than the bank says.
        if let (Some(stated), Some(held)) = (item.stated_closing_minor, account_balance_minor) {
            if stated != held {
                warnings.push(format!(
                    "the export closes at {} but {} holds {} — check the export is for that \
                     account, and that its date range reaches back far enough; the two also \
                     differ while the account's own history is still incomplete",
                    major(stated),
                    account.name,
                    major(held)
                ));
            }
        }

        // The account's value before the item's first row. Withheld when something is already on
        // the ledger from before that date, because then this wouldn't be an opening balance —
        // it would be a large invented movement in the middle of existing history.
        let opening = match (opts.opening_balance, item.opening_balance.clone()) {
            (true, Some(row)) => {
                let existing = self.transactions.earliest_posted_at(account.id).await?;
                let clear = existing
                    .as_deref()
                    .is_none_or(|at| at >= row.posted_at.as_str());
                if !clear {
                    warnings.push(format!(
                        "{} already has transactions from before {}, so the opening balance of {} \
                         the export implies was not recorded — it would land in the middle of the \
                         ledger rather than ahead of it",
                        account.name,
                        &row.posted_at[..10],
                        major(row.amount_minor)
                    ));
                }
                clear.then_some(row)
            }
            // Nothing to work back from, or the caller opted out.
            (true, None) | (false, _) => None,
        };

        let mut out = describe(
            &item,
            rows_total,
            Some(account),
            Some(matched_by),
            std::mem::take(&mut warnings),
        );
        out.held_back = held_back;
        out.cutover = cutover.map(|d| d.to_string());
        if let Some(rec) = out.reconciliation.as_mut() {
            rec.account_balance_minor = account_balance_minor;
            rec.opening_balance_minor = opening.as_ref().map(|r| r.amount_minor);
            rec.opening_balance_as_of = opening.as_ref().map(|r| r.posted_at[..10].to_string());
        }
        // Counted with the rows it goes in alongside, so the preview's figure is the number of
        // rows the commit actually writes.
        out.would_import += i64::from(opening.is_some());
        if opts.dry_run {
            return Ok((out, None));
        }

        let mut rows = std::mem::take(&mut item.rows);
        rows.extend(opening);
        let follow_up = self
            .write(account, &provider_tag, &rows, &item.extras, &mut out)
            .await?;

        // Recorded after the write, so the log describes what landed rather than what was
        // attempted, and only for a routed commit — a dry run and an unplaced item leave nothing
        // behind for a log entry to be about.
        self.history
            .record(NewImport {
                account_id: account.id,
                source,
                provider_tag: provider_tag.clone(),
                source_account: Some(out.source_account.clone()),
                filenames: out.sources.clone(),
                imported: out.imported,
                skipped: out.skipped,
                held_back: out.held_back,
                covered_from: out.covered_from.clone(),
                covered_to: out.covered_to.clone(),
                cutover: out.cutover.clone(),
            })
            .await?;

        // Does the account's ledger now land on the balance it is recorded at? An opening
        // balance is worked back from what the *export* says, so it is only exact if Sure holds
        // the same movements the source does for the period it covers. Where a live feed owns
        // part of that window and its rows disagree, the difference shows up here and nowhere
        // else.
        if out.reconciliation.is_some() {
            let ledger_sum = self.transactions.sum_amount_minor(account.id).await?;
            if let Some(rec) = out.reconciliation.as_mut() {
                rec.ledger_sum_minor = Some(ledger_sum);
            }
            // Re-read rather than reuse the figure from before the insert. For an account whose
            // balance *is* its transaction sum — every kind these sources accept, unless a feed
            // has left a valuation — that earlier figure is now stale by exactly what was
            // imported, so comparing against it would warn on every successful import.
            // Re-read, and the check still bites wherever there is a balance recorded
            // independently of these rows to reconcile against, and is silent where there isn't.
            if let Some(balance) = self.latest_balance(account).await {
                if ledger_sum != balance {
                    out.warnings.push(format!(
                        "{}'s transactions now sum to {} but the account is recorded at {}, a \
                         difference of {} — some period is either counted twice or missing, so \
                         the reconstructed history before today will be out by that much",
                        account.name,
                        major(ledger_sum),
                        major(balance),
                        major(balance - ledger_sum),
                    ));
                }
            }
        }

        Ok((out, follow_up))
    }

    /// The one write. Matched exhaustively on what the source produced, so a fifth source that
    /// brings a new kind of record is a compile error here rather than rows silently dropped.
    async fn write(
        &self,
        account: &Account,
        provider_tag: &str,
        rows: &[ImportRow],
        extras: &ParsedExtras,
        out: &mut ImportItem,
    ) -> AppResult<Option<FollowUp>> {
        match extras {
            ParsedExtras::None => {
                let (imported, skipped) = self
                    .providers
                    .import_transactions(account.id, &account.currency_code, provider_tag, rows)
                    .await?;
                out.imported = imported;
                out.skipped = skipped;
                Ok(None)
            }
            ParsedExtras::Brokerage {
                holdings,
                dividends,
            } => {
                // One call, not three: the brokerage repo writes the wallet rows itself and puts
                // the lots and dividends in a single transaction, which is the atomicity a
                // holdings ledger needs.
                let counts = self
                    .brokerage
                    .import_export(
                        account.id,
                        &account.currency_code,
                        provider_tag,
                        rows,
                        holdings,
                        dividends,
                    )
                    .await?;
                out.imported = counts.transactions_imported;
                out.skipped = counts.transactions_skipped;
                out.extras = vec![
                    ImportExtra {
                        kind: ImportExtraKind::Holdings,
                        imported: counts.holdings_imported,
                        skipped: counts.holdings_skipped,
                    },
                    ImportExtra {
                        kind: ImportExtraKind::Dividends,
                        imported: counts.dividends_imported,
                        skipped: counts.dividends_skipped,
                    },
                ];
                Ok(Some(FollowUp::BackfillValuations {
                    account_id: account.id,
                }))
            }
        }
    }

    /// Remove one source's import from one account, leaving every other source untouched.
    ///
    /// Source-wide rather than per-import, and that is not a shortcut: two overlapping uploads
    /// of the same window share their content-derived ids, so the second one's rows were
    /// *skipped* rather than written, and there is nothing about it left to take back. "Undo
    /// the import I did on Tuesday" has no answer; "remove what myIR put here" does.
    pub async fn undo(&self, account_id: i64, source: ImportSource) -> AppResult<ImportUndoResult> {
        // Confirms the account exists, so undoing against a bad id is a 404 rather than a
        // cheerful "deleted 0".
        self.accounts.get(account_id).await?;
        let provider_tag = source.provider_tag(account_id);
        let deleted = self
            .transactions
            .delete_by_provider(account_id, &provider_tag)
            .await?;

        let mut extras = Vec::new();
        let mut warnings = Vec::new();
        // Matched on the rule rather than the source so the question asked is the one that
        // matters: did this source write anything besides transactions?
        match source {
            ImportSource::SharesiesZip => {
                let holdings = self
                    .brokerage
                    .delete_holdings_by_provider(account_id, &provider_tag)
                    .await?;
                let dividends = self
                    .brokerage
                    .delete_dividends_by_provider(account_id, &provider_tag)
                    .await?;
                extras.push(ImportExtra {
                    kind: ImportExtraKind::Holdings,
                    imported: 0,
                    skipped: holdings,
                });
                extras.push(ImportExtra {
                    kind: ImportExtraKind::Dividends,
                    imported: 0,
                    skipped: dividends,
                });
                // The valuation series the import backfilled is derived from those holdings but
                // stored in its own right, and deleting it would take live valuations with it.
                // Say so, rather than leave the account looking untouched when it isn't.
                warnings.push(
                    "the daily valuation history this import backfilled is still there — use \
                     Revalue to bring the account's value back in line"
                        .to_string(),
                );
            }
            ImportSource::AsbCsv | ImportSource::MyirSls | ImportSource::CsvUpload => {}
        }
        Ok(ImportUndoResult {
            deleted,
            extras,
            warnings,
        })
    }

    /// The date from which another feed already owns this account's movements — everything from
    /// it is that feed's to post, and this import stops there.
    ///
    /// Reads the provider list and then the ledger, because neither alone can tell "nothing
    /// else posts here" from "we couldn't tell": see [`decide_cutover`], which makes the
    /// decision.
    ///
    /// `stated` is the date the caller supplied for this item, and it is consulted in exactly one
    /// situation: the derivation came back [`Cutover::Blocked`] for a reason a stated date can
    /// answer. That keeps [`CutoverRule`]'s "it is never a parameter" true where it has to be —
    /// an account whose feeds *do* establish a window keeps it, and a caller cannot widen their
    /// import by passing a later date.
    async fn cutover_for(
        &self,
        source: ImportSource,
        account: &Account,
        provider_tag: &str,
        stated: Option<NaiveDate>,
    ) -> AppResult<Cutover> {
        if source.cutover_rule() == CutoverRule::Never {
            return Ok(Cutover::Whole);
        }
        let providers = self.providers.list().await?;
        let earliest = self
            .transactions
            .earliest_posted_at_from_other_feed(account.id, provider_tag)
            .await?;
        let decided = decide_cutover(
            source,
            account.id,
            &account.name,
            earliest.as_deref(),
            &providers,
            |kind| self.provider_registry.get(kind).is_some(),
        );
        Ok(match (decided, stated) {
            (Cutover::Blocked(block), Some(date))
                if block.reason.resolvable_by_stating_cutover() =>
            {
                Cutover::From(date)
            }
            (decided, _) => decided,
        })
    }

    /// The account's current balance, if it has one, as the balances report derives it: its
    /// newest valuation on or before today, else the running sum of its transactions
    /// (`crate::reports::account_value_at`). Taken from the report service rather than
    /// re-derived here, so the figure an export is checked against is the one the account page
    /// shows.
    ///
    /// It has to be that derivation and not the newest valuation alone: every depository kind
    /// seeds its opening balance as a *transaction* (the DAL's `opening_balance_ledger`) and
    /// accumulates from its own transaction stream, so such an account has no valuation at all
    /// unless a provider sync wrote one — and reading valuations meant the reconciliation this
    /// feeds, the strongest wrong-account signal there is, never ran for the very kinds a bank
    /// export goes to.
    ///
    /// `None` — no comparison, no warning — in four cases, each of which has nothing to
    /// compare: a read failure (this only feeds a warning, so it must not fail the import); an
    /// archived account, which the balances report doesn't cover; a balance recorded in another
    /// currency, where minor units against the export's would be arithmetic on two different
    /// things; and a zero, which here is the absence of a balance rather than a balance of zero
    /// — an account nothing has been recorded on yet derives 0 from an empty ledger, and "the
    /// export closes at 3,412.09 but the account holds 0.00" on every first import would train
    /// the reader to ignore the one that matters.
    async fn latest_balance(&self, account: &Account) -> Option<i64> {
        let report = match self.reports.balances(&ReportQuery::default()).await {
            Ok(report) => report,
            Err(e) => {
                tracing::warn!(
                    account_id = account.id,
                    error = %e,
                    "import: could not read the account's balance"
                );
                return None;
            }
        };
        report
            .accounts
            .into_iter()
            .find(|a| a.account_id == account.id && a.currency_code == account.currency_code)
            .map(|a| a.value_minor)
            .filter(|value| *value != 0)
    }
}

/// Drop the rows a live feed already owns. Returns how many went.
///
/// Applied here rather than inside a parser because only this layer knows the target account,
/// and therefore the cutover — the same file routed to two accounts is held back differently.
fn hold_back(item: &mut ParsedItem, cutover: Option<NaiveDate>) -> i64 {
    let Some(cutover) = cutover else {
        return 0;
    };
    let before = item.rows.len();
    // `posted_at` is `YYYY-MM-DDT…`, so a lexical compare on the first ten characters is the
    // date compare, without re-parsing every row.
    let boundary = cutover.to_string();
    item.rows
        .retain(|r| r.posted_at.get(..10).unwrap_or(&r.posted_at) < boundary.as_str());
    (before - item.rows.len()) as i64
}

/// The parts of a result that don't depend on whether anything was written.
fn describe(
    item: &ParsedItem,
    rows_total: i64,
    account: Option<&Account>,
    matched_by: Option<ImportMatch>,
    warnings: Vec<String>,
) -> ImportItem {
    ImportItem {
        source_account: item.source_account.clone(),
        account_id: account.map(|a| a.id),
        account_name: account.map(|a| a.name.clone()),
        matched_by,
        sources: item.sources.clone(),
        label: item.label.clone(),
        covered_from: item.covered_from.clone(),
        covered_to: item.covered_to.clone(),
        rows_total,
        would_import: item.rows.len() as i64,
        imported: 0,
        skipped: 0,
        held_back: 0,
        cutover: None,
        // Present exactly when the export gave something to reconcile against — a stated
        // closing balance, or an opening one to check the arithmetic from. The rest of the
        // fields are filled in by the caller, which is what knows the account.
        reconciliation: (item.stated_closing_minor.is_some() || item.opening_balance.is_some())
            .then(|| Reconciliation {
                ledger_balance_minor: item.stated_closing_minor,
                account_balance_minor: None,
                implied_opening_minor: item.opening_balance.as_ref().map(|r| r.amount_minor),
                opening_balance_minor: None,
                opening_balance_as_of: None,
                ledger_sum_minor: None,
            }),
        blocked: None,
        extras: Vec::new(),
        warnings,
    }
}

/// One thing in the upload the caller said to leave alone — reported, because a preview that
/// silently omits a file cannot be checked against the files that were picked.
///
/// Not routed at all, so no account is named: whichever account this *would* have gone to is not
/// a fact about an import that isn't happening, and naming one would put a row in the table
/// implying otherwise.
fn describe_skipped(item: &ParsedItem) -> ImportItem {
    let rows_total = item.rows.len() as i64;
    let mut warnings = item.warnings.clone();
    // Said outright, so a caller that isn't the UI — which knows, having asked — can tell this
    // apart from an item nothing could place. The two have the same empty `account_id`.
    warnings.push(
        "you chose to skip this one, so nothing from it was imported and nothing about it \
         changed"
            .to_string(),
    );
    let mut out = describe(item, rows_total, None, None, warnings);
    out.would_import = 0;
    out
}

/// How much of one account another feed owns, as far as this import can tell.
///
/// Three answers, and the third is the one that had nowhere to go before: *we could not tell*.
/// It used to be an `Err` out of the service, which made one account's unresolved feed the whole
/// upload's failure — see [`ImportService::import_one`]. It is a value now, so the item carries
/// it and the upload continues.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cutover {
    /// Nothing else posts here. Import the file whole.
    Whole,
    /// Another feed owns everything from this date; the import stops there.
    From(NaiveDate),
    /// Neither could be established, and this source may not guess — see [`CutoverRule::Strict`].
    Blocked(ImportBlock),
}

/// The cutover decision, from what the ledger and the provider list say. Pure, so both ways of
/// *not* knowing stay covered by a unit test rather than by a live database.
///
/// `earliest_from_other_feed` is `MIN(posted_at)` over the rows some other feed wrote.
/// `supplies_transactions` answers whether a provider kind posts transactions at all (the
/// registry): a row whose kind is no longer registered has nothing pending to post.
///
/// A connection that derives transactions from a balance feed is consulted *first*, because it
/// states its own start date outright — a better answer than the ledger read, which can only
/// see what has already been derived.
///
/// Under [`CutoverRule::Strict`], neither way of failing may return [`Cutover::Whole`]. `Whole`
/// means "no other feed owns any of this account", which holds nothing back and imports the file
/// whole — and the only warning on this path fires on rows that *were* held back, so a failure to
/// establish the cutover would be entirely silent. Both are therefore [`Cutover::Blocked`], and
/// nothing of that item is written:
///
/// * A connected, enabled feed with no rows yet. A link whose first sync failed is deliberately
///   kept, so this is also the state for the seconds after linking and for as long as
///   credentials are wrong. Import seven years into that window and the feed's next poll lands
///   its own two on top of the same two: dedupe is `(provider, external_id)` and cannot see
///   across `asb#N` and `akahu#M`, so every transaction in the overlap exists twice,
///   permanently.
/// * A `posted_at` that won't parse. It is `MIN()` under SQLite's BINARY collation, where a
///   non-ISO date sorts ahead of every ISO one (`'0' < '2'`), so a single legacy row is exactly
///   the one that decides the window — and an unreadable date in the account's history is a
///   defect worth surfacing in its own right.
fn decide_cutover(
    source: ImportSource,
    account_id: i64,
    account_name: &str,
    earliest_from_other_feed: Option<&str>,
    providers: &[Provider],
    supplies_transactions: impl Fn(&str) -> bool,
) -> Cutover {
    let mine = |p: &&Provider| p.account_id == account_id && p.enabled;

    // A feed that only reports a balance says where its derived half begins. That statement
    // needs no rows to exist yet, which is what makes it the better answer.
    let derived = providers
        .iter()
        .filter(mine)
        .filter(|p| {
            p.config
                .get("derive_transactions_from_balance")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|p| {
            p.config
                .get("derive_from")
                .and_then(serde_json::Value::as_str)
                .and_then(|s| NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").ok())
        })
        .min();
    if let Some(date) = derived {
        return Cutover::From(date);
    }

    if let Some(at) = earliest_from_other_feed {
        // Stored as a full timestamp; the cutover is a whole day.
        return match NaiveDate::parse_from_str(at.get(..10).unwrap_or(at), "%Y-%m-%d") {
            Ok(date) => Cutover::From(date),
            Err(_) => Cutover::Blocked(ImportBlock {
                reason: ImportBlockReason::UnreadableLedgerDate,
                message: format!(
                    "{account_name}'s earliest transaction from another feed is dated '{at}', \
                     which is not a date this import can read, so it cannot tell which period \
                     that feed owns — correct that row's date, then import again"
                ),
                feeds: Vec::new(),
            }),
        };
    }

    let waiting: Vec<BlockingFeed> = providers
        .iter()
        .filter(mine)
        .filter(|p| supplies_transactions(&p.kind))
        .map(|p| BlockingFeed {
            provider_id: p.id,
            name: p.name.clone(),
        })
        .collect();
    if waiting.is_empty() {
        return Cutover::Whole;
    }
    match source.cutover_rule() {
        // Nothing is held back for this source at all; `cutover_for` returned before reaching
        // here, and reaching it anyway would mean the two disagreed.
        CutoverRule::Never | CutoverRule::Lenient => Cutover::Whole,
        CutoverRule::Strict => Cutover::Blocked(ImportBlock {
            reason: ImportBlockReason::UnsyncedFeed,
            message: format!(
                "{account_name} is connected to {}, which has not posted a transaction yet, so \
                 this import cannot tell which period belongs to it — importing now would count \
                 that period twice once it syncs. Sync it, disable it, or say which date it owns \
                 from; everything else in this upload is unaffected",
                waiting
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            feeds: waiting,
        }),
    }
}

/// Minor units as a plain decimal string, for a message a person reads.
fn major(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(name: &str, kind: &str, account_id: i64, enabled: bool) -> Provider {
        Provider {
            id: 1,
            name: name.to_string(),
            kind: kind.to_string(),
            account_id,
            config: serde_json::Value::Null,
            enabled,
            last_synced_at: None,
            created_at: "2026-01-01T00:00:00+00:00".to_string(),
            updated_at: "2026-01-01T00:00:00+00:00".to_string(),
        }
    }

    /// Every kind these tests name is one the registry knows, unless a test says otherwise.
    fn registered(_kind: &str) -> bool {
        true
    }

    fn decide(earliest: Option<&str>, providers: &[Provider]) -> Cutover {
        decide_cutover(
            ImportSource::AsbCsv,
            8,
            "Everyday",
            earliest,
            providers,
            registered,
        )
    }

    /// The date a decision landed on — `None` where nothing else posts to the account. Panics
    /// naming the block where the decision was blocked, so a test that regresses says which of
    /// the two it got.
    fn date_of(got: Cutover, why: &str) -> Option<NaiveDate> {
        match got {
            Cutover::Whole => None,
            Cutover::From(date) => Some(date),
            Cutover::Blocked(block) => panic!("{why}, but it was blocked: {}", block.message),
        }
    }

    /// The block a decision produced, for the two states that have no answer.
    fn refused(earliest: Option<&str>, providers: &[Provider]) -> ImportBlock {
        match decide(earliest, providers) {
            Cutover::Blocked(block) => block,
            // Not an `Err` any more, and that is the point: it is one item's problem, carried on
            // that item, while the rest of the upload imports.
            other @ (Cutover::Whole | Cutover::From(_)) => {
                panic!("the item should have been blocked, got {other:?}")
            }
        }
    }

    #[test]
    fn a_timestamp_from_another_feed_becomes_a_whole_day_cutover() {
        let got = date_of(
            decide(Some("2022-01-01T12:00:00+00:00"), &[]),
            "a parseable date is an answer",
        );
        assert_eq!(got, NaiveDate::from_ymd_opt(2022, 1, 1));
    }

    #[test]
    fn an_account_nothing_else_posts_to_has_no_cutover() {
        let got = date_of(decide(None, &[]), "no other feed is a legitimate answer");
        assert_eq!(got, None);
    }

    #[test]
    fn an_enabled_feed_that_has_posted_nothing_yet_blocks_the_item() {
        // The state after a link whose first sync failed (the link is kept deliberately), and
        // the one where importing the file whole doubles every row that feed later posts for
        // the same period.
        let block = refused(None, &[provider("Akahu", "akahu", 8, true)]);
        assert_eq!(block.reason, ImportBlockReason::UnsyncedFeed);
        let msg = &block.message;
        assert!(msg.contains("Akahu"), "the feed to sync is named: {msg}");
        assert!(msg.contains("Everyday"), "the account is named: {msg}");
    }

    /// The ids are what let a UI offer to sync or disable the feed in place. Named alone, the
    /// reader has to go and find it in another screen — which is where this refusal used to
    /// leave them, with the whole upload already thrown away.
    #[test]
    fn a_block_carries_the_feeds_by_id_and_not_only_by_name() {
        let mut akahu = provider("Akahu", "akahu", 8, true);
        akahu.id = 42;
        let block = refused(None, &[akahu]);
        assert_eq!(
            block.feeds,
            vec![BlockingFeed {
                provider_id: 42,
                name: "Akahu".to_string()
            }]
        );
    }

    /// Nothing to sync, so there is nothing to offer — and a UI that rendered a Sync button here
    /// would be offering to fix an account's history from an import screen.
    #[test]
    fn an_unreadable_ledger_date_names_no_feed_to_sync() {
        let block = refused(Some("03/07/2019"), &[provider("Akahu", "akahu", 8, true)]);
        assert_eq!(block.reason, ImportBlockReason::UnreadableLedgerDate);
        assert!(block.feeds.is_empty());
    }

    #[test]
    fn every_waiting_feed_is_named_so_the_user_syncs_all_of_them() {
        let block = refused(
            None,
            &[
                provider("Akahu", "akahu", 8, true),
                provider("Statements", "csv", 8, true),
            ],
        );
        let msg = &block.message;
        assert!(msg.contains("Akahu") && msg.contains("Statements"), "{msg}");
        assert_eq!(block.feeds.len(), 2);
    }

    #[test]
    fn a_disabled_feed_or_one_on_another_account_is_not_waiting() {
        let providers = [
            provider("Akahu", "akahu", 8, false),
            provider("Akahu", "akahu", 9, true),
        ];
        let got = date_of(
            decide(None, &providers),
            "neither of these posts to account 8",
        );
        assert_eq!(got, None);
    }

    #[test]
    fn a_feed_whose_kind_is_no_longer_registered_is_not_waiting() {
        // Nothing can sync it, so it has no window pending and must not block the import.
        let providers = [provider("Retired", "decommissioned", 8, true)];
        let got = date_of(
            decide_cutover(
                ImportSource::AsbCsv,
                8,
                "Everyday",
                None,
                &providers,
                |_| false,
            ),
            "an unregistered kind posts nothing",
        );
        assert_eq!(got, None);
    }

    #[test]
    fn a_posted_at_that_is_not_a_date_blocks_the_item() {
        // A CSV provider stores the date cell verbatim, and `MIN()` under SQLite's BINARY
        // collation sorts a `0`-leading day ahead of every ISO date — so this one row is
        // exactly the one that decides the window, and it can't.
        let msg = refused(Some("03/07/2019"), &[]).message;
        assert!(
            msg.contains("03/07/2019"),
            "the offending value is quoted: {msg}"
        );
    }

    #[test]
    fn a_posted_at_too_short_to_hold_a_date_blocks_rather_than_panics() {
        assert!(refused(Some("2019-07"), &[]).message.contains("2019-07"));
    }

    #[test]
    fn a_posted_at_with_no_char_boundary_at_ten_blocks_rather_than_panics() {
        // `str::get` returns `None` mid-codepoint rather than panicking the way slicing would;
        // the point of this test is that the decision is still a block either way.
        assert!(!refused(Some("2019-07-0\u{1f600}3"), &[]).message.is_empty());
    }

    /// Only the person importing can know when a silent feed will start posting, so their answer
    /// is the best evidence there is — and it is the third way out of this block, beside syncing
    /// the feed and disabling it.
    #[test]
    fn a_stated_date_resolves_an_unsynced_feed_but_not_an_unreadable_one() {
        assert!(ImportBlockReason::UnsyncedFeed.resolvable_by_stating_cutover());
        // The read a stated date would be checked against is the broken thing, so stating one
        // would build the same double-count on a different number.
        assert!(!ImportBlockReason::UnreadableLedgerDate.resolvable_by_stating_cutover());
    }

    // ---- the rules that let every source share one cutover ------------------------------

    fn deriving(from: Option<&str>) -> Provider {
        let mut p = provider("Akahu", "akahu", 8, true);
        p.config = match from {
            Some(d) => json!({ "derive_transactions_from_balance": true, "derive_from": d }),
            None => json!({ "derive_transactions_from_balance": true }),
        };
        p
    }

    /// A balance-only feed states where its own half begins, and that statement needs no rows to
    /// exist yet — which is exactly the case the ledger read cannot answer. It therefore wins.
    #[test]
    fn a_feed_that_derives_from_a_balance_states_its_own_cutover() {
        let got = date_of(
            decide(None, &[deriving(Some("2024-03-01"))]),
            "a stated derive date is an answer",
        );
        assert_eq!(got, NaiveDate::from_ymd_opt(2024, 3, 1));
    }

    #[test]
    fn a_stated_derive_date_wins_over_what_the_ledger_shows() {
        let got = date_of(
            decide(
                Some("2025-01-01T00:00:00+00:00"),
                &[deriving(Some("2024-03-01"))],
            ),
            "both are available",
        );
        assert_eq!(
            got,
            NaiveDate::from_ymd_opt(2024, 3, 1),
            "the earlier, stated date is the one the other half actually starts at"
        );
    }

    /// The myIR case, and the reason `CutoverRule` exists. IR posts no transactions, so a loan
    /// whose feed has said nothing really does mean nothing else posts there — refusing would
    /// give advice ("sync it, then import again") that can never come true.
    #[test]
    fn a_lenient_source_takes_a_silent_feed_at_face_value() {
        let providers = [provider("Akahu", "akahu", 8, true)];
        let got = date_of(
            decide_cutover(
                ImportSource::MyirSls,
                8,
                "Student loan",
                None,
                &providers,
                registered,
            ),
            "a silent feed is not a block for this source",
        );
        assert_eq!(got, None);
        // …while the strict source, on the same facts, blocks the item.
        assert!(matches!(decide(None, &providers), Cutover::Blocked(_)));
    }

    /// A lenient source still holds back where there *is* something to hold back from — being
    /// lenient about silence is not the same as ignoring a feed that has spoken.
    #[test]
    fn a_lenient_source_still_holds_back_from_a_feed_that_has_posted() {
        let got = date_of(
            decide_cutover(
                ImportSource::MyirSls,
                8,
                "Student loan",
                Some("2024-06-01T00:00:00+00:00"),
                &[],
                registered,
            ),
            "a parseable date is an answer",
        );
        assert_eq!(got, NaiveDate::from_ymd_opt(2024, 6, 1));
    }

    /// `derive_transactions_from_balance` with no readable `derive_from` falls through rather
    /// than resolving to "no cutover" — otherwise a misconfigured connection would silently
    /// import the file whole, which is the failure the strict rule exists to prevent.
    #[test]
    fn a_deriving_feed_with_no_start_date_falls_through_to_the_other_tiers() {
        assert!(
            refused(None, &[deriving(None)]).message.contains("Akahu"),
            "with nothing else to go on, a strict source must block the item"
        );
        let got = date_of(
            decide(Some("2025-01-01T00:00:00+00:00"), &[deriving(None)]),
            "the ledger can still answer",
        );
        assert_eq!(got, NaiveDate::from_ymd_opt(2025, 1, 1));
    }

    // ---- holding back --------------------------------------------------------------------

    fn row(posted_at: &str, amount_minor: i64) -> ImportRow {
        ImportRow {
            external_id: posted_at.to_string(),
            posted_at: posted_at.to_string(),
            amount_minor,
            currency_code: None,
            description: String::new(),
            merchant: None,
            category_name: None,
            category_group: None,
            category_kind: None,
            is_one_off: false,
        }
    }

    fn item(rows: Vec<ImportRow>) -> ParsedItem {
        ParsedItem {
            source_account: "12-3456-0000123-50".to_string(),
            label: None,
            holder: None,
            sources: vec![],
            rows,
            covered_from: None,
            covered_to: None,
            stated_closing_minor: None,
            opening_balance: None,
            extras: ParsedExtras::None,
            warnings: vec![],
        }
    }

    /// On or after the cutover, not merely after it: the cutover is the first date the other
    /// feed owns, so a row dated exactly then is already theirs.
    #[test]
    fn the_cutover_day_itself_belongs_to_the_other_feed() {
        let mut it = item(vec![
            row("2024-02-29T00:00:00+00:00", 100),
            row("2024-03-01T00:00:00+00:00", 200),
            row("2024-03-02T00:00:00+00:00", 300),
        ]);
        let held = hold_back(&mut it, NaiveDate::from_ymd_opt(2024, 3, 1));
        assert_eq!(held, 2);
        assert_eq!(it.rows.len(), 1);
        assert_eq!(it.rows[0].amount_minor, 100);
    }

    #[test]
    fn no_cutover_holds_nothing_back() {
        let mut it = item(vec![row("2024-03-01T00:00:00+00:00", 100)]);
        assert_eq!(hold_back(&mut it, None), 0);
        assert_eq!(it.rows.len(), 1);
    }

    /// A `posted_at` too short to slice must not panic the import — `str::get` returns `None`
    /// and the row is compared whole, which sorts it before any ISO date and keeps it.
    #[test]
    fn a_malformed_posted_at_does_not_panic_the_hold_back() {
        let mut it = item(vec![row("2024", 100), row("\u{1f600}", 200)]);
        let held = hold_back(&mut it, NaiveDate::from_ymd_opt(2024, 3, 1));
        assert_eq!(
            held, 1,
            "'2024' sorts before '2024-03-01'; the emoji after it"
        );
    }

    #[test]
    fn minor_units_read_as_money_including_negatives_and_sub_dollar_amounts() {
        assert_eq!(major(114_269_63), "114269.63");
        assert_eq!(major(-1_50), "-1.50");
        assert_eq!(major(0), "0.00");
        assert_eq!(major(7), "0.07");
    }
}
