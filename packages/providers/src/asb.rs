//! Parses ASB "Export transactions" CSVs into normalized bank transactions. Pure parsing
//! only — no DB, no `TransactionProvider` impl, since there's exactly one implementation of
//! this ever (see `docs/ARCHITECTURE.md`'s "plain functions where polymorphism isn't real"),
//! mirroring [`crate::myir`] and [`crate::sharesies`]. `sure-api` takes the upload, calls
//! [`parse_upload`], and persists the result via `sure_dal::providers`.
//!
//! One upload is a CSV or a zip of them, and a zip may hold **several accounts** — ASB
//! exports one file per account, and a household with a chequing account and half a dozen
//! savings pots would otherwise repeat the exercise a dozen times. So [`parse_upload`]
//! returns one [`AsbExport`] per ASB account number it found, reconciling several files of
//! the *same* account into one; deciding which Sure account each belongs to is the caller's
//! job, since nothing in the file names it.
//!
//! Akahu serves about two years of history; ASB's own export reaches seven. The two
//! therefore overlap, and because dedupe is `(provider, external_id)` and the two sources
//! tag their rows differently, nothing downstream can spot a row that arrives twice. So the
//! caller calls [`AsbExport::hold_back_from`] with the first date the live feed owns, leaving
//! that feed authoritative for its own window — the same seam [`crate::myir`] uses against
//! the balance-delta task. It's a separate step from parsing because the cutover belongs to
//! the target account, and one upload can be routed to a dozen of them.
//!
//! The substantive transformation is **text repair**. ASB renders each description from
//! fixed-width subfields, trimmed and joined with a space, so a bank account number comes
//! back split after the branch (`12-3136- 0000123-51`) and a name longer than its subfield
//! comes back with a space inside it (`3DPRINTERSTO RENZ` for `3DPRINTERSTORENZ`). That
//! matters because the description is what categorisation rules match on and what a human
//! reads.
//!
//! The account number is repaired unconditionally — the digit shapes either side pin it
//! down. The split name is repaired only where the format actually says so, which is a
//! minority of cases: `MCDONALDS PT CHEVALIER` and `3DPRINTERSTORENZ` are indistinguishable
//! once ASB has written them, and inventing `NZ TRANSPORTAGENCY` is worse than leaving a
//! stray space. [`rejoin_split_field`] documents the three signals that do prove it, and
//! [`AsbTranType::memo_is_card_descriptor`] which transaction types they may be applied to
//! at all.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::str::FromStr;

use chrono::NaiveDate;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use sure_app::ports::{ProviderCategory, ProviderTransaction};
use sure_core::CategoryKind;

/// Ceilings on what an upload may turn into. The HTTP body limit bounds what arrives;
/// these bound what it becomes, which is the part a zip bomb attacks. Seven years of a busy
/// everyday account is ~3,000 rows and a dozen accounts is a few hundred kilobytes, so these
/// are orders of magnitude above any honest export.
mod limits {
    /// CSV files in one zip, and the byte ceilings — shared with the other zip-taking
    /// importers, see [`crate::zipfile`].
    pub use crate::zipfile::ENTRIES;
    /// Transaction rows across the whole upload.
    pub const ROWS: usize = 100_000;
}

/// Beyond this an amount isn't a bank movement, it's bad data. Bounding it here also keeps
/// the minor-unit conversion from overflowing.
const MAX_ABS_MINOR: i64 = 1_000_000_000_000_00;

/// Width of one of ASB's fixed-width description subfields.
const FIELD_WIDTH: usize = 12;

// --------------------------------------------------------------------------------------
// transaction types
// --------------------------------------------------------------------------------------

/// ASB's `Tran Type` column — a closed set, and the thing that decides which of `Payee`
/// and `Memo` carries the counterparty, whether the memo is a card descriptor, and whether
/// the movement is internal. Parsed from text once, here at the file's edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsbTranType {
    /// `DEBIT` — a card or online payment. The merchant is in the **memo**; the payee
    /// column is the literal text "DEBIT".
    Debit,
    /// `EFTPOS` — an in-person card payment. The merchant is in the **payee**; the memo is
    /// the literal text "EFTPOS".
    Eftpos,
    /// `CREDIT` — a miscellaneous inbound credit; the detail is in the memo.
    Credit,
    /// `D/C` — a direct credit (salary, a refund). The payer is in the payee, prefixed
    /// `D/C FROM `.
    DirectCredit,
    /// `D/D` — a direct debit the payee pulls (IRD, ACC).
    DirectDebit,
    /// `A/P` — a standing automatic payment the customer set up.
    AutomaticPayment,
    /// `BILLPAY` — a bill payment to another bank account, ASB's or not.
    BillPay,
    /// `TFR IN` — moved in from another of the customer's own accounts.
    TransferIn,
    /// `TFR OUT` — moved out to another of the customer's own accounts.
    TransferOut,
    /// `ATM` — cash withdrawn or deposited at a machine.
    Atm,
}

impl AsbTranType {
    /// The label ASB writes in the CSV.
    pub fn as_str(self) -> &'static str {
        use AsbTranType::*;
        match self {
            Debit => "DEBIT",
            Eftpos => "EFTPOS",
            Credit => "CREDIT",
            DirectCredit => "D/C",
            DirectDebit => "D/D",
            AutomaticPayment => "A/P",
            BillPay => "BILLPAY",
            TransferIn => "TFR IN",
            TransferOut => "TFR OUT",
            Atm => "ATM",
        }
    }

    /// Whether this type's memo is a card-acquirer descriptor — one 12-char-chunked
    /// merchant field, rather than the Particulars/Code/Reference triple. Selects the
    /// stronger text repair; see [`rejoin_split_field`] for why it must not be applied to
    /// the triple.
    fn memo_is_card_descriptor(self) -> bool {
        use AsbTranType::*;
        match self {
            Debit => true,
            Eftpos | Credit | DirectCredit | DirectDebit | AutomaticPayment | BillPay
            | TransferIn | TransferOut | Atm => false,
        }
    }

    /// The counterparty, from whichever column this type puts it in. `None` where the row
    /// names an account rather than a merchant, or restates the type — passing those
    /// through would mint merchants called "Eftpos" and "Debit", since
    /// `import_transactions` creates a merchant verbatim from whatever it is handed.
    fn merchant(self, payee: &str, memo: &str) -> Option<String> {
        use AsbTranType::*;
        match self {
            Eftpos => as_merchant(clean(payee, false)),
            // The descriptor sits after `CARD 1111 ` or `USD 12.64 `; anything else
            // (`FC12-…` internal payments, the offshore-margin fee) names no merchant.
            Debit => {
                let memo = clean(memo, true);
                let offset = card_prefix_len(&memo).or_else(|| fx_prefix_len(&memo))?;
                let rest: String = memo.chars().skip(offset).collect();
                as_merchant(strip_fx_rate(&rest))
            }
            DirectCredit => {
                let payee = clean(payee, false);
                as_merchant(
                    payee
                        .strip_prefix("D/C FROM ")
                        .unwrap_or(&payee)
                        .to_string(),
                )
            }
            DirectDebit | AutomaticPayment => as_merchant(clean(payee, false)),
            // A transfer names an account, a bill payment names an account number, an ATM
            // names a machine, and `CREDIT` puts nothing usable in either column.
            Credit | BillPay | TransferIn | TransferOut | Atm => None,
        }
    }

    /// The category hint carried into the import, where the type is unambiguous about what
    /// kind of movement it is.
    fn category(self) -> Option<ProviderCategory> {
        use AsbTranType::*;
        match self {
            // Money between the customer's own accounts is a real movement but neither
            // spending nor income, so it must land in neither report.
            TransferIn | TransferOut => Some(ProviderCategory {
                name: "Transfer".to_string(),
                group: None,
                kind: Some(CategoryKind::Transfer),
            }),
            // `BILLPAY` reaches third parties, so it is ordinary spending as often as it
            // is a transfer. None of the rest carry a reliable hint either.
            Debit | Eftpos | Credit | DirectCredit | DirectDebit | AutomaticPayment | BillPay
            | Atm => None,
        }
    }
}

impl FromStr for AsbTranType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use AsbTranType::*;
        // Over a `&str` at the parse boundary, not over an enum: this is the one place an
        // ASB label is text (CLAUDE.md rule 1).
        match s.trim().to_ascii_uppercase().as_str() {
            "DEBIT" => Ok(Debit),
            "EFTPOS" => Ok(Eftpos),
            "CREDIT" => Ok(Credit),
            "D/C" => Ok(DirectCredit),
            "D/D" => Ok(DirectDebit),
            "A/P" => Ok(AutomaticPayment),
            "BILLPAY" => Ok(BillPay),
            "TFR IN" => Ok(TransferIn),
            "TFR OUT" => Ok(TransferOut),
            "ATM" => Ok(Atm),
            other => Err(format!("unrecognised transaction type '{other}'")),
        }
    }
}

// --------------------------------------------------------------------------------------
// text repair
// --------------------------------------------------------------------------------------

/// Payee/memo values that restate the transaction type instead of naming a counterparty.
/// ASB writes one of these in whichever column has nothing to say, so carrying it into the
/// description is noise and carrying it into `merchant` invents a merchant.
fn is_filler(text: &str) -> bool {
    matches!(text.trim(), "" | "DEBIT" | "CREDIT" | "EFTPOS" | "ATM")
}

/// A merchant name, or `None` if the text isn't one. Beyond the filler tokens, ASB writes
/// a bare `0` where it has no counterparty on file (`D/C FROM 0`), so a candidate with no
/// letter in it is a placeholder rather than a name. A row rejected here still imports —
/// it just doesn't create a merchant called "0".
fn as_merchant(text: String) -> Option<String> {
    if is_filler(&text) || !text.chars().any(char::is_alphabetic) {
        None
    } else {
        Some(text)
    }
}

/// Collapse runs of whitespace to one space and trim. ASB leaves the padding of a
/// short subfield behind as a run of spaces.
fn squeeze(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        }
    }
    out
}

/// Deletes the space ASB leaves *inside* a name that overflowed its 12-char subfield —
/// but only where the text after the space cannot be starting a new word, because the
/// format makes the general case unrecoverable.
///
/// ASB trims each subfield and joins them with a single space. So `3DPRINTERSTORENZ` (one
/// word, split at twelve) and `MCDONALDS PT CHEVALIER` (two words, no split) both arrive
/// as twelve characters, one space, and a remainder — the same shape, with the original
/// space simply gone. Guessing costs either way, and it costs more when it guesses to
/// join: welding `NZ TRANSPORT AGENCY` into `NZ TRANSPORTAGENCY` invents a merchant that
/// never existed, and an invented name reads as real in a way a stray space does not.
///
/// So only three signals count, each ruling out "this is a new word":
/// * the remainder starts lowercase and the subfield ends alphanumeric —
///   `Chemist Ware` + `house`;
/// * the remainder starts with punctuation — `AMZN Mktp US` + `*QK4XR7ZM2`;
/// * putting the halves back together completes a domain — `WWW.ALIEXPRE` + `SS.COM`.
///
/// Everything else keeps ASB's spacing. Over a seven-year everyday-account export that is
/// 215 rows repaired and 506 left alone, with nothing invented in either group. Do not
/// "improve" this into a general rule; the information needed is not in the file.
///
/// **Only valid for a card descriptor** regardless. The Particulars/Code/Reference triple
/// every other transaction type uses is three *distinct* 12-char fields, so there the
/// space is real content: applied to a `D/C` memo this would turn `ACME CORP CO IPAYROLL`
/// (particulars `ACME CORP CO`, code `IPAYROLL`) into `ACME CORP COIPAYROLL`.
fn rejoin_split_field(s: &str, offset: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let i = offset + FIELD_WIDTH;
    // A full subfield, a single joining space, and something after it. A space on either
    // side means the subfield was short and this whitespace is padding, not a split.
    let split_here =
        i + 1 < chars.len() && chars[i] == ' ' && chars[i - 1] != ' ' && chars[i + 1] != ' ';
    if !split_here {
        return s.to_string();
    }
    let head: String = chars[..i].iter().collect();
    let tail: String = chars[i + 1..].iter().collect();
    let next = tail.split(' ').next().unwrap_or_default();
    if !is_continuation(&head, next) {
        return s.to_string();
    }
    format!("{head}{tail}")
}

/// Whether `b` cannot be the start of a new word, so the space before it is an artifact of
/// ASB's fixed-width split rather than content. See [`rejoin_split_field`].
fn is_continuation(a: &str, b: &str) -> bool {
    let Some(first) = b.chars().next() else {
        return false;
    };
    if !first.is_alphanumeric() {
        return true;
    }
    if first.is_lowercase() && a.chars().next_back().is_some_and(char::is_alphanumeric) {
        return true;
    }
    // A domain that only appears once the two halves are put back together.
    has_domain(&format!("{a}{b}")) && !has_domain(a)
}

/// Whether the text contains a domain-like ending. No merchant name splits a TLD across a
/// space, so one appearing only after a rejoin proves the rejoin.
fn has_domain(s: &str) -> bool {
    const TLDS: [&str; 5] = [".com", ".co.nz", ".net", ".org", ".nz"];
    let lower = s.to_lowercase();
    TLDS.iter().any(|tld| {
        lower.match_indices(tld).any(|(at, _)| {
            // The TLD has to end the label, not merely appear inside a longer one.
            lower[at + tld.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '.')
        })
    })
}

/// Deletes the space ASB leaves inside a bank account number, which straddles two
/// subfields: `12-3136- 0000123-51` → `12-3136-0000123-51`. Every occurrence, and for
/// every transaction type — unlike [`rejoin_split_field`] this is unambiguous, because the
/// shape either side pins it down.
fn rejoin_account_numbers(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if is_branch(&chars, i) && chars.get(i + 8) == Some(&' ') && is_suffix(&chars, i + 9) {
            out.extend(&chars[i..i + 8]);
            i += 9; // the spurious space
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `12-3136-` — bank, branch, and their separators.
fn is_branch(c: &[char], i: usize) -> bool {
    let Some(w) = c.get(i..i + 8) else {
        return false;
    };
    w[0].is_ascii_digit()
        && w[1].is_ascii_digit()
        && w[2] == '-'
        && w[3..7].iter().all(char::is_ascii_digit)
        && w[7] == '-'
}

/// `0000123-51` — account number and suffix.
fn is_suffix(c: &[char], i: usize) -> bool {
    let Some(w) = c.get(i..i + 10) else {
        return false;
    };
    w[..7].iter().all(char::is_ascii_digit)
        && w[7] == '-'
        && w[8].is_ascii_digit()
        && w[9].is_ascii_digit()
}

/// Length of a `CARD 1111 ` prefix, if the text has one.
fn card_prefix_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("CARD ")?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || rest.chars().nth(digits) != Some(' ') {
        return None;
    }
    Some("CARD ".chars().count() + digits + 1)
}

/// Length of a foreign-currency prefix (`USD 12.64 `), if the text has one.
fn fx_prefix_len(s: &str) -> Option<usize> {
    let mut chars = s.chars();
    for _ in 0..3 {
        if !chars.next()?.is_ascii_uppercase() {
            return None;
        }
    }
    if chars.next()? != ' ' {
        return None;
    }
    // Only ASCII has been consumed, so the char count is also the byte offset.
    let after_ccy = &s[4..];
    let digits = after_ccy
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .count();
    if digits == 0 || after_ccy.chars().nth(digits) != Some(' ') {
        return None;
    }
    Some(4 + digits + 1)
}

/// Drops the trailing ` at 0.6706*` exchange rate from a foreign-currency descriptor,
/// leaving the merchant. Only when the tail really is a rate — ` at ` occurs in merchant
/// names too.
fn strip_fx_rate(s: &str) -> String {
    match s.rsplit_once(" at ") {
        Some((head, tail))
            if !tail.is_empty()
                && tail
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '*') =>
        {
            head.to_string()
        }
        // No rate suffix, or ` at ` is part of the name.
        Some(_) | None => s.to_string(),
    }
}

/// Repairs one column's text. `card_style` enables [`rejoin_split_field`], which is only
/// safe for a card descriptor.
fn clean(text: &str, card_style: bool) -> String {
    let mut out = text.to_string();
    if card_style {
        let offset = card_prefix_len(&out)
            .or_else(|| fx_prefix_len(&out))
            .unwrap_or(0);
        out = rejoin_split_field(&out, offset);
    }
    squeeze(&rejoin_account_numbers(&out))
}

/// The row's description: both text columns, minus whichever restates the type. This is
/// what rules match on, so it deliberately lands close to how the live Akahu feed renders
/// the same transaction (`MB TRANSFER TO 12-3456-0000456-00`).
fn describe(kind: Option<AsbTranType>, payee: &str, memo: &str) -> String {
    let card_style = kind.is_some_and(AsbTranType::memo_is_card_descriptor);
    let payee = if is_filler(payee) {
        String::new()
    } else {
        clean(payee, false)
    };
    let memo = if is_filler(memo) {
        String::new()
    } else {
        clean(memo, card_style)
    };
    let joined = squeeze(&format!("{payee} {memo}"));
    if joined.is_empty() {
        // Both columns were the type restated (an `ATM`/`ATM` row). Say what it was rather
        // than storing a blank description.
        kind.map(AsbTranType::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        joined
    }
}

// --------------------------------------------------------------------------------------
// the export
// --------------------------------------------------------------------------------------

/// Everything one upload contained: a bare CSV, or a zip of them, grouped into one
/// [`AsbExport`] per ASB account named inside. One zip can therefore carry a whole bank's
/// worth of accounts, which is the point — a per-account upload would mean repeating the
/// exercise a dozen times.
#[derive(Debug, Default)]
pub struct AsbUpload {
    /// One per distinct ASB account, ordered by account number so a preview is stable.
    pub exports: Vec<AsbExport>,
    /// Upload-level observations: entries that weren't CSVs, and so on.
    pub warnings: Vec<String>,
}

/// One ASB account's parsed history, plus everything the caller needs to show what an
/// import will do before it commits to one.
#[derive(Debug, Default)]
pub struct AsbExport {
    /// Rows to import. Every parsed row until [`AsbExport::hold_back_from`] withholds the
    /// ones a live feed already owns.
    pub transactions: Vec<ProviderTransaction>,
    /// The account the export is for, as ASB formats it (`12-3136-0000123-50`). Echoed
    /// back so a wrong upload is obvious, and namespaced into every `external_id` so a
    /// mis-targeted one creates visible rows instead of silently colliding.
    pub account: String,
    /// The file(s) this account's rows came from, so a preview can name them. More than
    /// one when a zip holds several windows of the same account.
    pub sources: Vec<String>,
    /// ASB's product name for the account (`Streamline`), when the preamble names one.
    pub product: Option<String>,
    /// The window the *rows* actually cover, which is what the balance reconstruction can
    /// be trusted back to. Not the file's declared range — an export's `To date` runs to
    /// the day it was taken, past the last transaction.
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    /// Rows in the file, before the cutover was applied.
    pub rows_total: i64,
    /// Rows the cutover held back, because a live feed already owns their window.
    pub held_back: i64,
    /// The closing balance ASB states in the preamble, and the day it is as of. The
    /// caller reconciles this against the account's own valuation.
    pub ledger_balance_minor: Option<i64>,
    pub ledger_balance_as_of: Option<String>,
    /// Every row in the file summed, so a caller can state the implied opening balance.
    pub sum_minor: i64,
    /// Non-fatal observations: an unfamiliar transaction type, rows held back, a row
    /// outside the file's declared window.
    pub warnings: Vec<String>,
}

impl AsbExport {
    /// The balance the account must have held immediately before this export's first row,
    /// given the closing balance ASB states and every movement in between. `None` when the
    /// export states no closing balance, since then there is nothing to work back from.
    ///
    /// Computed over *every* row in the file, including any the cutover held back — those
    /// movements still happened, they are just already on the ledger from the live feed.
    pub fn implied_opening_minor(&self) -> Option<i64> {
        self.ledger_balance_minor.map(|b| b - self.sum_minor)
    }

    /// A transaction that puts [`Self::implied_opening_minor`] on the ledger, dated the day
    /// before the first row.
    ///
    /// Without it an imported history starts from nothing: the balance reconstruction reads
    /// an account as 0 before its earliest transaction, so the account appears out of thin
    /// air at whatever its first day's movements leave behind rather than at the balance it
    /// actually held. It also makes the ledger self-consistent — every row plus this one sums
    /// to the balance ASB states.
    ///
    /// A one-off, so it counts towards balances and net worth but never towards income; a
    /// valuation would be the wrong instrument entirely, because the reconstruction returns
    /// the most recent valuation on or before a date *directly*, which would freeze the
    /// account at its opening figure for every date after it.
    pub fn opening_balance_row(&self) -> Option<ProviderTransaction> {
        let amount_minor = self.implied_opening_minor()?;
        let first = NaiveDate::parse_from_str(self.covered_from.as_deref()?, "%Y-%m-%d").ok()?;
        let as_of = first.pred_opt()?;
        Some(ProviderTransaction {
            // Stable, and namespaced like every other row, so re-uploading replaces nothing
            // and the undo takes it away with the rest.
            external_id: format!("asb:{}:opening", self.account),
            posted_at: format!("{}T12:00:00+00:00", iso(as_of)),
            amount_minor,
            currency_code: None,
            description: "Opening balance".to_string(),
            merchant: None,
            category: None,
        })
    }

    /// Withhold every row dated on or after `until`, counting them in
    /// [`AsbExport::held_back`]. `until` is the first date a live feed already posts this
    /// account's movements for; see the module docs for why importing over it is not an
    /// option. Separate from parsing because the cutover belongs to the *target account*,
    /// and one upload can be routed to a dozen of them.
    pub fn hold_back_from(&mut self, until: Option<NaiveDate>) {
        let Some(until) = until else {
            return;
        };
        let cutover = iso(until);
        let before = self.transactions.len();
        // `posted_at` is `YYYY-MM-DDT…`, so a lexical compare on the first ten characters is
        // the date compare, without re-parsing every row.
        self.transactions
            .retain(|t| t.posted_at.get(..10).unwrap_or(&t.posted_at) < cutover.as_str());
        self.held_back = (before - self.transactions.len()) as i64;
        if self.held_back > 0 {
            self.warnings.push(format!(
                "{} row(s) from {cutover} onward were held back: a connected feed already \
                 covers that period, and importing them again would count the same money twice",
                self.held_back
            ));
        }
    }
}

/// Parses an ASB transaction upload: one export CSV, or a zip of them. Told apart by
/// content rather than filename, so a `.zip` that is really a CSV (or the reverse) still
/// works. Several CSVs for the same ASB account are reconciled into one export, so
/// overlapping download windows are free the way they are for a myIR upload.
///
/// Fatal (the upload is not what it claims to be, or is corrupt): no header row, no account
/// line, a missing column, an unreadable date or amount, an amount out of range, a
/// duplicate `Unique Id` within one file, two files disagreeing about the same id, a zip
/// with no CSVs in it, or anything past the [`limits`]. An unfamiliar transaction type is
/// *not* fatal: the row imports on a conservative path and the type is reported through
/// [`AsbExport::warnings`], so a new ASB label can't block a seven-year import.
pub fn parse_upload(bytes: &[u8]) -> anyhow::Result<AsbUpload> {
    let Entries {
        files,
        mut warnings,
    } = read_entries(bytes)?;
    let mut parsed = Vec::new();
    let mut rows = 0usize;
    for entry in files {
        let export = parse_csv(&entry.name, &entry.body)?;
        rows += export.rows_total as usize;
        if rows > limits::ROWS {
            anyhow::bail!(
                "too many rows: this upload holds more than {} transactions",
                limits::ROWS
            );
        }
        parsed.push(export);
    }
    let exports = merge_by_account(parsed)?;
    if exports.len() > 1 {
        warnings.push(format!(
            "the upload holds exports for {} different ASB accounts",
            exports.len()
        ));
    }
    Ok(AsbUpload { exports, warnings })
}

/// One named CSV body out of an upload.
struct Entry {
    name: String,
    body: Vec<u8>,
}

/// The CSV bodies an upload contained, plus what was ignored getting them out.
struct Entries {
    files: Vec<Entry>,
    warnings: Vec<String>,
}

/// The CSV bodies in an upload. A zip yields its `.csv` entries; anything else is treated as
/// a single CSV.
fn read_entries(bytes: &[u8]) -> anyhow::Result<Entries> {
    // Local file header magic. A zip's own trailer could be found by seeking, but a CSV
    // never starts with this and that is all this needs to decide.
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(Entries {
            files: vec![Entry {
                name: "export.csv".to_string(),
                body: bytes.to_vec(),
            }],
            warnings: Vec::new(),
        });
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("upload looks like a .zip but could not be read: {e}"))?;
    let mut names = Vec::new();
    let mut skipped = 0usize;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else {
            continue;
        };
        let name = entry.name().to_string();
        // Skip macOS's resource-fork shadows, which zipping a Finder selection includes.
        let hidden = name.starts_with("__MACOSX/")
            || name.rsplit('/').next().is_some_and(|f| f.starts_with('.'));
        if entry.is_dir() || hidden {
            continue;
        }
        if name.to_lowercase().ends_with(".csv") {
            names.push(name);
        } else {
            skipped += 1;
        }
    }

    if names.is_empty() {
        anyhow::bail!("the zip holds no .csv files");
    }
    if names.len() > limits::ENTRIES {
        anyhow::bail!(
            "the zip holds {} .csv files; at most {} are read at once",
            names.len(),
            limits::ENTRIES
        );
    }

    let mut out = Vec::new();
    let mut budget = crate::zipfile::Budget::default();
    for name in names {
        let mut entry = archive.by_name(&name)?;
        let declared = entry.size();
        let body = budget.read(&name, declared, &mut entry)?;
        drop(entry);
        out.push(Entry { name, body });
    }

    let mut warnings = Vec::new();
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} file(s) in the zip weren't .csv exports and were ignored"
        ));
    }
    Ok(Entries {
        files: out,
        warnings,
    })
}

/// Reconcile the parsed files into one export per ASB account. Two files covering
/// overlapping windows of one account are merged; a row present in both is kept once.
fn merge_by_account(parsed: Vec<AsbExport>) -> anyhow::Result<Vec<AsbExport>> {
    let mut by_account: Vec<AsbExport> = Vec::new();
    for export in parsed {
        match by_account.iter_mut().find(|e| e.account == export.account) {
            Some(into) => absorb(into, export)?,
            None => by_account.push(export),
        }
    }
    for export in &mut by_account {
        // Chronological, so a preview and the insert order both read naturally.
        export.transactions.sort_by(|a, b| {
            a.posted_at
                .cmp(&b.posted_at)
                .then(a.external_id.cmp(&b.external_id))
        });
        let dates: Vec<&str> = export
            .transactions
            .iter()
            .filter_map(|t| t.posted_at.get(..10))
            .collect();
        export.covered_from = dates.first().map(|d| d.to_string());
        export.covered_to = dates.last().map(|d| d.to_string());
    }
    by_account.sort_by(|a, b| a.account.cmp(&b.account));
    Ok(by_account)
}

/// Fold `from` into `into`, both being exports of the same ASB account.
fn absorb(into: &mut AsbExport, from: AsbExport) -> anyhow::Result<()> {
    let existing: HashMap<&str, (&str, i64)> = into
        .transactions
        .iter()
        .map(|t| {
            (
                t.external_id.as_str(),
                (t.posted_at.as_str(), t.amount_minor),
            )
        })
        .collect();
    let mut fresh = Vec::new();
    let mut duplicates = 0i64;
    for row in &from.transactions {
        match existing.get(row.external_id.as_str()) {
            // The same row in two overlapping windows.
            Some(&(posted_at, amount_minor))
                if posted_at == row.posted_at && amount_minor == row.amount_minor =>
            {
                duplicates += 1;
            }
            // The same id describing different money. The import is `INSERT OR IGNORE` with
            // no update-on-conflict, so a restated row would land *alongside* the stale one
            // and double-count. Refuse rather than pick one.
            Some(_) => anyhow::bail!(
                "{} and {} disagree about transaction {}: one upload cannot hold two \
                 versions of the same row",
                into.sources.join(", "),
                from.sources.join(", "),
                row.external_id
            ),
            None => fresh.push(row.external_id.clone()),
        }
    }
    let keep: HashSet<String> = fresh.into_iter().collect();
    let mut moved: Vec<ProviderTransaction> = from
        .transactions
        .into_iter()
        .filter(|t| keep.contains(&t.external_id))
        .collect();

    into.rows_total += from.rows_total - duplicates;
    into.sum_minor += moved.iter().map(|t| t.amount_minor).sum::<i64>();
    into.transactions.append(&mut moved);
    into.sources.extend(from.sources);
    into.product = into.product.take().or(from.product);
    into.warnings.extend(from.warnings);
    // The later statement is the authoritative closing balance.
    if from.ledger_balance_as_of > into.ledger_balance_as_of {
        into.ledger_balance_minor = from.ledger_balance_minor;
        into.ledger_balance_as_of = from.ledger_balance_as_of;
    }
    Ok(())
}

/// Parses one ASB export CSV. `source` names it in any error or warning.
fn parse_csv(source: &str, bytes: &[u8]) -> anyhow::Result<AsbExport> {
    let text = decode(bytes);
    let (preamble, body) = split_at_header(&text)?;
    let preamble = parse_preamble(preamble)?;

    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_reader(body.as_bytes());

    let headers = reader.headers()?.clone();
    let col = |name: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(name));
    let need = |name: &'static str| {
        col(name).ok_or_else(|| {
            anyhow::anyhow!("{source} is not an ASB export: missing '{name}' column")
        })
    };
    let (i_date, i_id, i_type, i_payee, i_memo, i_amount) = (
        need("Date")?,
        need("Unique Id")?,
        need("Tran Type")?,
        need("Payee")?,
        need("Memo")?,
        need("Amount")?,
    );

    let mut out = AsbExport {
        account: preamble.account,
        product: preamble.product,
        sources: vec![source.to_string()],
        ..AsbExport::default()
    };
    if let Some((minor, as_of)) = preamble.ledger {
        out.ledger_balance_minor = Some(minor);
        out.ledger_balance_as_of = Some(iso(as_of));
    } else {
        out.warnings.push(
            "the export states no ledger balance, so it can't be reconciled against the \
             account's own balance"
                .to_string(),
        );
    }

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut unknown_types: BTreeMap<String, i64> = BTreeMap::new();
    let mut outside_window = 0i64;
    let (mut first, mut last): (Option<NaiveDate>, Option<NaiveDate>) = (None, None);

    for record in reader.records() {
        let record = record?;
        if record.iter().all(|f| f.trim().is_empty()) {
            continue; // the blank line ASB writes between the header and the rows
        }
        // +1 for the header, +1 for 1-based. Named, because one zip can hold a dozen files
        // and "row 412" alone doesn't say which.
        let at = format!("{source} row {}", out.rows_total as usize + 2);
        let field = |i: usize, name: &str| -> anyhow::Result<&str> {
            record
                .get(i)
                .ok_or_else(|| anyhow::anyhow!("{at}: truncated, no '{name}' field"))
        };

        let date = parse_date(field(i_date, "Date")?, &at)?;
        let amount_minor = parse_minor(field(i_amount, "Amount")?, &at)?;
        let unique_id = field(i_id, "Unique Id")?.to_string();
        if unique_id.is_empty() {
            anyhow::bail!("{at}: empty 'Unique Id' — imported rows dedupe on it");
        }
        // Duplicated ids would collapse into one row under `INSERT OR IGNORE`, losing
        // money silently. Better to reject the file.
        if !seen_ids.insert(unique_id.clone()) {
            anyhow::bail!("{at}: 'Unique Id' {unique_id} appears twice in this export");
        }

        out.rows_total += 1;
        out.sum_minor += amount_minor;
        first = Some(first.map_or(date, |d: NaiveDate| d.min(date)));
        last = Some(last.map_or(date, |d: NaiveDate| d.max(date)));
        if preamble.declared.is_some_and(|(f, t)| date < f || date > t) {
            outside_window += 1;
        }

        let kind = match AsbTranType::from_str(field(i_type, "Tran Type")?) {
            Ok(k) => Some(k),
            Err(_) => {
                *unknown_types
                    .entry(field(i_type, "Tran Type")?.to_string())
                    .or_default() += 1;
                None
            }
        };

        let payee = field(i_payee, "Payee")?;
        let memo = field(i_memo, "Memo")?;
        out.transactions.push(ProviderTransaction {
            external_id: format!("asb:{}:{}", out.account, unique_id),
            // Midday UTC, matching every other `posted_at` this app writes, so an imported
            // row sorts sensibly against a feed's row on the same day.
            posted_at: format!("{}T12:00:00+00:00", iso(date)),
            amount_minor,
            // The export names no currency; the account's own applies.
            currency_code: None,
            description: describe(kind, payee, memo),
            merchant: kind.and_then(|k| k.merchant(payee, memo)),
            category: kind.and_then(AsbTranType::category),
        });
    }

    out.covered_from = first.map(iso);
    out.covered_to = last.map(iso);

    for (label, count) in unknown_types {
        out.warnings.push(format!(
            "{count} row(s) have an unfamiliar transaction type '{label}' — imported \
             without a merchant or category"
        ));
    }
    if outside_window > 0 {
        out.warnings.push(format!(
            "{outside_window} row(s) fall outside the date range the export declares"
        ));
    }
    Ok(out)
}

/// Lossy on purpose: one odd byte in a merchant name must not sink a seven-year import.
fn decode(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
}

/// Splits the preamble from the CSV proper. Found by scanning for the header row rather
/// than skipping a fixed number of lines, so a change in how many facts ASB states above
/// it doesn't break the parse.
fn split_at_header(text: &str) -> anyhow::Result<(&str, &str)> {
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let cols: Vec<String> = line
            .split(',')
            .map(|c| c.trim().to_ascii_lowercase())
            .collect();
        if cols.iter().any(|c| c == "date") && cols.iter().any(|c| c == "unique id") {
            return Ok((&text[..offset], &text[offset..]));
        }
        offset += line.len();
    }
    Err(anyhow::anyhow!(
        "not an ASB export: no 'Date,Unique Id,Tran Type,…' header row"
    ))
}

/// The facts ASB states above the header.
#[derive(Debug, Default)]
struct Preamble {
    account: String,
    product: Option<String>,
    /// The range the export declares it covers.
    declared: Option<(NaiveDate, NaiveDate)>,
    /// Closing balance and the day it is as of.
    ledger: Option<(i64, NaiveDate)>,
}

fn parse_preamble(text: &str) -> anyhow::Result<Preamble> {
    let mut out = Preamble::default();
    let (mut from, mut to) = (None, None);
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("From date ") {
            from = NaiveDate::parse_from_str(rest.trim(), "%Y%m%d").ok();
        } else if let Some(rest) = line.strip_prefix("To date ") {
            to = NaiveDate::parse_from_str(rest.trim(), "%Y%m%d").ok();
        } else if let Some(rest) = line.strip_prefix("Ledger Balance") {
            out.ledger = parse_balance(rest);
        } else if line.starts_with("Bank ") {
            let (account, product) = parse_account_line(line)?;
            out.account = account;
            out.product = product;
        }
    }
    if out.account.is_empty() {
        return Err(anyhow::anyhow!(
            "not an ASB export: no 'Bank …; Branch …; Account …' line naming the account"
        ));
    }
    out.declared = from.zip(to);
    Ok(out)
}

/// `Bank 12; Branch 3136; Account 0000123-50 (Streamline)` → `12-3136-0000123-50`,
/// `Streamline`.
fn parse_account_line(line: &str) -> anyhow::Result<(String, Option<String>)> {
    let malformed = || anyhow::anyhow!("cannot read the account from '{line}'");
    let mut bank = None;
    let mut branch = None;
    let mut number = None;
    let mut product = None;
    for part in line.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Bank ") {
            bank = Some(v.trim().to_string());
        } else if let Some(v) = part.strip_prefix("Branch ") {
            branch = Some(v.trim().to_string());
        } else if let Some(v) = part.strip_prefix("Account ") {
            let v = v.trim();
            // The product name trails in parentheses.
            match v.split_once('(') {
                Some((num, prod)) => {
                    number = Some(num.trim().to_string());
                    product = Some(prod.trim_end_matches(')').trim().to_string());
                }
                None => number = Some(v.to_string()),
            }
        }
    }
    let (bank, branch, number) = (
        bank.ok_or_else(malformed)?,
        branch.ok_or_else(malformed)?,
        number.ok_or_else(malformed)?,
    );
    if bank.is_empty() || branch.is_empty() || number.is_empty() {
        return Err(malformed());
    }
    Ok((
        format!("{bank}-{branch}-{number}"),
        product.filter(|p| !p.is_empty()),
    ))
}

/// ` : 100.00 as of 20260803` → the amount in minor units and the day.
fn parse_balance(rest: &str) -> Option<(i64, NaiveDate)> {
    let rest = rest.trim().trim_start_matches(':').trim();
    let (amount, as_of) = rest.split_once(" as of ")?;
    let minor = parse_minor(amount.trim(), "ledger balance").ok()?;
    let as_of = NaiveDate::parse_from_str(as_of.trim(), "%Y%m%d").ok()?;
    Some((minor, as_of))
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// ASB's export offers a choice of date format; this parser wants the ISO-ordered one. A
/// day/month-first file fails here rather than being silently misread, because
/// `%Y/%m/%d` cannot make sense of `03/08/2026`.
fn parse_date(text: &str, at: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y/%m/%d").map_err(|_| {
        anyhow::anyhow!(
            "{at}: cannot read '{text}' as a date — re-export with the YYYY/MM/DD date format"
        )
    })
}

/// Exact 2-dp minor units. `Decimal`, not float — `329.36` must not land as `32935`.
/// Every step is checked: an uploaded file is arbitrary input, and `Decimal`'s multiply
/// panics on overflow rather than returning an error.
fn parse_minor(text: &str, at: &str) -> anyhow::Result<i64> {
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | ' '))
        .collect();
    let value: Decimal = cleaned
        .parse()
        .map_err(|_| anyhow::anyhow!("{at}: cannot read '{text}' as an amount"))?;
    let out_of_range = || anyhow::anyhow!("{at}: amount '{text}' is out of range");
    let minor = value
        .checked_mul(Decimal::from(100))
        .ok_or_else(out_of_range)?
        .round()
        .to_i64()
        .ok_or_else(out_of_range)?;
    if !(-MAX_ABS_MINOR..=MAX_ABS_MINOR).contains(&minor) {
        return Err(out_of_range());
    }
    Ok(minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREAMBLE: &str = "Created date / time : 03 August 2026 / 16:27:53\r\n\
         Bank 12; Branch 3136; Account 0000123-50 (Streamline)\r\n\
         From date 20200101\r\n\
         To date 20260803\r\n\
         Avail Bal : 100.00 as of 20260727\r\n\
         Ledger Balance : 100.00 as of 20260803\r\n\
         Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n\r\n";

    /// The single export a well-formed upload of one file yields.
    fn export(rows: &[&str]) -> AsbExport {
        only(parse_upload(file(rows).as_bytes()).expect("parses"))
    }

    fn only(mut upload: AsbUpload) -> AsbExport {
        assert_eq!(upload.exports.len(), 1, "expected one account's export");
        upload.exports.pop().unwrap()
    }

    fn file(rows: &[&str]) -> String {
        file_for("0000123-50", "100.00", "20260803", rows)
    }

    /// A well-formed export for an arbitrary account, so a multi-account zip can be built.
    fn file_for(account: &str, ledger: &str, as_of: &str, rows: &[&str]) -> String {
        let preamble = format!(
            "Created date / time : 03 August 2026 / 16:27:53\r\n\
             Bank 12; Branch 3136; Account {account} (Streamline)\r\n\
             From date 20200101\r\n\
             To date 20260803\r\n\
             Avail Bal : {ledger} as of {as_of}\r\n\
             Ledger Balance : {ledger} as of {as_of}\r\n\
             Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n\r\n"
        );
        format!("{preamble}{}\r\n", rows.join("\r\n"))
    }

    /// A minimal stored (uncompressed) zip — enough for the parser, and dependency-free.
    fn zip(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, body) in entries {
            let crc = crc32(body.as_bytes());
            let (n, len) = (name.as_bytes(), body.len() as u32);
            let offset = out.len() as u32;
            let mut header = Vec::new();
            header.extend_from_slice(&0u16.to_le_bytes()); // version
            header.extend_from_slice(&0u16.to_le_bytes()); // flags
            header.extend_from_slice(&0u16.to_le_bytes()); // stored
            header.extend_from_slice(&[0; 4]); // time, date
            header.extend_from_slice(&crc.to_le_bytes());
            header.extend_from_slice(&len.to_le_bytes());
            header.extend_from_slice(&len.to_le_bytes());
            header.extend_from_slice(&(n.len() as u16).to_le_bytes());
            header.extend_from_slice(&0u16.to_le_bytes()); // extra len

            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&header);
            out.extend_from_slice(n);
            out.extend_from_slice(body.as_bytes());

            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes()); // made by
            central.extend_from_slice(&header);
            central.extend_from_slice(&[0; 6]); // comment len, disk, attrs
            central.extend_from_slice(&[0; 4]); // external attrs
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(n);
        }
        let (start, size) = (out.len() as u32, central.len() as u32);
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]); // disk numbers
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in data {
            crc ^= *byte as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
            }
        }
        !crc
    }

    fn one(row: &str) -> ProviderTransaction {
        export(&[row]).transactions.pop().expect("one row")
    }

    // ---------------------------------------------------------------- text repair

    #[test]
    fn rejoins_a_split_name_where_the_remainder_cannot_start_a_word() {
        for (memo, want) in [
            // Lowercase remainder.
            ("OffshoreServ iceMargins**", "OffshoreServiceMargins**"),
            (
                "CARD 1111 Chemist Ware house Albany",
                "CARD 1111 Chemist Warehouse Albany",
            ),
            (
                "CARD 1111 Pak N Save Q ueenstown Frankton",
                "CARD 1111 Pak N Save Queenstown Frankton",
            ),
            (
                "CARD 1111 www.newegg.c om City of Indu",
                "CARD 1111 www.newegg.com City of Indu",
            ),
            // Punctuation remainder.
            (
                "CARD 1111 AMZN Mktp US *QK4XR7ZM2 Amzn.com/bil",
                "CARD 1111 AMZN Mktp US*QK4XR7ZM2 Amzn.com/bil",
            ),
            (
                "CARD 1111 DIGITALOCEAN .COM AMSTERDAM",
                "CARD 1111 DIGITALOCEAN.COM AMSTERDAM",
            ),
            // Only the rejoin completes a domain.
            (
                "USD 12.64 WWW.ALIEXPRE SS.COM at 0.6706*",
                "USD 12.64 WWW.ALIEXPRESS.COM at 0.6706*",
            ),
            (
                "CARD 1111 STEAMGAMES.C OM 425-952-2985",
                "CARD 1111 STEAMGAMES.COM 425-952-2985",
            ),
            (
                "CARD 1111 UBER TRIP HE LP.UBER.COM Vorden",
                "CARD 1111 UBER TRIP HELP.UBER.COM Vorden",
            ),
        ] {
            assert_eq!(clean(memo, true), want, "memo {memo:?}");
        }
    }

    /// The whole reason [`rejoin_split_field`] is narrow: ASB writes a real space at
    /// position twelve and a split word at position twelve identically, so an all-caps
    /// remainder that could be its own word is left exactly as it arrived. Welding
    /// `NZ TRANSPORT AGENCY` shut would invent a merchant.
    #[test]
    fn leaves_an_ambiguous_split_exactly_as_asb_wrote_it() {
        for memo in [
            "CARD 1111 MCDONALDS PT CHEVALIER AUCKLAND",
            "CARD 1111 NZ TRANSPORT AGENCY PALMERSTON N",
            "CARD 1111 CHANCERY CAR PARK AUCKLAND",
            "CARD 1111 PARADICE ICE SKATING BOTANY DOWNS",
            "CARD 1111 SUPER LIQUOR ALBANY AUCKLAND",
            // Genuinely a split word — but indistinguishable from the five above, so it
            // stays split too. That is the deliberate trade.
            "CARD 1111 3DPRINTERSTO RENZ AUCKLAND",
            "CARD 1111 TWL 119 ALBA NY ALBANY",
        ] {
            assert_eq!(clean(memo, true), memo, "memo {memo:?}");
        }
    }

    #[test]
    fn leaves_a_short_subfields_padding_alone() {
        // The subfield was shorter than 12, so the run of spaces is padding, not a split.
        for (memo, want) in [
            (
                "CARD 1111 KMART ALBANY  AUCKLAND",
                "CARD 1111 KMART ALBANY AUCKLAND",
            ),
            ("CARD 1111 Z ALBANY  ALBANY", "CARD 1111 Z ALBANY ALBANY"),
            ("CARD 1111 SKINNY  AUCKLAND", "CARD 1111 SKINNY AUCKLAND"),
        ] {
            assert_eq!(clean(memo, true), want, "memo {memo:?}");
        }
    }

    /// The regression that scopes [`rejoin_split_field`] to card descriptors: for every
    /// other type the 12-char fields are Particulars/Code/Reference and the space is real.
    #[test]
    fn never_joins_the_particulars_code_reference_triple() {
        assert_eq!(
            clean("ACME CORP CO IPAYROLL 10000-0001", false),
            "ACME CORP CO IPAYROLL 10000-0001"
        );
        assert_eq!(
            describe(
                Some(AsbTranType::DirectCredit),
                "D/C FROM ACME CORP CONSULTING",
                "ACME CORP CO IPAYROLL 10000-0001"
            ),
            "D/C FROM ACME CORP CONSULTING ACME CORP CO IPAYROLL 10000-0001"
        );
    }

    #[test]
    fn rejoins_a_split_account_number_for_every_type() {
        assert_eq!(
            clean("TO 12-3136- 0000123-51 SavingsTrans", false),
            "TO 12-3136-0000123-51 SavingsTrans"
        );
        assert_eq!(
            clean("EX 12-3136- 0000123-51 gas", false),
            "EX 12-3136-0000123-51 gas"
        );
        // Already whole, and mid-string digits must not be mistaken for a split.
        assert_eq!(
            clean("FC12-3456-0000002-00 HolidayRef", false),
            "FC12-3456-0000002-00 HolidayRef"
        );
    }

    #[test]
    fn strips_an_exchange_rate_but_not_a_merchants_own_at() {
        assert_eq!(
            strip_fx_rate("WWW.ALIEXPRESS.COM at 0.6706*"),
            "WWW.ALIEXPRESS.COM"
        );
        assert_eq!(strip_fx_rate("DINNER at THE SHED"), "DINNER at THE SHED");
    }

    // ---------------------------------------------------------------- per-type mapping

    #[test]
    fn takes_the_merchant_from_the_column_each_type_puts_it_in() {
        let eftpos =
            one(r#"2020/01/20,2020012001,EFTPOS,,"MCDONALDS ALBANY F/CRT ALBANY","EFTPOS",-5.00"#);
        assert_eq!(eftpos.description, "MCDONALDS ALBANY F/CRT ALBANY");
        assert_eq!(
            eftpos.merchant.as_deref(),
            Some("MCDONALDS ALBANY F/CRT ALBANY")
        );

        let debit = one(
            r#"2020/01/13,2020011302,DEBIT,,"DEBIT","CARD 1111 Chemist Ware house Albany",-118.22"#,
        );
        assert_eq!(debit.description, "CARD 1111 Chemist Warehouse Albany");
        assert_eq!(debit.merchant.as_deref(), Some("Chemist Warehouse Albany"));

        let fx = one(
            r#"2020/01/02,2020010201,DEBIT,,"DEBIT","USD 12.64 WWW.ALIEXPRE SS.COM at 0.6706*",-18.85"#,
        );
        assert_eq!(fx.merchant.as_deref(), Some("WWW.ALIEXPRESS.COM"));

        let dc = one(
            r#"2021/03/01,2021030101,D/C,,"D/C FROM ACME CORP CONSULTING","ACME CORP CO IPAYROLL 10000-0001",1500.00"#,
        );
        assert_eq!(dc.merchant.as_deref(), Some("ACME CORP CONSULTING"));

        let ap = one(r#"2020/02/01,2020020101,A/P,,"Samplepay7","A/P SamplePower",-36.07"#);
        assert_eq!(ap.merchant.as_deref(), Some("Samplepay7"));

        let dd = one(
            r#"2021/04/01,2021040101,D/D,,"INLAND REVENUE DEPT","D/D SLS 012345678SLS 100000001",-1342.00"#,
        );
        assert_eq!(dd.merchant.as_deref(), Some("INLAND REVENUE DEPT"));
    }

    #[test]
    fn claims_no_merchant_where_the_row_names_an_account_or_restates_the_type() {
        for row in [
            r#"2020/01/01,2020010101,TFR OUT,,"MB TRANSFER","TO 12-3136- 0000123-51 init",-15694.18"#,
            r#"2020/01/22,2020012204,CREDIT,,"CREDIT","From MR X Y ZEDD AND pat sam",152.26"#,
            r#"2020/03/02,2020030201,BILLPAY,,"PMT TO FC02-1234-0000001-00","BILL PAYMENT TO examplename  reference",-45.00"#,
            r#"2020/04/01,2020040101,ATM,,"WITHDRAWAL","ATM",-100.00"#,
            r#"2020/05/01,2020050101,DEBIT,,"DEBIT","FC12-3456-0000002-00 HolidayRef",-100.00"#,
        ] {
            assert_eq!(one(row).merchant, None, "row {row}");
        }
    }

    /// ASB writes `D/C FROM 0` when it has no payer on file. Stripping the prefix leaves
    /// `0`, which must not become a merchant.
    #[test]
    fn a_placeholder_counterparty_is_not_a_merchant() {
        let out = one(
            r#"2024/01/22,2024012201,D/C,,"D/C FROM 0","Countdown OL /80 Favona R Favona",-22.00"#,
        );
        assert_eq!(out.merchant, None);
        assert_eq!(
            out.description,
            "D/C FROM 0 Countdown OL /80 Favona R Favona"
        );
    }

    #[test]
    fn describes_a_row_whose_both_columns_restate_the_type() {
        assert_eq!(
            one(r#"2020/04/01,2020040101,ATM,,"WITHDRAWAL","ATM",-100.00"#).description,
            "WITHDRAWAL"
        );
        assert_eq!(
            one(r#"2020/04/01,2020040101,ATM,,"ATM","ATM",-100.00"#).description,
            "ATM"
        );
    }

    #[test]
    fn marks_only_own_account_transfers_as_transfers() {
        let out = one(
            r#"2020/01/01,2020010101,TFR OUT,,"MB TRANSFER","TO 12-3136- 0000123-51 init",-15694.18"#,
        );
        let category = out.category.expect("a transfer category");
        assert_eq!(category.kind, Some(CategoryKind::Transfer));
        assert_eq!(category.name, "Transfer");
        assert_eq!(out.description, "MB TRANSFER TO 12-3136-0000123-51 init");

        // A bill payment reaches a third party, so it is spending as often as a transfer.
        assert!(one(r#"2020/03/02,2020030201,BILLPAY,,"PMT TO FC02-1234-0000001-00","BILL PAYMENT TO examplename  reference",-45.00"#)
            .category
            .is_none());
    }

    // ---------------------------------------------------------------- rows and amounts

    #[test]
    fn reads_amounts_exactly_and_keeps_their_sign() {
        assert_eq!(
            one(
                r#"2020/01/01,2020010101,TFR OUT,,"MB TRANSFER","TO 12-3136- 0000123-51",-15694.18"#
            )
            .amount_minor,
            -15_694_18
        );
        assert_eq!(
            one(r#"2021/03/01,2021030101,D/C,,"D/C FROM X","Y",329.36"#).amount_minor,
            329_36
        );
    }

    #[test]
    fn dates_a_row_at_midday_and_namespaces_its_id_on_the_account() {
        let out = one(r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#);
        assert_eq!(out.posted_at, "2020-01-20T12:00:00+00:00");
        assert_eq!(out.external_id, "asb:12-3136-0000123-50:2020012001");
        assert_eq!(out.currency_code, None);
    }

    #[test]
    fn reads_the_preamble_and_the_window_the_rows_actually_cover() {
        let out = export(&[
            r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#,
            r#"2026/07/27,2026072701,TFR IN,,"ATM DEPOSIT","palms CARD 2222",7.80"#,
        ]);
        assert_eq!(out.account, "12-3136-0000123-50");
        assert_eq!(out.product.as_deref(), Some("Streamline"));
        assert_eq!(out.ledger_balance_minor, Some(100_00));
        assert_eq!(out.ledger_balance_as_of.as_deref(), Some("2026-08-03"));
        // The rows' own range, not the export's `To date` of 2026-08-03.
        assert_eq!(out.covered_from.as_deref(), Some("2020-01-20"));
        assert_eq!(out.covered_to.as_deref(), Some("2026-07-27"));
        assert_eq!(out.rows_total, 2);
        assert_eq!(out.sum_minor, -5_00 + 7_80);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn an_account_with_no_product_name_still_parses() {
        let text = file(&[r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#])
            .replace(" (Streamline)", "");
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.account, "12-3136-0000123-50");
        assert_eq!(out.product, None);
    }

    // ---------------------------------------------------------------- opening balance

    /// The arithmetic the whole thing rests on: closing balance, less every movement in the
    /// file, is what the account held before the first row.
    #[test]
    fn works_the_opening_balance_back_from_the_closing_one() {
        let out = export(&[
            // 18,694.18 opening, two big transfers out, leaves 1,000.00 …
            r#"2020/01/01,2020010101,TFR OUT,,"MB TRANSFER","TO 12-3136- 0000123-51",-15694.18"#,
            r#"2020/01/01,2020010102,TFR OUT,,"MB TRANSFER","TO 12-3136- 0000123-51",-2000.00"#,
            // … and 900.00 of later spending leaves the 100.00 the preamble states.
            r#"2021/05/05,2021050501,EFTPOS,,"SHOP","EFTPOS",-900.00"#,
        ]);
        assert_eq!(out.ledger_balance_minor, Some(100_00));
        assert_eq!(out.sum_minor, -18_594_18);
        assert_eq!(out.implied_opening_minor(), Some(18_694_18));

        let row = out.opening_balance_row().expect("a row");
        assert_eq!(row.amount_minor, 18_694_18);
        // The day before the first row, so the account is 0 before it and correct from it on.
        assert_eq!(row.posted_at, "2019-12-31T12:00:00+00:00");
        assert_eq!(row.external_id, "asb:12-3136-0000123-50:opening");
        assert_eq!(row.description, "Opening balance");
        assert!(
            row.category.is_none(),
            "an opening balance is not spend or income"
        );
    }

    /// It has to be worked out over the whole file, not just the rows being imported — the
    /// held-back ones happened too, they are simply already on the ledger from the feed.
    #[test]
    fn the_opening_balance_counts_held_back_rows_too() {
        let rows = [
            r#"2020/01/01,2020010101,EFTPOS,,"EARLY","EFTPOS",-40.00"#,
            r#"2025/09/09,2025090901,EFTPOS,,"LATE","EFTPOS",-60.00"#,
        ];
        let mut out = export(&rows);
        out.hold_back_from(Some(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()));
        assert_eq!(out.transactions.len(), 1);
        assert_eq!(out.held_back, 1);
        // 100.00 closing + 100.00 spent = 200.00 opening, both rows counted.
        assert_eq!(out.implied_opening_minor(), Some(200_00));
        assert_eq!(out.opening_balance_row().unwrap().amount_minor, 200_00);
    }

    #[test]
    fn no_closing_balance_means_no_opening_balance() {
        let text = file(&[r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#])
            .replace("Ledger Balance : 100.00 as of 20260803\r\n", "");
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.implied_opening_minor(), None);
        assert!(out.opening_balance_row().is_none());
    }

    #[test]
    fn a_file_with_no_rows_has_no_opening_balance_to_place() {
        let out = only(parse_upload(PREAMBLE.as_bytes()).expect("parses"));
        assert!(out.opening_balance_row().is_none());
    }

    // ---------------------------------------------------------------- the cutover

    #[test]
    fn holds_back_rows_the_cutover_covers_and_says_so() {
        let rows = [
            r#"2025/08/01,2025080101,EFTPOS,,"BEFORE","EFTPOS",-1.00"#,
            r#"2025/08/03,2025080301,EFTPOS,,"ON","EFTPOS",-2.00"#,
            r#"2025/08/04,2025080401,EFTPOS,,"AFTER","EFTPOS",-3.00"#,
        ];
        let mut out = export(&rows);
        out.hold_back_from(Some(NaiveDate::from_ymd_opt(2025, 8, 3).unwrap()));

        assert_eq!(out.transactions.len(), 1);
        assert_eq!(out.transactions[0].description, "BEFORE");
        assert_eq!(out.held_back, 2);
        // The whole file is still described, so a preview can show what was left out.
        assert_eq!(out.rows_total, 3);
        assert_eq!(out.sum_minor, -6_00);
        assert_eq!(out.covered_to.as_deref(), Some("2025-08-04"));
        assert!(
            out.warnings.iter().any(|w| w.contains("held back")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn imports_everything_when_no_feed_owns_a_window() {
        let out = export(&[r#"2026/07/27,2026072701,TFR IN,,"ATM DEPOSIT","palms",7.80"#]);
        assert_eq!(out.transactions.len(), 1);
        assert_eq!(out.held_back, 0);
    }

    // ---------------------------------------------------------------- tolerated

    #[test]
    fn an_unfamiliar_transaction_type_warns_but_still_imports() {
        let out = export(&[r#"2024/06/01,2024060101,INT,,"INTEREST","CREDIT INTEREST",1.23"#]);
        assert_eq!(out.transactions.len(), 1);
        let row = &out.transactions[0];
        assert_eq!(row.amount_minor, 1_23);
        assert_eq!(row.description, "INTEREST CREDIT INTEREST");
        assert_eq!(row.merchant, None);
        assert!(row.category.is_none());
        assert!(
            out.warnings.iter().any(|w| w.contains("'INT'")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_row_outside_the_declared_window_warns() {
        let out = export(&[r#"2019/12/31,2019123101,EFTPOS,,"EARLY","EFTPOS",-1.00"#]);
        assert_eq!(out.transactions.len(), 1);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("outside the date range")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_missing_ledger_balance_warns_rather_than_failing() {
        let text = file(&[r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#])
            .replace("Ledger Balance : 100.00 as of 20260803\r\n", "");
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.ledger_balance_minor, None);
        assert!(
            out.warnings.iter().any(|w| w.contains("no ledger balance")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn extra_preamble_lines_do_not_break_the_header_scan() {
        let text = format!(
            "Something ASB Added Later\r\n{}",
            file(&[r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#])
        );
        assert_eq!(
            only(parse_upload(text.as_bytes()).expect("parses")).rows_total,
            1
        );
    }

    #[test]
    fn a_file_with_no_rows_parses_to_nothing() {
        let out = only(parse_upload(PREAMBLE.as_bytes()).expect("parses"));
        assert_eq!(out.rows_total, 0);
        assert!(out.transactions.is_empty());
        assert_eq!(out.covered_from, None);
    }

    // ---------------------------------------------------------------- rejected

    /// Each of these has to come back as an error naming the problem, never a panic: the
    /// endpoint takes an arbitrary uploaded file, so "the parser panicked" and "the
    /// request failed" are failures of the same kind.
    #[test]
    fn malformed_files_are_rejected_with_a_reason() {
        let cases: [(&str, String, &str); 9] = [
            ("empty", String::new(), "header row"),
            (
                "not a csv at all",
                "<html><body>nope</body></html>".to_string(),
                "header row",
            ),
            (
                "no account line",
                file(&[r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#]).replace(
                    "Bank 12; Branch 3136; Account 0000123-50 (Streamline)\r\n",
                    "",
                ),
                "naming the account",
            ),
            (
                "a renamed column",
                file(&[r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#])
                    .replace("Unique Id", "Reference"),
                "header row",
            ),
            (
                "a missing column",
                file(&[r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS""#]).replace(",Amount", ""),
                "missing 'Amount' column",
            ),
            (
                "an unreadable amount",
                file(&[r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS",twelve"#]),
                "as an amount",
            ),
            (
                "an amount past any real balance",
                file(&[r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-99999999999999999.00"#]),
                "out of range",
            ),
            (
                "a day/month-first date",
                file(&[r#"20/01/2020,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#]),
                "YYYY/MM/DD",
            ),
            (
                "a repeated unique id",
                file(&[
                    r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#,
                    r#"2020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-6.00"#,
                ]),
                "appears twice",
            ),
        ];
        for (name, text, want) in cases {
            let err = parse_upload(text.as_bytes())
                .expect_err(&format!("{name} must be rejected"))
                .to_string();
            assert!(
                err.contains(want),
                "{name}: {err:?} should mention {want:?}"
            );
        }
    }

    #[test]
    fn a_truncated_row_is_rejected_not_silently_shortened() {
        let text = file(&[r#"2020/01/20,2020012001,EFTPOS"#]);
        let err = parse_upload(text.as_bytes())
            .expect_err("rejected")
            .to_string();
        assert!(
            err.contains("truncated") || err.contains("as an amount"),
            "{err:?}"
        );
    }

    // ---------------------------------------------------------------- zips

    const A: &str = r#"2020/01/20,2020012001,EFTPOS,,"SHOP A","EFTPOS",-5.00"#;
    const B: &str = r#"2021/02/10,2021021001,EFTPOS,,"SHOP B","EFTPOS",-7.00"#;

    #[test]
    fn reads_a_single_csv_out_of_a_zip() {
        let bytes = zip(&[("Export20260803.csv", &file(&[A]))]);
        let out = only(parse_upload(&bytes).expect("parses"));
        assert_eq!(out.rows_total, 1);
        assert_eq!(out.account, "12-3136-0000123-50");
        assert_eq!(out.sources, ["Export20260803.csv"]);
    }

    /// The point of the zip: one upload, every account.
    #[test]
    fn splits_a_zip_of_several_accounts_into_one_export_each() {
        let bytes = zip(&[
            (
                "chequing.csv",
                &file_for("0000123-50", "100.00", "20260803", &[A]),
            ),
            (
                "savings.csv",
                &file_for("0000123-51", "9500.00", "20260803", &[B]),
            ),
        ]);
        let out = parse_upload(&bytes).expect("parses");
        assert_eq!(out.exports.len(), 2);
        // Ordered by account number, so a preview is stable.
        assert_eq!(out.exports[0].account, "12-3136-0000123-50");
        assert_eq!(out.exports[1].account, "12-3136-0000123-51");
        assert_eq!(out.exports[0].ledger_balance_minor, Some(100_00));
        assert_eq!(out.exports[1].ledger_balance_minor, Some(9_500_00));
        assert_eq!(out.exports[0].transactions[0].description, "SHOP A");
        assert_eq!(out.exports[1].transactions[0].description, "SHOP B");
        // Each row's id is namespaced on its own account, so two accounts can't collide.
        assert!(out.exports[1].transactions[0]
            .external_id
            .starts_with("asb:12-3136-0000123-51:"));
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("2 different ASB accounts")),
            "{:?}",
            out.warnings
        );
    }

    /// Two windows of one account reconcile, the way a myIR upload's do — so a user can zip
    /// up every export they have without pruning the overlaps first.
    #[test]
    fn merges_overlapping_windows_of_the_same_account() {
        let bytes = zip(&[
            ("2020-2021.csv", &file(&[A, B])),
            // Overlaps on A, adds C.
            (
                "2021-2022.csv",
                &file(&[
                    B,
                    r#"2022/03/04,2022030401,EFTPOS,,"SHOP C","EFTPOS",-9.00"#,
                ]),
            ),
        ]);
        let out = only(parse_upload(&bytes).expect("parses"));
        assert_eq!(out.transactions.len(), 3);
        assert_eq!(out.rows_total, 3, "the shared row is counted once");
        assert_eq!(out.sum_minor, -5_00 - 7_00 - 9_00);
        assert_eq!(out.sources.len(), 2);
        assert_eq!(out.covered_from.as_deref(), Some("2020-01-20"));
        assert_eq!(out.covered_to.as_deref(), Some("2022-03-04"));
        let descriptions: Vec<&str> = out
            .transactions
            .iter()
            .map(|t| t.description.as_str())
            .collect();
        assert_eq!(descriptions, ["SHOP A", "SHOP B", "SHOP C"]);
    }

    /// `INSERT OR IGNORE` has no update-on-conflict, so a restated row would import
    /// *alongside* the stale one and double-count. Refuse rather than pick one.
    #[test]
    fn two_files_disagreeing_about_one_row_are_refused() {
        let restated = r#"2020/01/20,2020012001,EFTPOS,,"SHOP A","EFTPOS",-6.00"#;
        let bytes = zip(&[
            ("first.csv", &file(&[A])),
            ("second.csv", &file(&[restated])),
        ]);
        let err = parse_upload(&bytes).expect_err("refused").to_string();
        assert!(err.contains("disagree about transaction"), "{err:?}");
    }

    #[test]
    fn keeps_the_later_statements_closing_balance() {
        let bytes = zip(&[
            (
                "old.csv",
                &file_for("0000123-50", "42.00", "20250101", &[A]),
            ),
            (
                "new.csv",
                &file_for("0000123-50", "100.00", "20260803", &[B]),
            ),
        ]);
        let out = only(parse_upload(&bytes).expect("parses"));
        assert_eq!(out.ledger_balance_minor, Some(100_00));
        assert_eq!(out.ledger_balance_as_of.as_deref(), Some("2026-08-03"));
    }

    #[test]
    fn ignores_the_junk_a_finder_zip_carries_and_says_so() {
        let bytes = zip(&[
            ("__MACOSX/._chequing.csv", "junk"),
            (".DS_Store", "junk"),
            ("readme.txt", "not an export"),
            ("chequing.csv", &file(&[A])),
        ]);
        let out = parse_upload(&bytes).expect("parses");
        assert_eq!(out.exports.len(), 1);
        assert_eq!(out.exports[0].sources, ["chequing.csv"]);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("weren't .csv exports")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_zip_with_no_csv_in_it_is_refused() {
        let err = parse_upload(&zip(&[("notes.txt", "hello")]))
            .expect_err("refused")
            .to_string();
        assert!(err.contains("no .csv files"), "{err:?}");
    }

    #[test]
    fn a_bad_file_inside_a_zip_names_which_one() {
        let bytes = zip(&[
            ("good.csv", &file(&[A])),
            (
                "broken.csv",
                &file(&[r#"2020/01/20,2020012002,EFTPOS,,"X","EFTPOS",twelve"#]),
            ),
        ]);
        let err = parse_upload(&bytes).expect_err("refused").to_string();
        assert!(err.contains("broken.csv"), "{err:?}");
        assert!(err.contains("as an amount"), "{err:?}");
    }

    #[test]
    fn a_zip_of_too_many_files_is_refused() {
        let bodies: Vec<String> = (0..limits::ENTRIES + 1).map(|_| file(&[A])).collect();
        let names: Vec<String> = (0..limits::ENTRIES + 1)
            .map(|i| format!("e{i}.csv"))
            .collect();
        let entries: Vec<(&str, &str)> = names
            .iter()
            .zip(&bodies)
            .map(|(n, b)| (n.as_str(), b.as_str()))
            .collect();
        let err = parse_upload(&zip(&entries))
            .expect_err("refused")
            .to_string();
        assert!(err.contains("at most"), "{err:?}");
    }

    /// A zip bomb declares an enormous expansion, and must be refused on the declaration
    /// alone — before anything is read into memory.
    #[test]
    fn an_entry_declaring_more_than_the_limit_is_refused() {
        let mut bytes = zip(&[("big.csv", &file(&[A]))]);
        // The uncompressed size the reader believes is the *central directory's*, not the
        // local header's. Central record: signature(4) + made-by(2) + version(2) + flags(2)
        // + method(2) + time/date(4) + crc(4) + compressed(4) + uncompressed.
        let central = bytes
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("a central directory");
        let at = central + 4 + 2 + 2 + 2 + 2 + 4 + 4 + 4;
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());

        let err = parse_upload(&bytes).expect_err("refused").to_string();
        assert!(
            err.contains("over the limit") || err.contains("expands"),
            "{err:?}"
        );
    }

    #[test]
    fn the_cutover_applies_per_account_not_per_upload() {
        let bytes = zip(&[
            (
                "chequing.csv",
                &file_for("0000123-50", "1.00", "20260803", &[A, B]),
            ),
            (
                "savings.csv",
                &file_for("0000123-51", "2.00", "20260803", &[A, B]),
            ),
        ]);
        let mut out = parse_upload(&bytes).expect("parses");
        // The chequing account's feed reaches back to 2021; the savings one has none.
        out.exports[0].hold_back_from(Some(NaiveDate::from_ymd_opt(2021, 1, 1).unwrap()));
        out.exports[1].hold_back_from(None);

        assert_eq!(out.exports[0].transactions.len(), 1);
        assert_eq!(out.exports[0].held_back, 1);
        assert_eq!(out.exports[1].transactions.len(), 2);
        assert_eq!(out.exports[1].held_back, 0);
    }
}
