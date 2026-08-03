use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::types::{AccountKind, SaveAccount};

/// Whether a sync attempt succeeded. Stored as `provider_syncs.status` (plain `TEXT`).
/// Named `SyncOutcome` rather than `SyncStatus` so `SyncOutcome::Ok` doesn't shadow
/// `Result::Ok` at use sites.
#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SyncOutcome {
    Ok,
    Error,
}

impl SyncOutcome {
    /// The stored/wire representation (snake_case) — matches
    /// `#[serde(rename_all = "snake_case")]`. Used by the DAL to bind this as a plain
    /// `TEXT` column without `sure-core` needing an `sqlx` dependency.
    pub fn as_str(self) -> &'static str {
        match self {
            SyncOutcome::Ok => "ok",
            SyncOutcome::Error => "error",
        }
    }
}

impl std::str::FromStr for SyncOutcome {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "ok" => SyncOutcome::Ok,
            "error" => SyncOutcome::Error,
            other => return Err(format!("unknown sync outcome '{other}'")),
        })
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Provider {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub account_id: i64,
    pub config: Value,
    pub enabled: bool,
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SaveProvider {
    pub name: String,
    pub kind: String,
    pub account_id: i64,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SyncRequest {
    /// Inline data for payload-based providers (e.g. CSV text).
    #[serde(default)]
    pub payload: Option<String>,
}

/// Link an upstream account (surfaced by `GET /provider-kinds/{kind}/accounts`) to a local
/// account, creating the `providers` connection in the same step. Exactly one of
/// `new_account` / `existing_account_id` must be set.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkProviderAccount {
    pub kind: String,
    /// The upstream's stable identifier for this account (`ProviderAccount::external_id`);
    /// stored as `config.external_account_id` on the created `providers` row.
    pub external_id: String,
    /// Name for the new `providers` row (not the account itself).
    pub name: String,
    /// Create a new local account for this external account.
    #[serde(default)]
    pub new_account: Option<SaveAccount>,
    /// Or attach to an already-existing local account instead.
    #[serde(default)]
    pub existing_account_id: Option<i64>,
}

/// Link several upstream accounts to a *single* local account at once — the case where one
/// real account is exposed by the source as several sibling accounts (e.g. a Sharesies
/// brokerage account surfaces one Akahu account per currency wallet). Each member becomes
/// its own `providers` row pointing at the one local account, so their transactions/
/// balances all flow into it. Exactly one of `new_account` / `existing_account_id` must be
/// set; the account is created once and every member is linked in the same transaction.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkProviderGroup {
    pub kind: String,
    /// The upstream accounts to link (must be non-empty).
    pub members: Vec<LinkGroupMember>,
    #[serde(default)]
    pub new_account: Option<SaveAccount>,
    #[serde(default)]
    pub existing_account_id: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LinkGroupMember {
    /// The upstream's stable identifier (`ProviderAccount::external_id`).
    pub external_id: String,
    /// Name for this member's `providers` row.
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderSync {
    pub id: i64,
    pub provider_id: i64,
    pub imported: i64,
    pub skipped: i64,
    pub status: SyncOutcome,
    pub detail: Option<String>,
    pub created_at: String,
}

/// Result of a myIR student-loan export upload (`POST
/// /api/accounts/{id}/student-loan/import`). Mirrors [`ProviderSync`]'s imported/skipped
/// counts, plus what the exports covered — the window is the useful part, because the
/// balance reconstruction is only trustworthy back to the earliest date the ledger reaches.
#[derive(Debug, Serialize, ToSchema)]
pub struct StudentLoanImportResult {
    pub imported: i64,
    pub skipped: i64,
    /// The SLS account the exports were for, echoed back so a wrong upload is obvious.
    pub account_id: String,
    /// The union of every uploaded export's window.
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    /// Non-fatal observations — an unfamiliar transaction type, rows held back by the
    /// balance-delta cutover.
    pub warnings: Vec<String>,
}

/// How an export in a multi-account upload was matched to a Sure account. Reported so the
/// UI can say *why* it pre-selected one, and so a guess is visibly a guess.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AsbMatch {
    /// The request named the account outright. The only certainty here.
    Assigned,
    /// This account already holds rows imported from that ASB account number — the durable
    /// memory that makes every re-upload route itself.
    PreviousImport,
    /// The account's stored `account_number` metadata is that number.
    AccountNumber,
    /// The account's *name* contains that number, the way a name does when two accounts
    /// would otherwise be indistinguishable ("Emergency Fund (0000123-51)"). A hint, not
    /// proof — worth pre-selecting, worth showing as a guess.
    AccountName,
}

impl AsbMatch {
    /// The wire representation (snake_case) — matches `#[serde(rename_all = "snake_case")]`.
    pub fn as_str(self) -> &'static str {
        use AsbMatch::*;
        match self {
            Assigned => "assigned",
            PreviousImport => "previous_import",
            AccountNumber => "account_number",
            AccountName => "account_name",
        }
    }
}

/// Result of importing one ASB export. One type serves the dry run and the commit, so a
/// preview can never describe an import the commit wouldn't perform: the handler branches
/// once, at the end, and everything above the branch is shared. On a dry run
/// `imported`/`skipped` stay 0 and `would_import` carries the count.
#[derive(Debug, Serialize, ToSchema)]
pub struct AsbImportResult {
    /// Whether this was a preview. `false` means the rows are in the database.
    pub dry_run: bool,
    pub imported: i64,
    pub skipped: i64,
    /// Rows the commit would insert or skip — what the preview shows on its button.
    pub would_import: i64,
    /// Rows withheld because a connected feed already covers their dates.
    pub held_back: i64,
    /// The cutover the rows were withheld from, if any feed set one.
    pub cutover: Option<String>,
    /// Rows in the file, before the cutover.
    pub rows_total: i64,
    /// The ASB account the export was for (`12-3136-0000123-50`), echoed back so a wrong
    /// upload is obvious. Not to be confused with `account_id`, which is Sure's.
    pub asb_account: String,
    /// The Sure account the rows went to (or would go to). `None` on a multi-account upload
    /// where nothing identified it — the caller has to say which, and nothing was imported.
    pub account_id: Option<i64>,
    pub account_name: Option<String>,
    /// How `account_id` was arrived at.
    pub matched_by: Option<AsbMatch>,
    /// The file(s) in the upload this account's rows came from.
    pub sources: Vec<String>,
    /// ASB's product name for it (e.g. `Streamline`).
    pub product: Option<String>,
    /// The window the file's rows cover.
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    /// The closing balance ASB states, and the balance Sure holds for the account on that
    /// day. Equal is the strongest available evidence that the export belongs to this
    /// account and its coverage is complete.
    pub ledger_balance_minor: Option<i64>,
    pub account_balance_minor: Option<i64>,
    /// What the account must have held immediately before the file's first row, given the
    /// closing balance and the movements in between.
    pub implied_opening_minor: Option<i64>,
    /// The opening balance actually recorded (or, on a dry run, that would be), and the day
    /// it is dated — the day before the first row.
    ///
    /// Distinct from `implied_opening_minor`, which is only the arithmetic: this is `None`
    /// when the caller opted out, or when the account already holds rows from before that
    /// date and an "opening" balance would really be a movement in the middle of the ledger.
    /// Without it the reconstructed history starts from nothing, because an account reads as
    /// 0 before its earliest transaction.
    pub opening_balance_minor: Option<i64>,
    pub opening_balance_as_of: Option<String>,
    /// Every amount on the account summed, once the import has been written. Equal to
    /// `account_balance_minor` means the ledger reconciles: the opening balance plus every
    /// movement since lands exactly on the balance the account is recorded at. Unequal means
    /// some period is double-counted or missing — most likely a live feed's rows for the
    /// overlap disagreeing with the export's. `None` on a dry run, where nothing was written.
    pub ledger_sum_minor: Option<i64>,
    /// Non-fatal observations — an unfamiliar transaction type, rows held back, a balance
    /// that doesn't reconcile.
    pub warnings: Vec<String>,
}

/// Result of a whole ASB upload (`POST /api/asb/import`) — one entry per ASB account the
/// upload named, so a zip of every account reports itself account by account.
#[derive(Debug, Serialize, ToSchema)]
pub struct AsbUploadResult {
    pub dry_run: bool,
    /// One per ASB account found, ordered by account number. An entry with no `account_id`
    /// was not imported: nothing said where it belongs.
    pub exports: Vec<AsbImportResult>,
    /// Upload-level observations — files that weren't exports, more than one account found.
    pub warnings: Vec<String>,
}

/// Result of removing a previous ASB import (`DELETE /api/accounts/{id}/asb/import`).
#[derive(Debug, Serialize, ToSchema)]
pub struct AsbUndoResult {
    pub deleted: i64,
}

/// An upstream account surfaced by a provider that supports account discovery
/// (see `sure_app::ports::TransactionProvider::list_accounts`) — not yet linked to a
/// local `Account`. Surfaced by `GET /provider-kinds/{kind}/accounts`. Lives here, with
/// the other provider API DTOs, so both the provider adapters and the OpenAPI document
/// can name it without either depending on the other.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderAccount {
    /// Stable identifier from the source; stored as `config.external_account_id` on the
    /// `providers` row once linked, and used to fetch that account's transactions.
    pub external_id: String,
    pub name: String,
    pub currency_code: String,
    /// The financial institution's display name (e.g. "ASB"), if the source reports one.
    pub institution: Option<String>,
    /// Which upstream *authorisation* (login) this account was discovered through.
    ///
    /// Not the institution: two people who each connect their own ASB login produce two
    /// values here and one institution. It is the only thing in a discovery response that
    /// separates one person's accounts from another's — Akahu reports no account-holder
    /// name (`meta.holder` is empty in practice, and `/parties` needs a permission a
    /// personal app doesn't have), so the household attribution this drives is a grouping,
    /// not a lookup. `None` for sources with no such concept.
    pub authorisation_id: Option<String>,
    /// The account number as the source formats it (e.g. `12-3456-0123456-00`), when it
    /// reports one.
    ///
    /// Two accounts under one login routinely share a name ("Emergency Fund" twice), and
    /// the *same* joint account seen through two logins can carry a different nickname in
    /// each — so this is what actually identifies an account to the person linking it.
    pub account_number: Option<String>,
    /// Best-effort suggestion for the local account's `kind`; the user confirms/edits it
    /// when linking, so an imperfect guess here isn't a correctness problem.
    pub kind_hint: AccountKind,
    pub balance_minor: i64,
    /// Whether the source can provide transaction history for this account (some upstream
    /// account types are balance-only).
    pub supports_transactions: bool,
}

/// Metadata about an available provider kind, surfaced via `GET /provider-kinds`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProviderKind {
    pub kind: String,
    pub description: String,
    pub accepts_payload: bool,
    pub supports_account_discovery: bool,
}
