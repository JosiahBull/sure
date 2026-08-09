//! The vocabulary every file import speaks: which source a blob came from, and what
//! happened to it.
//!
//! Four importers grew separately — an ASB transaction export, a myIR student-loan
//! workbook, a Sharesies export zip, a hand-written CSV — and each arrived with its own
//! result type, its own provider-tag `format!`, and its own answer to "does this account
//! kind take this file?". The three copies drifted in the ways duplicated tables always do:
//! only one of them could preview, only one could be undone, and only one pre-checked how
//! many bytes it had been handed.
//!
//! So the differences that are real live in [`ImportSource`]'s methods, exhaustively
//! matched, and the ones that were only accidents live nowhere. A fifth importer is a
//! compile error at each question it has to answer.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::AccountKind;

/// The largest upload any importer will look at, checked once before a parser is handed the
/// bytes.
///
/// A seven-year ASB export is a few hundred kilobytes and a myIR workbook is smaller, so
/// this is orders of magnitude above the real cases and exists only to bound the hostile
/// one. It is deliberately *the same* number as the HTTP body cap
/// (`sure_api::config::Limits::max_import_body_bytes`): while the two differed, an upload
/// between the two figures was accepted by the transport and then refused by the parser,
/// which reads to the person uploading as the server changing its mind.
pub const MAX_UPLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Which importer a blob belongs to. Decided by sniffing the bytes
/// (`sure_app::ports::ImportRegistry::detect`), or stated outright by the caller when the
/// sniff gets it wrong.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    /// ASB "Export transactions" — a `.csv` in either of its two shapes, or a `.zip` of
    /// them spanning several accounts.
    AsbCsv,
    /// myIR "TAP SLS Transactions" — an `.xlsx`, or a `.zip` of them covering one loan.
    MyirSls,
    /// A Sharesies export `.zip`: wallet transactions, holdings and dividends.
    SharesiesZip,
    /// A plain transaction `.csv` someone assembled themselves. The fallback, tried last,
    /// because it would otherwise claim every file above.
    CsvUpload,
}

impl ImportSource {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        use ImportSource::*;
        match self {
            AsbCsv => "asb_csv",
            MyirSls => "myir_sls",
            SharesiesZip => "sharesies_zip",
            CsvUpload => "csv_upload",
        }
    }

    /// What to call this source in a sentence a person reads.
    pub fn label(self) -> &'static str {
        use ImportSource::*;
        match self {
            AsbCsv => "ASB transaction export",
            MyirSls => "myIR student-loan export",
            SharesiesZip => "Sharesies export",
            CsvUpload => "transaction CSV",
        }
    }

    /// The `transactions.provider` stem this source writes under. Every row it imports
    /// carries `{stem}#{account id}`, which is what makes an import findable — and
    /// undoable — without touching another source's rows on the same account.
    ///
    /// **These four strings are load-bearing history.** They are already written into the
    /// live database by the importers that predate this module, so changing one does not
    /// migrate anything: it orphans every row already imported under the old spelling from
    /// both undo and the previous-import routing tier, silently, with the import appearing
    /// to succeed. `provider_tag_stems_are_the_ones_already_in_the_database` pins them.
    ///
    /// They must also stay distinct from every *provider kind*, because a scheduled sync
    /// tags its rows `{kind}#{provider id}` from a different id sequence
    /// (`sure_app::sync`): a hand-uploaded CSV tagged `csv#5` keyed on account 5 would sit
    /// under the same tag as the CSV *connection* 5's rows, and undoing either would delete
    /// the other's. Hence `csv-upload`, and hence: never name a provider kind after a
    /// source here.
    pub fn tag_stem(self) -> &'static str {
        use ImportSource::*;
        match self {
            AsbCsv => "asb",
            MyirSls => "myir",
            SharesiesZip => "sharesies",
            CsvUpload => "csv-upload",
        }
    }

    /// The full provider tag for one account — see [`Self::tag_stem`].
    pub fn provider_tag(self, account_id: i64) -> String {
        format!("{}#{account_id}", self.tag_stem())
    }

    /// How far this source's rows must give way to a live feed on the same account — see
    /// [`CutoverRule`] for what each answer means and why the sources differ.
    pub fn cutover_rule(self) -> CutoverRule {
        use ImportSource::*;
        match self {
            // A bank posts the same transactions Akahu does, so the halves must not overlap,
            // and a feed that has been connected but hasn't spoken is precisely the case
            // where guessing costs a permanently doubled ledger.
            AsbCsv | CsvUpload => CutoverRule::Strict,
            // IR posts no transactions at all — the forward half of a loan's ledger is
            // *derived* from its balance, by `sure_app::tasks::balance_delta`, and that
            // connection records the date it starts deriving from. A silent feed on a loan
            // therefore really does mean nothing else posts here, so refusing would block a
            // legitimate import with advice ("sync it, then import again") that can never
            // come true.
            MyirSls => CutoverRule::Lenient,
            // Nothing else posts a Sharesies wallet, a holding lot or a dividend. There is no
            // second source to collide with, so there is no cutover to derive.
            SharesiesZip => CutoverRule::Never,
        }
    }

    /// Whether an upload of exactly one thing may be routed to the only account that accepts this
    /// source, when nothing identified it.
    ///
    /// This tier exists to replace what a per-account URL used to provide for free, and it is only
    /// sound where the export **cannot** identify itself. A myIR export names an SLS account and a
    /// Sharesies export names nothing at all — neither matches any Sure field, so every other tier
    /// returns nothing and the upload would be reported unroutable while the answer is the only one
    /// there is.
    ///
    /// A bank export is the opposite: it carries an account number, which the stored-number and
    /// account-name tiers match on. If those found nothing, the honest reading is *that account is
    /// not in Sure yet* — not "it must be the one that is". Routing on "there's only one" would
    /// then file a savings account's seven-year history into a chequing account, which is what the
    /// evidence tiers exist to prevent.
    pub fn routes_by_sole_candidate(self) -> bool {
        use ImportSource::*;
        match self {
            MyirSls | SharesiesZip => true,
            // …and a hand-written CSV names nothing *and* every kind accepts it, so "the only
            // candidate" isn't a narrowing at all. It is always routed by an explicit assignment.
            AsbCsv | CsvUpload => false,
        }
    }

    /// Whether this source's file makes sense for an account of this kind. The refusal is
    /// worth making: putting a savings account's history into a mortgage is not a
    /// recoverable mistake, and the file itself never names a Sure account.
    ///
    /// Exhaustively matched rather than defaulted, so a new [`AccountKind`] has to come here
    /// and decide (CLAUDE.md rule 2).
    pub fn accepts(self, kind: AccountKind) -> bool {
        use AccountKind::*;
        match self {
            // ASB exports the same CSV for everyday, savings and card accounts. The rest of
            // Sure's kinds either have no ASB statement (a property, a share holding) or
            // have an importer of their own below.
            ImportSource::AsbCsv => match kind {
                Cash | Bank | Savings | CreditCard | RevolvingCredit => true,
                Mortgage | StudentLoan | Loan | Liability | Vehicle | RealEstate | Asset
                | SharesNz | SharesUs | SharesPrivate | Brokerage | Crypto => false,
            },
            ImportSource::MyirSls => match kind {
                StudentLoan => true,
                Cash | Bank | Savings | CreditCard | RevolvingCredit | Mortgage | Loan
                | Liability | Vehicle | RealEstate | Asset | SharesNz | SharesUs
                | SharesPrivate | Brokerage | Crypto => false,
            },
            ImportSource::SharesiesZip => match kind {
                Brokerage => true,
                Cash | Bank | Savings | CreditCard | RevolvingCredit | Mortgage | StudentLoan
                | Loan | Liability | Vehicle | RealEstate | Asset | SharesNz | SharesUs
                | SharesPrivate | Crypto => false,
            },
            // Every kind, and not a match on `kind` at all: a list of dated amounts is
            // meaningful against anything that has a ledger, and the CSV provider this
            // replaces as a human affordance has never had a kind guard either. Narrowing
            // it here would take away something that works today.
            ImportSource::CsvUpload => true,
        }
    }
}

impl std::str::FromStr for ImportSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "asb_csv" => ImportSource::AsbCsv,
            "myir_sls" => ImportSource::MyirSls,
            "sharesies_zip" => ImportSource::SharesiesZip,
            "csv_upload" => ImportSource::CsvUpload,
            other => return Err(format!("unknown import source '{other}'")),
        })
    }
}

/// How much of an import a live feed on the same account is entitled to take back.
///
/// The problem this exists for: dedupe is `(provider, external_id)` and cannot see across two
/// sources, so one movement that arrives from both a feed and an uploaded export is two rows
/// in the ledger, forever. The cutover is the date from which the feed owns the account, and
/// an import stops there.
///
/// It is never a parameter. It is read from the account's own feeds, so the two halves cannot
/// be made to overlap by getting an argument wrong — and the sources differ only in what to do
/// when that read can't answer.
///
/// Named `Never` rather than `None` so it doesn't shadow `Option::None` at use sites, the same
/// reason `SyncOutcome` isn't `SyncStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutoverRule {
    /// Nothing else posts what this source posts. Nothing is held back, and no feed is
    /// consulted.
    Never,
    /// Held back from wherever another feed's coverage begins: the date it has posted from, or
    /// the date a balance-derived connection says it derives from. A feed that is connected
    /// and enabled but has done *neither* is a **refusal**, not a guess — "nothing else posts
    /// here" and "we couldn't tell" would otherwise both produce a silent `None` that imports
    /// the file whole, and the cost of being wrong is one re-upload against a doubled ledger
    /// nobody notices.
    Strict,
    /// Held back the same way, but a silent feed is taken at face value. For an account whose
    /// feed genuinely posts no transactions, so there is nothing waiting to collide.
    Lenient,
}

/// How an item in an upload was matched to a Sure account. Reported so the UI can say *why*
/// it pre-selected one, and so a guess is visibly a guess.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportMatch {
    /// The request named the account outright. The only certainty here.
    Assigned,
    /// This account already holds rows this source imported for that same source account —
    /// the durable memory that makes every re-upload route itself.
    PreviousImport,
    /// The account's stored `account_number` metadata is that number.
    AccountNumber,
    /// The account's *name* contains that number, the way a name does when two accounts
    /// would otherwise be indistinguishable ("Emergency Fund (0000123-51)"). A hint, not
    /// proof — worth pre-selecting, worth showing as a guess.
    AccountName,
    /// The only account of a kind this source accepts. Sound where a source can name just
    /// one thing (one student loan, one brokerage) and there is one candidate; never used
    /// where two accounts could both be meant.
    OnlyCandidate,
    /// The export names a person, exactly one household member answers to that name, and
    /// exactly one account this source accepts is theirs. The tier that tells two student
    /// loans apart, where the file's own identifier (an SLS account id) matches no Sure
    /// field and [`Self::OnlyCandidate`] has to decline.
    AccountOwner,
    /// The item's own rows match the transactions this account already holds, over the
    /// window both cover. The last resort, for an account whose number nothing recorded —
    /// but evidence-rich when it fires: a busy account's run of dated amounts is close to a
    /// fingerprint, and the match is reported with the counts behind it.
    TransactionHistory,
}

impl ImportMatch {
    /// The wire representation (snake_case) — matches `#[serde(rename_all = "snake_case")]`.
    pub fn as_str(self) -> &'static str {
        use ImportMatch::*;
        match self {
            Assigned => "assigned",
            PreviousImport => "previous_import",
            AccountNumber => "account_number",
            AccountName => "account_name",
            OnlyCandidate => "only_candidate",
            AccountOwner => "account_owner",
            TransactionHistory => "transaction_history",
        }
    }
}

/// Why one thing in an upload could not be imported, where the obstacle is a conflict that has
/// to be *resolved* rather than an account nobody could identify.
///
/// A block is per item, and that is the whole point of it existing. These conditions are
/// properties of one target account — a feed pending on it, a bad row already on it — and an
/// upload is routinely a zip of a household's exports going to a dozen accounts. Failing the
/// request threw away eleven good imports to report a twelfth's problem, and left the reader no
/// way to act on it: the preview never rendered, so the "skip this one" control that was already
/// on screen for every other reason was unreachable for this one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportBlockReason {
    /// A connected, enabled feed on the target account has posted nothing yet, so the period it
    /// owns cannot be derived from the ledger and this source may not guess — see
    /// [`CutoverRule::Strict`]. Resolvable three ways: sync the feed, disable it, or state the
    /// date it owns from.
    UnsyncedFeed,
    /// A `posted_at` already on the account won't parse, and under SQLite's BINARY collation a
    /// non-ISO date sorts ahead of every ISO one — so that single row is exactly the one
    /// `MIN(posted_at)` would have derived the cutover from. Correcting the row is the only
    /// resolution; a stated date would be built on the same broken read.
    UnreadableLedgerDate,
}

impl ImportBlockReason {
    /// The wire representation (snake_case) — matches `#[serde(rename_all = "snake_case")]`.
    pub fn as_str(self) -> &'static str {
        use ImportBlockReason::*;
        match self {
            UnsyncedFeed => "unsynced_feed",
            UnreadableLedgerDate => "unreadable_ledger_date",
        }
    }

    /// Whether stating the cutover date outright is a legitimate way out of this block. Only the
    /// person importing can know when a feed that has never spoken will start posting from, so
    /// for [`Self::UnsyncedFeed`] their answer is the best evidence there is. An unreadable row
    /// is the opposite: the read the date would be checked against is the broken thing.
    pub fn resolvable_by_stating_cutover(self) -> bool {
        use ImportBlockReason::*;
        match self {
            UnsyncedFeed => true,
            UnreadableLedgerDate => false,
        }
    }
}

/// A feed standing between one item and its account, named *with its id* so a UI can offer to
/// sync or disable it in place. The message has always named these; without the id the reader
/// had to go and find them by name in another screen.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct BlockingFeed {
    pub provider_id: i64,
    pub name: String,
}

/// One item's unresolved conflict: nothing of it was written, and here is what it would take.
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct ImportBlock {
    pub reason: ImportBlockReason,
    /// The sentence to put in front of the reader, built where the specifics are known.
    pub message: String,
    /// The feeds waiting to post. Empty for every reason but [`ImportBlockReason::UnsyncedFeed`].
    pub feeds: Vec<BlockingFeed>,
}

/// Which kind of record an [`ImportExtra`] counts — the things a source writes that aren't
/// transactions.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImportExtraKind {
    Holdings,
    Dividends,
}

/// Non-transaction records one item wrote. Only the Sharesies source produces any; the field
/// is empty rather than absent for every other source, so the UI has one shape to render.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportExtra {
    pub kind: ImportExtraKind,
    pub imported: i64,
    pub skipped: i64,
}

/// Whether the ledger adds up, for the one source whose export states a balance to check
/// against (ASB). Grouped rather than inlined into [`ImportItem`] so a myIR or Sharesies
/// result doesn't carry six nulls describing a check that was never available to it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Reconciliation {
    /// The closing balance the export itself states, and the balance Sure holds for the
    /// account on that day. Equal is the strongest available evidence that the export
    /// belongs to this account and its coverage is complete.
    pub ledger_balance_minor: Option<i64>,
    pub account_balance_minor: Option<i64>,
    /// What the account must have held immediately before the first row, given the closing
    /// balance and the movements in between.
    pub implied_opening_minor: Option<i64>,
    /// The opening balance actually recorded (or, on a dry run, that would be), and the day
    /// it is dated — the day before the first row.
    ///
    /// Distinct from `implied_opening_minor`, which is only the arithmetic: this is `None`
    /// when the caller opted out, or when the account already holds rows from before that
    /// date and an "opening" balance would really be a movement in the middle of the
    /// ledger. Without it the reconstructed history starts from nothing, because an account
    /// reads as 0 before its earliest transaction.
    pub opening_balance_minor: Option<i64>,
    pub opening_balance_as_of: Option<String>,
    /// Every amount on the account summed, once the import has been written. Equal to
    /// `account_balance_minor` means the ledger reconciles: the opening balance plus every
    /// movement since lands exactly on the balance the account is recorded at. Unequal means
    /// some period is double-counted or missing — most likely a live feed's rows for the
    /// overlap disagreeing with the export's. `None` on a dry run, where nothing was written.
    pub ledger_sum_minor: Option<i64>,
}

/// What happened to one thing inside an upload: one ASB account's export, one myIR loan, one
/// Sharesies account. An upload of a single file has exactly one; a zip of a household's
/// bank exports has one per bank account.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportItem {
    /// How the *source* names it (`12-3456-0000123-50`, `012-345-678-SLS004`), echoed back
    /// so a wrong upload is obvious, and the key `assign` addresses it by. Not to be
    /// confused with `account_id`, which is Sure's.
    pub source_account: String,
    /// The Sure account the rows went to (or would go to). `None` means nothing identified
    /// one and **nothing was written** — the caller has to say which and import again.
    pub account_id: Option<i64>,
    pub account_name: Option<String>,
    /// How `account_id` was arrived at.
    pub matched_by: Option<ImportMatch>,
    /// The file(s) in the upload this item's rows came from.
    pub sources: Vec<String>,
    /// The source's own name for the thing, where it has one — ASB's product (`Streamline`).
    pub label: Option<String>,
    /// The window the rows cover.
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    /// Rows in the file, before anything was held back.
    pub rows_total: i64,
    /// Rows a commit would insert or skip — what a preview shows on its button. Equal to
    /// `imported + skipped` once committed, which is what makes the preview trustworthy.
    pub would_import: i64,
    pub imported: i64,
    pub skipped: i64,
    /// Rows withheld because a connected feed already covers their dates.
    pub held_back: i64,
    /// The cutover the rows were withheld from, if any feed set one.
    pub cutover: Option<String>,
    /// Present only for a source whose export states a balance to check — see
    /// [`Reconciliation`].
    pub reconciliation: Option<Reconciliation>,
    /// Set when a conflict on the target account stopped *this item* — nothing of it was
    /// written, whatever the rest of the upload did. `Some` and `imported > 0` is a
    /// contradiction: the block is decided before the only write.
    pub blocked: Option<ImportBlock>,
    /// Non-transaction records written. Empty for every source but Sharesies.
    pub extras: Vec<ImportExtra>,
    /// Non-fatal observations — an unfamiliar transaction type, rows held back, a balance
    /// that doesn't reconcile, or why nothing was imported.
    pub warnings: Vec<String>,
}

/// The result of one upload, whatever was in it and whichever source it came from.
///
/// One type serves the dry run and the commit, so a preview can never describe an import the
/// commit wouldn't perform: the pipeline branches once, at the end, and everything above the
/// branch is shared. On a dry run `imported`/`skipped` stay 0 and `would_import` carries the
/// count.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportResult {
    /// Whether this was a preview. `false` means the rows are in the database.
    pub dry_run: bool,
    /// The source the upload was read as — sniffed, unless the request named one.
    pub source: ImportSource,
    /// One per thing found in the upload, ordered by `source_account`.
    pub items: Vec<ImportItem>,
    /// Upload-level observations — files that weren't exports, entries that were skipped.
    pub warnings: Vec<String>,
}

/// One recorded import: what an upload did to one account, and when.
///
/// The counterpart to [`ProviderSync`](crate::ProviderSync) for uploads rather than feeds. It
/// answers the question the transactions themselves can only answer expensively — how much of
/// this account came from an export, and how far back does it reach — and it is not a handle for
/// undo, which is per (account, source); see the `imports` migration for why per-upload undo has
/// no honest definition.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportRecord {
    pub id: i64,
    pub account_id: i64,
    pub source: ImportSource,
    /// How the source named what was imported — an ASB account number, an SLS account id.
    pub source_account: Option<String>,
    /// The files inside the upload. Which downloads these were is the one thing neither the rows
    /// nor the window can tell you afterwards.
    pub filenames: Vec<String>,
    pub imported: i64,
    pub skipped: i64,
    pub held_back: i64,
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    pub cutover: Option<String>,
    pub created_at: String,
}

/// The result of undoing one source's import on one account.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ImportUndoResult {
    pub deleted: i64,
    /// Non-transaction records removed — holdings and dividends, for the Sharesies source.
    pub extras: Vec<ImportExtra>,
    /// What an undo could not take back, so the panel can say so rather than implying the
    /// account is as it was.
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    const EVERY_SOURCE: [ImportSource; 4] = [
        ImportSource::AsbCsv,
        ImportSource::MyirSls,
        ImportSource::SharesiesZip,
        ImportSource::CsvUpload,
    ];

    /// The one test whose failure means data loss rather than a broken build: these stems are
    /// already written into `transactions.provider` in every existing database, and nothing
    /// migrates them. A row whose tag no longer matches what its source computes is invisible
    /// to undo and to the previous-import routing tier, while the import still reports success.
    #[test]
    fn provider_tag_stems_are_the_ones_already_in_the_database() {
        assert_eq!(ImportSource::AsbCsv.provider_tag(12), "asb#12");
        assert_eq!(ImportSource::MyirSls.provider_tag(12), "myir#12");
        assert_eq!(ImportSource::SharesiesZip.provider_tag(12), "sharesies#12");
        // The one stem with no history to preserve, chosen so it cannot collide with the
        // `csv#{provider id}` a scheduled CSV sync writes.
        assert_eq!(ImportSource::CsvUpload.provider_tag(12), "csv-upload#12");
    }

    /// Two sources sharing a stem would make one's undo delete the other's rows.
    #[test]
    fn every_source_has_its_own_tag_stem() {
        let mut seen = std::collections::HashSet::new();
        for source in EVERY_SOURCE {
            assert!(
                seen.insert(source.tag_stem()),
                "{} reuses the stem {}",
                source.as_str(),
                source.tag_stem()
            );
        }
    }

    #[test]
    fn every_source_round_trips_through_its_wire_name() {
        for source in EVERY_SOURCE {
            assert_eq!(ImportSource::from_str(source.as_str()), Ok(source));
        }
        assert!(ImportSource::from_str("asb").is_err());
    }

    /// The three sources that replace a per-kind route must accept exactly the kinds that
    /// route accepted, or an account that could be imported into yesterday can't be today.
    #[test]
    fn each_source_accepts_the_kinds_its_old_route_did() {
        use AccountKind::*;
        for kind in [Cash, Bank, Savings, CreditCard, RevolvingCredit] {
            assert!(ImportSource::AsbCsv.accepts(kind), "{kind:?}");
        }
        for kind in [Mortgage, StudentLoan, Loan, RealEstate, Brokerage, Crypto] {
            assert!(!ImportSource::AsbCsv.accepts(kind), "{kind:?}");
        }
        assert!(ImportSource::MyirSls.accepts(StudentLoan));
        assert!(!ImportSource::MyirSls.accepts(Loan));
        assert!(ImportSource::SharesiesZip.accepts(Brokerage));
        assert!(!ImportSource::SharesiesZip.accepts(SharesNz));
    }

    /// A blob has to be routable somewhere, or the fallback isn't one.
    #[test]
    fn the_fallback_source_accepts_every_kind() {
        use AccountKind::*;
        for kind in [
            Cash,
            Bank,
            Savings,
            CreditCard,
            RevolvingCredit,
            Mortgage,
            StudentLoan,
            Loan,
            Liability,
            Vehicle,
            RealEstate,
            Asset,
            SharesNz,
            SharesUs,
            SharesPrivate,
            Brokerage,
            Crypto,
        ] {
            assert!(ImportSource::CsvUpload.accepts(kind), "{kind:?}");
        }
    }
}
