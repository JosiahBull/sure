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
//!
//! ASB offers **two shapes** of this export — plain, and "CSV with running balance" — and both
//! are accepted, because which one you get is a checkbox at export time rather than a property
//! of the account. The running-balance shape adds a trailing `Balance` column, brackets the
//! rows with two synthetic `Opening Balance`/`Closing Balance` entries carrying no id and no
//! amount, and writes its dates as `M/D/YY` instead of `YYYY/MM/DD`.
//!
//! That last part is what [`AsbDateOrder`] exists for. The order is decided **once per file**
//! from the whole date column, never row by row, and a short-form date is accepted *only* in
//! the running-balance shape: in a plain export it means a spreadsheet re-saved the file, which
//! is the one way this parser could silently misdate seven years of history. Where the two
//! shapes were compared over the same 2,851 rows of one account they agreed on every date,
//! amount, type, payee and memo, so everything above this paragraph applies to both unchanged.
//!
//! The balance column then pays for itself as a check on the result: it *states* the opening
//! balance [`AsbExport::implied_opening_minor`] otherwise has to work backwards for, and the
//! two disagreeing means a row is missing from the file — reported, not silently preferred.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::str::FromStr;

use chrono::{NaiveDate, Utc};
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
    /// Bytes the upload may itself be, checked before a byte of it is looked at. The zip
    /// budget bounds what an *archive* expands to and nothing else bounds a bare CSV, which
    /// is parsed straight out of the request body — and decoding it costs up to three times
    /// its size again if the bytes aren't valid UTF-8. The bound lives with the parser so it
    /// holds whether or not the route in front of it is still configured with a body limit.
    pub const UPLOAD_BYTES: usize = 16 * 1024 * 1024;
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
    /// `INT` — interest, which is **income on a savings account and a cost on a borrowing
    /// one**. The payee is the fixed label `ASB BANK - INTEREST` either way, so the sign is
    /// what separates them: over one household's seven-year history it was positive on all
    /// five savings accounts (136 rows) and negative only on the revolving home-loan facility
    /// (18 rows), with no account mixing the two. Hence [`AsbTranType::category`] takes the
    /// amount.
    Interest,
    /// `BANK FEE` — **not** reliably a fee. ASB writes this label for a $2.00 `ACTIVITY FEE`
    /// and for a $50,000 `Advance to Solicitor` (a mortgage drawn down to buy a house) alike,
    /// both negative, so nothing in the row separates them. Recognised so it stops being
    /// reported as an unknown type, but it carries no category — see
    /// [`AsbTranType::category`].
    BankFee,
    /// `LOAN INT` — interest charged on a loan, posted from the facility it's drawn against.
    LoanInterest,
    /// `LOAN PRIN` — the principal part of a loan repayment, moved to the loan's own
    /// sub-account (`…-92` beside a `…-00` facility). Money between the customer's own
    /// accounts, not spending — see [`AsbTranType::category`].
    LoanPrincipal,
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
            Interest => "INT",
            BankFee => "BANK FEE",
            LoanInterest => "LOAN INT",
            LoanPrincipal => "LOAN PRIN",
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
            // The four interest/fee types write the Particulars/Code/Reference triple, not a
            // card descriptor — `12-3136-0000123-92 006 INTEREST` is three distinct subfields,
            // so the stronger repair would weld real content together.
            Eftpos | Credit | DirectCredit | DirectDebit | AutomaticPayment | BillPay
            | TransferIn | TransferOut | Atm | Interest | BankFee | LoanInterest
            | LoanPrincipal => false,
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
            // These name no counterparty either: the payee is a fixed label restating the type
            // (`ASB BANK - INTEREST`, `LOAN - PRINCIPAL`, `ACTIVITY FEE`), so carrying it
            // through would mint a merchant called "ASB BANK - INTEREST". The text is still in
            // the description, which is what rules match on, and the category carries the
            // meaning.
            Interest | BankFee | LoanInterest | LoanPrincipal => None,
        }
    }

    /// The category hint carried into the import, where the type is unambiguous about what
    /// kind of movement it is.
    ///
    /// `amount_minor` is needed for exactly one type: `INT` is interest *earned* on a savings
    /// account and interest *charged* on a borrowing one, and only the sign says which. The two
    /// get distinct category **names** rather than one name with two kinds, because
    /// `sure_dal::providers`' find-or-create keys on `(name, group)` and would otherwise hand
    /// the second one whichever kind the first created.
    fn category(self, amount_minor: i64) -> Option<ProviderCategory> {
        use AsbTranType::*;
        let hint = |name: &str, kind: CategoryKind| {
            Some(ProviderCategory {
                name: name.to_string(),
                group: None,
                kind: Some(kind),
            })
        };
        match self {
            // Money between the customer's own accounts is a real movement but neither
            // spending nor income, so it must land in neither report.
            TransferIn | TransferOut => hint("Transfer", CategoryKind::Transfer),
            // Paying down principal is the same kind of movement: one of the customer's
            // balances falls and the loan's own sub-account rises by the same amount, so it is
            // not spending. Only the interest beside it is a cost.
            LoanPrincipal => hint("Loan principal", CategoryKind::Transfer),
            Interest if amount_minor < 0 => hint("Interest charged", CategoryKind::Expense),
            Interest => hint("Interest earned", CategoryKind::Income),
            LoanInterest => hint("Loan interest", CategoryKind::Expense),
            // `BILLPAY` reaches third parties, so it is ordinary spending as often as it
            // is a transfer. `BANK FEE` looks like it should be safe and is not: ASB writes it
            // for a $2.00 `ACTIVITY FEE` and for a $50,000 `Advance to Solicitor` — a mortgage
            // drawn down to buy a house — with the same sign and no other distinguishing field.
            // Calling that "Bank fees" would put a property purchase in the spending report, so
            // this type is recognised but deliberately unclassified. None of the rest carry a
            // reliable hint either.
            Debit | Eftpos | Credit | DirectCredit | DirectDebit | AutomaticPayment | BillPay
            | Atm | BankFee => None,
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
            "INT" => Ok(Interest),
            "BANK FEE" => Ok(BankFee),
            "LOAN INT" => Ok(LoanInterest),
            "LOAN PRIN" => Ok(LoanPrincipal),
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
// date order
// --------------------------------------------------------------------------------------

/// The order one file writes its dates in — a closed set, decided once by
/// [`decide_date_order`] and then handed to [`parse_date_in`] for every row.
///
/// Per-file rather than per-row on purpose: a wrong date order is a property of the export,
/// and deciding it row by row is how `1/2/20` becomes January in one row and February in the
/// next. See the module docs for which shape of export may use which order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsbDateOrder {
    /// `2020/01/20` — four-digit year first. The only order a *plain* export may use.
    Iso,
    /// `1/20/20` — month, day, two-digit year. What "CSV with running balance" writes.
    MonthDay,
    /// `20/01/20` — day, month, two-digit year. Not a shape ASB has been seen to emit, but a
    /// file that *proves* this order is read in it rather than refused for no reason.
    DayMonth,
}

impl AsbDateOrder {
    /// How to describe this order to someone reading an error.
    pub fn as_str(self) -> &'static str {
        match self {
            AsbDateOrder::Iso => "YYYY/MM/DD",
            AsbDateOrder::MonthDay => "M/D/YY",
            AsbDateOrder::DayMonth => "D/M/YY",
        }
    }
}

/// The synthetic rows ASB's running-balance export brackets the real ones with: an
/// `Opening Balance` dated at the export's `From date`, and a `Closing Balance` at its
/// `To date`. Neither is a transaction — no `Unique Id`, no `Tran Type`, no `Amount` — each
/// stating a figure in the `Balance` column instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BalanceMarker {
    Opening,
    Closing,
}

/// One row's date as written, plus whether that row is a [`BalanceMarker`]. The marker
/// matters because those two rows sit on the *declared* window's edges, which is what makes
/// them usable as proof of the order — see [`order_from_markers`].
struct DateSample<'a> {
    text: &'a str,
    marker: Option<BalanceMarker>,
}

/// A date's three numeric components, with the widths of the outer two. The widths are what
/// separate `2020/01/20` (year first) from `1/20/20` (year last) without yet deciding which
/// of month and day comes first.
fn split_date(text: &str) -> Option<([u32; 3], usize, usize)> {
    let mut parts = text.split('/');
    let raw = [parts.next()?, parts.next()?, parts.next()?];
    if parts.next().is_some() {
        return None;
    }
    let mut values = [0u32; 3];
    for (slot, part) in values.iter_mut().zip(raw) {
        if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some((values, raw[0].len(), raw[2].len()))
}

/// Whether these components could be a date in `order`'s *shape* — the year in the right
/// place and the right width. A row whose shape disagrees with the file's decided order is
/// not the same kind of date as the rest of the file, so reading it would be a guess.
fn fits_shape(order: AsbDateOrder, first_width: usize, last_width: usize) -> bool {
    match order {
        AsbDateOrder::Iso => first_width == 4,
        AsbDateOrder::MonthDay | AsbDateOrder::DayMonth => {
            first_width <= 2 && (last_width == 2 || last_width == 4)
        }
    }
}

/// The date these components spell under `order`, or `None` if they don't spell one.
fn date_in(order: AsbDateOrder, values: [u32; 3]) -> Option<NaiveDate> {
    let [a, b, c] = values;
    let (year, month, day) = match order {
        AsbDateOrder::Iso => (a, b, c),
        AsbDateOrder::MonthDay => (c, a, b),
        AsbDateOrder::DayMonth => (c, b, a),
    };
    NaiveDate::from_ymd_opt(expand_year(order, year)?, month, day)
}

/// A short-form `yy` as a full year. Always 20xx: ASB's running-balance export is a current
/// feature and holds nothing from last century, so the alternative reading would only ever
/// turn a legible date into a wrong one. A year that lands somewhere implausible is caught by
/// [`parse_date_in`]'s range check, which says so by name instead of guessing again.
fn expand_year(order: AsbDateOrder, year: u32) -> Option<i32> {
    let year = i32::try_from(year).ok()?;
    match order {
        // Written in full. `0020` is a real four-digit year and stays wrong, for the range
        // check to reject.
        AsbDateOrder::Iso => Some(year),
        AsbDateOrder::MonthDay | AsbDateOrder::DayMonth => {
            Some(if year < 100 { 2000 + year } else { year })
        }
    }
}

/// Decides the order every date in one file is written in, and returns any warning that
/// decision needs to carry.
///
/// `allow_short_form` is whether this file is the running-balance export — the only shape that
/// legitimately writes `M/D/YY`. In a plain export a short date means a spreadsheet re-saved
/// the file, so it stays fatal: those dates *parse*, which is exactly why accepting them would
/// misdate an import silently rather than fail it.
///
/// Within a running-balance file the order is settled by the first of these that speaks, in
/// order of how much it proves:
/// 1. a component past 12 somewhere in the column — arithmetic, not inference;
/// 2. a [`BalanceMarker`] matching the window the preamble declares ([`order_from_markers`]);
/// 3. the column being sorted under exactly one order ([`order_from_monotonicity`]).
///
/// If none of them does, the file is read as `M/D/YY` — every ASB export seen is — and a
/// warning names the window that produced, so a wrong reading is visible and can be undone
/// rather than quietly becoming history.
fn decide_date_order(
    samples: &[DateSample<'_>],
    allow_short_form: bool,
    declared: Option<(NaiveDate, NaiveDate)>,
    source: &str,
) -> anyhow::Result<(AsbDateOrder, Option<String>)> {
    // Not `iso`, which is the function that renders one.
    let mut iso_rows = 0usize;
    let mut short: Vec<(&DateSample<'_>, [u32; 3])> = Vec::new();
    for sample in samples {
        // A shape neither order can be read from is left out of the decision rather than
        // deciding the whole file's format on it; the per-row parse rejects it by name.
        let Some((values, first, last)) = split_date(sample.text) else {
            continue;
        };
        if fits_shape(AsbDateOrder::Iso, first, last) {
            iso_rows += 1;
        } else if fits_shape(AsbDateOrder::MonthDay, first, last) {
            short.push((sample, values));
        }
    }

    if iso_rows > 0 && !short.is_empty() {
        anyhow::bail!(
            "{source} mixes date formats — {iso_rows} row(s) are YYYY/MM/DD and {} are not, so \
             no one order fits the file; re-export it rather than editing it",
            short.len()
        );
    }
    let Some((first_short, _)) = short.first() else {
        // Every date is ISO, or the file has no rows at all.
        return Ok((AsbDateOrder::Iso, None));
    };
    if !allow_short_form {
        anyhow::bail!(
            "{source} has {} row(s) whose dates are not YYYY/MM/DD (the first reads \
             '{}') — re-export with the YYYY/MM/DD date format, and don't re-save the export \
             in a spreadsheet, which rewrites every date in the machine's own locale",
            short.len(),
            first_short.text
        );
    }

    let day_first = short.iter().any(|(_, v)| v[0] > 12);
    let month_first = short.iter().any(|(_, v)| v[1] > 12);
    match (day_first, month_first) {
        (true, true) => anyhow::bail!(
            "{source} cannot be read: some rows put a number past 12 first and others put one \
             second, so no single date order fits the file — re-export with the YYYY/MM/DD \
             date format"
        ),
        (false, true) => return Ok((AsbDateOrder::MonthDay, None)),
        (true, false) => return Ok((AsbDateOrder::DayMonth, None)),
        // Every component is 12 or less, so the column itself proves nothing.
        (false, false) => {}
    }

    if let Some(order) = order_from_markers(&short, declared) {
        return Ok((order, None));
    }
    if let Some(order) = order_from_monotonicity(&short) {
        return Ok((order, None));
    }

    let order = AsbDateOrder::MonthDay;
    let window = |values: [u32; 3]| date_in(order, values).map(iso).unwrap_or_default();
    let (from, to) = (
        window(short.first().map(|(_, v)| *v).unwrap_or_default()),
        window(short.last().map(|(_, v)| *v).unwrap_or_default()),
    );
    Ok((
        order,
        Some(format!(
            "{source}: every date in it is short-form and none is past the 12th, so month/day \
             order could not be proven — it was read as {}, covering {from} to {to}. Check that \
             window: if it's wrong, remove the import and re-export with the YYYY/MM/DD date \
             format",
            order.as_str()
        )),
    ))
}

/// The order that makes ASB's own window markers land on the window the preamble declares.
/// The `Closing Balance` row is dated the export's `To date`, so `8/4/26` against a declared
/// `20260804` fits `M/D/YY` and not `D/M/YY` — an exact proof, from data every
/// running-balance file carries, that does not depend on any day being past the 12th.
fn order_from_markers(
    short: &[(&DateSample<'_>, [u32; 3])],
    declared: Option<(NaiveDate, NaiveDate)>,
) -> Option<AsbDateOrder> {
    let (from, to) = declared?;
    let mut decided: Option<AsbDateOrder> = None;
    for (sample, values) in short {
        let Some(marker) = sample.marker else {
            continue;
        };
        let want = match marker {
            BalanceMarker::Opening => from,
            BalanceMarker::Closing => to,
        };
        // Judged one marker at a time. `1/1/20` against a `From date` of 20200101 fits both
        // readings, and folding that in as evidence *for* both would cancel out the closing
        // marker that does discriminate — which is every real file's only proof.
        let proves = match (
            date_in(AsbDateOrder::MonthDay, *values) == Some(want),
            date_in(AsbDateOrder::DayMonth, *values) == Some(want),
        ) {
            (true, false) => AsbDateOrder::MonthDay,
            (false, true) => AsbDateOrder::DayMonth,
            (true, true) | (false, false) => continue,
        };
        match decided {
            None => decided = Some(proves),
            // Two markers proving opposite orders is a contradiction, not a decision; leave it
            // to the weaker signals and, failing those, to the warning.
            Some(already) if already != proves => return None,
            Some(_) => {}
        }
    }
    decided
}

/// The order that leaves the column sorted, where only one of the two does. ASB writes its
/// rows oldest-first, so a reading that goes backwards in time is the wrong reading. Weaker
/// than the two proofs above — a one-row file is sorted under both — so it is asked last.
fn order_from_monotonicity(short: &[(&DateSample<'_>, [u32; 3])]) -> Option<AsbDateOrder> {
    let sorted = |order: AsbDateOrder| {
        let mut prev: Option<NaiveDate> = None;
        for (_, values) in short {
            let Some(date) = date_in(order, *values) else {
                return false;
            };
            if prev.is_some_and(|p| date < p) {
                return false;
            }
            prev = Some(date);
        }
        true
    };
    match (
        sorted(AsbDateOrder::MonthDay),
        sorted(AsbDateOrder::DayMonth),
    ) {
        (true, false) => Some(AsbDateOrder::MonthDay),
        (false, true) => Some(AsbDateOrder::DayMonth),
        (true, true) | (false, false) => None,
    }
}

/// Whether this row is one of ASB's synthetic window markers rather than a transaction.
///
/// Matched on the whole shape — no id, no amount, *and* the label ASB writes in `Payee` — not
/// on the missing id alone: a real row that lost its id is a corrupt file and must still be
/// refused, because `Unique Id` is what imported rows dedupe on. Only considered for the
/// running-balance export, the only shape that emits these at all.
fn balance_marker(
    has_balance_column: bool,
    unique_id: &str,
    amount: &str,
    payee: &str,
) -> Option<BalanceMarker> {
    if !has_balance_column || !unique_id.trim().is_empty() || !amount.trim().is_empty() {
        return None;
    }
    // Over arbitrary text out of an uploaded file, not over a closed set: any other id-less
    // row is not a marker ASB writes, so it falls through to the ordinary path.
    match squeeze(payee).to_ascii_uppercase().as_str() {
        "OPENING BALANCE" => Some(BalanceMarker::Opening),
        "CLOSING BALANCE" => Some(BalanceMarker::Closing),
        _ => None,
    }
}

/// Minor units as a plain decimal, for a warning a human reads. No currency symbol: an export
/// names no currency, and the target account's applies.
fn money(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
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
    /// The opening balance the export *states*, from the `Opening Balance` row only the
    /// running-balance shape carries, and the day it is as of (that export's `From date`).
    /// `None` for a plain export, which states no such thing — then
    /// [`AsbExport::implied_opening_minor`] works it back from the closing balance instead.
    pub stated_opening_minor: Option<i64>,
    pub stated_opening_as_of: Option<String>,
    /// Every row in the file summed, so a caller can state the implied opening balance.
    pub sum_minor: i64,
    /// Non-fatal observations: an unfamiliar transaction type, rows held back, a row
    /// outside the file's declared window.
    pub warnings: Vec<String>,
}

impl AsbExport {
    /// The balance the account held immediately before this export's first row.
    ///
    /// Taken from [`AsbExport::stated_opening_minor`] where the export states it — the
    /// running-balance shape does — and otherwise worked back from the closing balance ASB
    /// states less every movement in between. `None` when the export offers neither, since
    /// then there is nothing to work from.
    ///
    /// The derivation runs over *every* row in the file, including any the cutover held back:
    /// those movements still happened, they are just already on the ledger from the live feed.
    /// Where both figures exist they must agree to the cent, and [`parse_csv`] warns when they
    /// don't rather than quietly preferring one — a disagreement means the file is missing a
    /// row, which is worth knowing about whichever figure is used.
    pub fn implied_opening_minor(&self) -> Option<i64> {
        self.stated_opening_minor
            .or_else(|| self.ledger_balance_minor.map(|b| b - self.sum_minor))
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
    if bytes.len() > limits::UPLOAD_BYTES {
        anyhow::bail!(
            "the upload is {} bytes; at most {} are read at once",
            bytes.len(),
            limits::UPLOAD_BYTES
        );
    }
    let Entries {
        files,
        mut warnings,
    } = read_entries(bytes)?;
    let mut parsed = Vec::new();
    let mut failed = Vec::new();
    let mut rows = 0usize;
    for entry in files {
        // One unreadable file does not sink the upload. ASB exports one file per account and a
        // zip carries the lot, so aborting on the first bad one costs every *good* account in
        // it an import and reports a single filename as if it were the only problem. Collected
        // and reported instead, and the rest goes through.
        let export = match parse_csv(&entry.name, &entry.body) {
            Ok(export) => export,
            Err(e) => {
                failed.push(format!("{e}"));
                continue;
            }
        };
        rows += export.rows_total as usize;
        if rows > limits::ROWS {
            anyhow::bail!(
                "too many rows: this upload holds more than {} transactions",
                limits::ROWS
            );
        }
        parsed.push(export);
    }
    // Nothing readable at all is still an error, not a cheerful empty result — and for the
    // single-CSV upload that is every failure, so a bare CSV behaves exactly as it used to.
    if parsed.is_empty() {
        anyhow::bail!("{}", failed.join("; "));
    }
    for failure in &failed {
        warnings.push(format!(
            "{failure} — that file was skipped; the rest of the upload was read"
        ));
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
        // A stated opening balance only opens the *merged* history if it predates every row in
        // it. Where the merge pulled in an earlier file than the one that stated it, it is a
        // mid-history figure, and putting that on the ledger would invent a large movement.
        // Dropped rather than adjusted: `implied_opening_minor` then works it back from the
        // closing balance, which spans every row the merge holds.
        if let (Some(at), Some(from)) = (&export.stated_opening_as_of, &export.covered_from) {
            if at > from {
                export.stated_opening_minor = None;
                export.stated_opening_as_of = None;
            }
        }
    }
    by_account.sort_by(|a, b| a.account.cmp(&b.account));
    Ok(by_account)
}

/// Fold `from` into `into`, both being exports of the same ASB account.
fn absorb(into: &mut AsbExport, from: AsbExport) -> anyhow::Result<()> {
    // The earlier window's opening balance is the one that opens the merged history; a later
    // file states a figure from the middle of it, which would be wrong to put on the ledger.
    // `merge_by_account` drops it entirely if it still doesn't predate every row.
    let take_opening = match (&into.stated_opening_as_of, &from.stated_opening_as_of) {
        (None, Some(_)) => true,
        (Some(into_at), Some(from_at)) => from_at < into_at,
        (Some(_), None) | (None, None) => false,
    };
    if take_opening {
        into.stated_opening_minor = from.stated_opening_minor;
        into.stated_opening_as_of = from.stated_opening_as_of.clone();
    }

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
    // Deduplicated: two windows of one account each report the same unfamiliar transaction
    // type, and the preview showing "9 row(s) have type 'INT'" twice reads as eighteen rows.
    for warning in from.warnings {
        if !into.warnings.contains(&warning) {
            into.warnings.push(warning);
        }
    }
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
    let (preamble, body) = split_at_header(source, &text)?;
    let preamble = parse_preamble(source, preamble)?;

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
    // Only ASB's "CSV with running balance" shape has this, and its presence is what permits
    // that shape's short-form dates — see [`decide_date_order`]. Absent from a plain export,
    // where the extra column simply isn't there to read.
    let i_balance = col("Balance");

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

    // Read the rows before parsing any of them: the date order is a property of the whole
    // column and cannot be settled row by row (see [`decide_date_order`]). The bodies are
    // already bounded by `limits::UPLOAD_BYTES`, and every row ends up held in `transactions`
    // regardless, so this holds nothing that wasn't going to be held anyway. The blank line
    // ASB writes between the header and the rows is dropped here.
    let records: Vec<csv::StringRecord> = reader
        .records()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|r| !r.iter().all(|f| f.trim().is_empty()))
        .collect();

    let marker = |record: &csv::StringRecord| {
        balance_marker(
            i_balance.is_some(),
            record.get(i_id).unwrap_or_default(),
            record.get(i_amount).unwrap_or_default(),
            record.get(i_payee).unwrap_or_default(),
        )
    };
    let samples: Vec<DateSample<'_>> = records
        .iter()
        .map(|r| DateSample {
            text: r.get(i_date).unwrap_or_default().trim(),
            marker: marker(r),
        })
        .collect();
    let (order, order_warning) =
        decide_date_order(&samples, i_balance.is_some(), preamble.declared, source)?;
    out.warnings.extend(order_warning);

    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut unknown_types: BTreeMap<String, i64> = BTreeMap::new();
    let mut outside_window = 0i64;
    let (mut first, mut last): (Option<NaiveDate>, Option<NaiveDate>) = (None, None);
    // The running balance, walked forward from the `Opening Balance` row, so a row missing
    // from the middle of the file shows up as an inconsistency rather than as an import that
    // is quietly short. Stays `None` for a plain export, which states no balances.
    let mut running: Option<i64> = None;
    let mut chain_breaks = 0i64;
    let mut stated_closing: Option<i64> = None;

    for (index, record) in records.iter().enumerate() {
        // +1 for the header, +1 for 1-based. Named, because one zip can hold a dozen files
        // and "row 412" alone doesn't say which.
        let at = format!("{source} row {}", index + 2);
        let field = |i: usize, name: &str| -> anyhow::Result<&str> {
            record
                .get(i)
                .ok_or_else(|| anyhow::anyhow!("{at}: truncated, no '{name}' field"))
        };
        let balance = match i_balance {
            Some(i) => {
                let text = field(i, "Balance")?;
                (!text.trim().is_empty())
                    .then(|| parse_minor(text, &at))
                    .transpose()?
            }
            None => None,
        };

        // ASB's synthetic window markers. Not transactions — they carry no id and no amount —
        // but the opening one *states* the balance that otherwise has to be worked backwards
        // for, and both are needed to prove the date order, so they are read and then dropped.
        if let Some(kind) = marker(record) {
            let date = parse_date_in(order, field(i_date, "Date")?, &at)?;
            match kind {
                BalanceMarker::Opening => {
                    out.stated_opening_minor = balance;
                    out.stated_opening_as_of = Some(iso(date));
                    running = balance;
                }
                BalanceMarker::Closing => stated_closing = balance,
            }
            continue;
        }

        let date = parse_date_in(order, field(i_date, "Date")?, &at)?;
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
        if let (Some(previous), Some(balance)) = (running, balance) {
            if previous + amount_minor != balance {
                chain_breaks += 1;
            }
        }
        if balance.is_some() {
            running = balance;
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
            category: kind.and_then(|k| k.category(amount_minor)),
        });
    }

    out.covered_from = first.map(iso);
    out.covered_to = last.map(iso);

    // What the running-balance shape buys: three checks a plain export cannot offer.
    if chain_breaks > 0 {
        out.warnings.push(format!(
            "{chain_breaks} row(s) don't square with the running balance stated beside them, so \
             the file is missing rows or restates some — the reconstructed history before today \
             will be out by the difference"
        ));
    }
    if let (Some(stated), Some(ledger)) = (stated_closing, out.ledger_balance_minor) {
        if stated != ledger {
            out.warnings.push(format!(
                "the export's closing balance row says {} but its header says {} — one of the \
                 two was misread",
                money(stated),
                money(ledger)
            ));
        }
    }
    // The strongest check in the file: what ASB says the account opened at, against what its
    // own rows and closing balance imply. Reported rather than resolved — a difference means a
    // row is missing, and which figure is nearer the truth isn't knowable from here.
    if let (Some(stated), Some(ledger)) = (out.stated_opening_minor, out.ledger_balance_minor) {
        let derived = ledger - out.sum_minor;
        if stated != derived {
            out.warnings.push(format!(
                "the export states an opening balance of {} but its own rows and closing balance \
                 imply {}, a difference of {} — the stated figure was used, and some row is \
                 missing from the file",
                money(stated),
                money(derived),
                money(derived - stated)
            ));
        }
    }

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
///
/// Returns a [`Cow`] and strips the BOM from the *bytes* rather than from the decoded text,
/// so the ordinary case — a valid UTF-8 export — is a borrow and copies nothing.
/// `from_utf8_lossy` already allocates up to three times the input when the bytes are bad
/// (every stray byte becomes a 3-byte U+FFFD); stripping the prefix afterwards forced a
/// second, unconditional copy on top of that, so an upload of invalid bytes held the
/// original, the replacement text and the copy of it live at once.
fn decode(bytes: &[u8]) -> Cow<'_, str> {
    // UTF-8 BOM. Excel writes one when it saves a CSV, and it would otherwise be read as
    // part of the preamble's first line.
    String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
}

/// Splits the preamble from the CSV proper. Found by scanning for the header row rather
/// than skipping a fixed number of lines, so a change in how many facts ASB states above
/// it doesn't break the parse.
fn split_at_header<'a>(source: &str, text: &'a str) -> anyhow::Result<(&'a str, &'a str)> {
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
        "{source} is not an ASB export: no 'Date,Unique Id,Tran Type,…' header row"
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

fn parse_preamble(source: &str, text: &str) -> anyhow::Result<Preamble> {
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
            let (account, product) = parse_account_line(source, line)?;
            out.account = account;
            out.product = product;
        }
    }
    if out.account.is_empty() {
        return Err(anyhow::anyhow!(
            "{source} is not an ASB export: no 'Bank …; Branch …; Account …' line naming the \
             account"
        ));
    }
    out.declared = from.zip(to);
    Ok(out)
}

/// `Bank 12; Branch 3136; Account 0000123-50 (Streamline)` → `12-3136-0000123-50`,
/// `Streamline`.
fn parse_account_line(source: &str, line: &str) -> anyhow::Result<(String, Option<String>)> {
    let malformed = || anyhow::anyhow!("{source}: cannot read the account from '{line}'");
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

/// The oldest date a statement row may carry. A bank movement from before this is not
/// history anyone is importing, it is a date whose format was misread.
const EARLIEST_ROW: NaiveDate = match NaiveDate::from_ymd_opt(1970, 1, 1) {
    Some(d) => d,
    None => unreachable!(),
};

/// One row's date, in the order [`decide_date_order`] settled for the whole file.
///
/// The order is a parameter rather than a guess, which is the trap this function exists to
/// close. `%Y/%m/%d` on its own does not close it: chrono's `%Y` accepts *one to four* digits,
/// so `20/01/2020` fails only because `2020` is not a day, while `20/01/20` parses happily as
/// `0020-01-20`. That second one is exactly what arrives when someone opens a plain export in
/// Excel and saves it — Excel rewrites every date in the machine's short-date locale, `d/mm/yy`
/// on an NZ or UK system. Those dates are *parseable*, so nothing downstream drops them the way
/// it drops garbage: they flow into `covered_from` and the report loaders'
/// `earliest_transaction_date` and drag the net-worth series back two thousand years.
///
/// So a row's shape must match the order its file was decided to be in (a plain export's must
/// be ISO, and short-form dates never reach here from one), the date is built from the
/// components directly rather than through a lenient format string, and it is then
/// range-checked at both ends: `0020/01/20` is four digits and just as wrong, and a row dated a
/// decade out is bad data whichever way it got there.
fn parse_date_in(order: AsbDateOrder, text: &str, at: &str) -> anyhow::Result<NaiveDate> {
    let text = text.trim();
    let unreadable = || {
        anyhow::anyhow!(
            "{at}: cannot read '{text}' as a date — the rest of the file reads as {}; re-export \
             with the YYYY/MM/DD date format",
            order.as_str()
        )
    };
    let Some((values, first_width, last_width)) = split_date(text) else {
        return Err(unreadable());
    };
    if !fits_shape(order, first_width, last_width) {
        return Err(unreadable());
    }
    let date = date_in(order, values).ok_or_else(unreadable)?;

    // Tomorrow, not today: the export is written in NZ local time and compared here in UTC,
    // so the last row of a statement pulled late in the evening is legitimately dated ahead
    // of this process's own date.
    let latest = Utc::now().date_naive().succ_opt().unwrap_or(NaiveDate::MAX);
    if date < EARLIEST_ROW || date > latest {
        return Err(anyhow::anyhow!(
            "{at}: '{text}' is not a plausible statement date — a row before {EARLIEST_ROW} or \
             after tomorrow means the file's date format was misread; re-export with the \
             YYYY/MM/DD date format"
        ));
    }
    Ok(date)
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

    /// ASB's "CSV with running balance" shape: the extra column, the two synthetic markers,
    /// and `M/D/YY` dates. `rows` are the real ones, given without their `Balance` cell —
    /// `balances` supplies those in step, so a test can state a consistent chain or break it
    /// deliberately.
    ///
    /// The declared window is 2020/01/01 → 2026/08/04, so the `Closing Balance` marker's
    /// `8/4/26` is what proves `M/D/YY` in a file whose own rows never pass the 12th.
    fn balance_file(opening: &str, rows: &[(&str, &str)], closing: &str) -> String {
        let mut out = String::from(
            "Created date / time : 04 August 2026 / 12:36:28\r\n\
             Bank 12; Branch 3136; Account 0000123-50 (Streamline)\r\n\
             From date 20200101\r\n\
             To date 20260804\r\n\
             Avail Bal : 100.00 as of 20260804\r\n\
             Ledger Balance : 100.00 as of 20260804\r\n\
             Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount,Balance\r\n\r\n",
        );
        out.push_str(&format!("1/1/20,,,,Opening Balance,,,{opening}\r\n"));
        for (row, balance) in rows {
            out.push_str(&format!("{row},{balance}\r\n"));
        }
        out.push_str(&format!("8/4/26,,,,Closing Balance,,,{closing}\r\n"));
        out
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

    // ------------------------------------------------- the running-balance export shape

    #[test]
    fn reads_the_running_balance_shape_and_the_short_dates_in_it() {
        let text = balance_file(
            "18694.18",
            &[(
                r#"1/2/20,2020010201,EFTPOS,,"SHOP","EFTPOS",-18594.18"#,
                "100.00",
            )],
            "100.00",
        );
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.account, "12-3136-0000123-50");
        // The two markers are not transactions, so only the real row counts.
        assert_eq!(out.rows_total, 1);
        assert_eq!(out.transactions.len(), 1);
        // 2 January, not 1 February: the closing marker's `8/4/26` against the declared
        // `To date 20260804` is what proves the order in a file whose rows never pass the 12th.
        assert_eq!(out.transactions[0].posted_at, "2020-01-02T12:00:00+00:00");
        assert_eq!(out.covered_from.as_deref(), Some("2020-01-02"));
        // Stated outright, rather than worked back from the closing balance.
        assert_eq!(out.stated_opening_minor, Some(18_694_18));
        assert_eq!(out.stated_opening_as_of.as_deref(), Some("2020-01-01"));
        assert_eq!(out.implied_opening_minor(), Some(18_694_18));
        assert_eq!(
            out.opening_balance_row().expect("a row").amount_minor,
            18_694_18
        );
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    /// The text repair is the substantive part of this parser, and it must not care which shape
    /// the row arrived in — the two exports were compared over one account's whole history and
    /// agreed on every payee and memo.
    #[test]
    fn repairs_text_the_same_way_in_either_shape() {
        let row = r#"2020010201,DEBIT,,"DEBIT","CARD 1111 Chemist Ware house Albany",-23.83"#;
        let plain = one(&format!("2020/01/02,{row}"));
        let with_balance = only(
            parse_upload(
                balance_file("100.00", &[(&format!("1/2/20,{row}"), "76.17")], "76.17").as_bytes(),
            )
            .expect("parses"),
        )
        .transactions
        .pop()
        .expect("one row");
        assert_eq!(
            with_balance.description,
            "CARD 1111 Chemist Warehouse Albany"
        );
        assert_eq!(with_balance.description, plain.description);
        assert_eq!(with_balance.merchant, plain.merchant);
        assert_eq!(with_balance.amount_minor, plain.amount_minor);
    }

    /// The cheapest proof, and the one the real exports mostly rely on: a day past the 12th
    /// can only be a day.
    #[test]
    fn proves_the_date_order_from_a_component_past_the_twelfth() {
        let text = balance_file(
            "0.00",
            &[
                (r#"9/18/20,2020091801,EFTPOS,,"S","EFTPOS",-5.00"#, "-5.00"),
                (
                    r#"10/2/20,2020100201,EFTPOS,,"S","EFTPOS",105.00"#,
                    "100.00",
                ),
            ],
            "100.00",
        );
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.transactions[0].posted_at, "2020-09-18T12:00:00+00:00");
        assert_eq!(out.transactions[1].posted_at, "2020-10-02T12:00:00+00:00");
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    /// Not a shape ASB has been seen to emit — but the proof is exact, so a file carrying it is
    /// read rather than refused.
    #[test]
    fn reads_a_day_month_export_where_the_file_proves_that_order() {
        let text = balance_file(
            "0.00",
            &[
                (r#"18/9/20,2020091801,EFTPOS,,"S","EFTPOS",-5.00"#, "-5.00"),
                (
                    r#"2/10/20,2020100201,EFTPOS,,"S","EFTPOS",105.00"#,
                    "100.00",
                ),
            ],
            "100.00",
        );
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.transactions[0].posted_at, "2020-09-18T12:00:00+00:00");
        assert_eq!(out.transactions[1].posted_at, "2020-10-02T12:00:00+00:00");
    }

    /// Nothing decides it: no component past the 12th, no declared window for the markers to be
    /// checked against, and one row is in order either way. Read as `M/D/YY` because every ASB
    /// export seen is — and said out loud, so a wrong window is visible rather than becoming
    /// history quietly.
    #[test]
    fn assumes_month_day_and_says_so_when_nothing_can_prove_the_order() {
        let text = balance_file(
            "0.00",
            &[(
                r#"1/2/20,2020010201,D/C,,"D/C FROM X","Y",100.00"#,
                "100.00",
            )],
            "100.00",
        )
        .replace("From date 20200101\r\n", "")
        .replace("To date 20260804\r\n", "");
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.transactions[0].posted_at, "2020-01-02T12:00:00+00:00");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("could not be proven") && w.contains("M/D/YY")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn refuses_a_file_no_single_date_order_fits() {
        let text = balance_file(
            "0.00",
            &[
                (r#"18/9/20,2020091801,EFTPOS,,"S","EFTPOS",-5.00"#, "-5.00"),
                (
                    r#"9/18/20,2020091802,EFTPOS,,"S","EFTPOS",105.00"#,
                    "100.00",
                ),
            ],
            "100.00",
        );
        let err = parse_upload(text.as_bytes())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("no single date order fits"), "{err:?}");
    }

    /// The discriminator the whole design rests on: short dates are ASB's in the
    /// running-balance shape, and spreadsheet damage in a plain one. Accepting them there would
    /// misdate an import silently, which is worse than refusing the file.
    #[test]
    fn a_plain_export_with_short_dates_is_refused_as_spreadsheet_damage() {
        let text = file(&[r#"20/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#]);
        let err = parse_upload(text.as_bytes())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("YYYY/MM/DD"), "{err:?}");
        assert!(err.contains("spreadsheet"), "{err:?}");
    }

    #[test]
    fn a_file_mixing_date_formats_is_refused() {
        let text = balance_file(
            "0.00",
            &[
                (
                    r#"2020/09/18,2020091801,EFTPOS,,"S","EFTPOS",-5.00"#,
                    "-5.00",
                ),
                (
                    r#"9/19/20,2020091901,EFTPOS,,"S","EFTPOS",105.00"#,
                    "100.00",
                ),
            ],
            "100.00",
        );
        let err = parse_upload(text.as_bytes())
            .expect_err("refused")
            .to_string();
        assert!(err.contains("mixes date formats"), "{err:?}");
    }

    /// A marker is recognised by its whole shape, not by the missing id: `Unique Id` is what
    /// imported rows dedupe on, so a real row that lost one is a corrupt file either way.
    #[test]
    fn an_id_less_row_that_is_not_a_balance_marker_is_still_refused() {
        for row in [
            // No id, but a real amount — a transaction, not a marker.
            r#"1/2/20,,EFTPOS,,"SHOP","EFTPOS",100.00"#,
            // Wears the marker's label and still carries an amount, so it is not one.
            r#"1/2/20,,,,Opening Balance,,100.00"#,
        ] {
            let text = balance_file("0.00", &[(row, "100.00")], "100.00");
            let err = parse_upload(text.as_bytes())
                .expect_err("refused")
                .to_string();
            assert!(err.contains("empty 'Unique Id'"), "{row}: {err:?}");
        }
    }

    /// What the balance column buys: a row missing from the middle of the file is otherwise
    /// invisible — every remaining row is well-formed and the totals simply come out short.
    #[test]
    fn warns_when_a_row_does_not_square_with_the_running_balance() {
        let text = balance_file(
            "0.00",
            &[
                (r#"1/2/20,2020010201,EFTPOS,,"S","EFTPOS",-5.00"#, "-5.00"),
                // The balance moves 200.00 on a 105.00 credit: something is missing.
                (
                    r#"1/3/20,2020010301,D/C,,"D/C FROM X","Y",105.00"#,
                    "195.00",
                ),
            ],
            "100.00",
        );
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert!(
            out.warnings.iter().any(|w| w.contains("running balance")),
            "{:?}",
            out.warnings
        );
    }

    /// The chain is self-consistent here, so only the header is out of step — and that is worth
    /// saying, because the *derived* opening balance is worked back from the header figure while
    /// the stated one comes from the rows.
    #[test]
    fn warns_when_the_header_and_the_closing_row_disagree() {
        let text = balance_file(
            "500.00",
            &[(
                r#"1/2/20,2020010201,D/C,,"D/C FROM X","Y",100.00"#,
                "600.00",
            )],
            "600.00",
        );
        let out = only(parse_upload(text.as_bytes()).expect("parses"));
        assert_eq!(out.stated_opening_minor, Some(500_00));
        // The stated figure is what lands on the ledger, not the 0.00 the header implies.
        assert_eq!(out.implied_opening_minor(), Some(500_00));
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("closing balance row says")),
            "{:?}",
            out.warnings
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("states an opening balance")),
            "{:?}",
            out.warnings
        );
    }

    // ------------------------------------------------- merging the two shapes together

    /// The real case: one account exported twice, once in each shape. They agree row for row,
    /// so the overlap collapses and the stated opening balance survives.
    #[test]
    fn merges_a_plain_and_a_running_balance_export_of_one_account() {
        let plain = file(&[r#"2020/01/02,2020010201,EFTPOS,,"SHOP","EFTPOS",-18594.18"#]);
        let with_balance = balance_file(
            "18694.18",
            &[(
                r#"1/2/20,2020010201,EFTPOS,,"SHOP","EFTPOS",-18594.18"#,
                "100.00",
            )],
            "100.00",
        );
        let out = only(
            parse_upload(&zip(&[
                ("plain.csv", &plain),
                ("balance.csv", &with_balance),
            ]))
            .expect("parses"),
        );
        assert_eq!(out.rows_total, 1, "the shared row is counted once");
        assert_eq!(out.transactions.len(), 1);
        assert_eq!(out.sources.len(), 2);
        assert_eq!(out.stated_opening_minor, Some(18_694_18));
        assert_eq!(out.implied_opening_minor(), Some(18_694_18));
    }

    /// A stated opening balance is only an *opening* balance for the history it opens. Where the
    /// merge pulled in a file that starts earlier, it is a figure from the middle, and putting
    /// that on the ledger would invent a large movement in the middle of it.
    #[test]
    fn a_stated_opening_from_the_middle_of_a_merged_history_is_dropped() {
        let row = |id: &str, at: &str| ProviderTransaction {
            external_id: format!("asb:12-3136-0000123-50:{id}"),
            posted_at: format!("{at}T12:00:00+00:00"),
            amount_minor: -100,
            currency_code: None,
            description: "X".to_string(),
            merchant: None,
            category: None,
        };
        let part = |id, at| AsbExport {
            account: "12-3136-0000123-50".to_string(),
            transactions: vec![row(id, at)],
            rows_total: 1,
            sum_minor: -100,
            sources: vec![format!("{id}.csv")],
            ..AsbExport::default()
        };
        let earlier = part("1", "2019-05-05");
        let later = AsbExport {
            ledger_balance_minor: Some(0),
            ledger_balance_as_of: Some("2026-08-04".to_string()),
            stated_opening_minor: Some(500_00),
            stated_opening_as_of: Some("2020-01-01".to_string()),
            ..part("2", "2021-05-05")
        };

        let merged = merge_by_account(vec![earlier, later]).expect("merges");
        let out = &merged[0];
        assert_eq!(out.covered_from.as_deref(), Some("2019-05-05"));
        assert_eq!(out.stated_opening_minor, None, "it opens nothing");
        // Worked back from the closing balance over both files instead: 0.00 less -2.00.
        assert_eq!(out.implied_opening_minor(), Some(200));
    }

    /// Two windows of one account each report the same unfamiliar type, and the preview showing
    /// it twice reads as twice as many rows.
    #[test]
    fn a_warning_both_files_raise_is_reported_once() {
        let odd = |id: &str| format!(r#"2020/01/0{id},202001010{id},KOHA,,"SHOP","EFTPOS",-5.00"#);
        let out = only(
            parse_upload(&zip(&[
                ("a.csv", &file(&[&odd("1")])),
                ("b.csv", &file(&[&odd("2")])),
            ]))
            .expect("parses"),
        );
        let about_type: Vec<&String> = out
            .warnings
            .iter()
            .filter(|w| w.contains("'KOHA'"))
            .collect();
        assert_eq!(about_type.len(), 1, "{:?}", out.warnings);
    }

    // ------------------------------------------------- interest, fees and loan repayments

    /// The four types a savings or loan account uses, which a chequing-only export never shows.
    /// Rows shaped as ASB writes them, with the fixed labels it puts in `Payee`.
    #[test]
    fn reads_the_interest_fee_and_loan_types() {
        for (label, want) in [
            ("INT", AsbTranType::Interest),
            ("BANK FEE", AsbTranType::BankFee),
            ("LOAN INT", AsbTranType::LoanInterest),
            ("LOAN PRIN", AsbTranType::LoanPrincipal),
        ] {
            assert_eq!(AsbTranType::from_str(label), Ok(want), "{label}");
            assert_eq!(want.as_str(), label, "round trip");
        }
    }

    /// The one type whose meaning the sign decides: credited on a savings account, charged on a
    /// borrowing one, with the same `ASB BANK - INTEREST` payee either way. Distinct *names*, so
    /// the DAL's find-or-create on `(name, group)` can't hand the second one the first's kind.
    #[test]
    fn interest_is_income_when_credited_and_a_cost_when_charged() {
        let earned =
            one(r#"2020/01/01,2020010101,INT,,"ASB BANK - INTEREST","CR.INT TO 01/01/2020",1.23"#);
        let charged = one(r#"2020/01/01,2020010101,INT,,"ASB BANK - INTEREST","",-45.67"#);

        let earned = earned.category.expect("a hint");
        assert_eq!(earned.name, "Interest earned");
        assert_eq!(earned.kind, Some(CategoryKind::Income));

        let charged = charged.category.expect("a hint");
        assert_eq!(charged.name, "Interest charged");
        assert_eq!(charged.kind, Some(CategoryKind::Expense));
    }

    /// Interest is a cost; the principal beside it is the customer's own money moving to the
    /// loan's sub-account, so it must land in neither report — the same call the transfer types
    /// get, and the reason `LOAN PRIN` is not simply "spending".
    #[test]
    fn loan_interest_is_a_cost_and_loan_principal_is_a_transfer() {
        let interest = one(
            r#"2023/06/01,2023060101,LOAN INT,,"LOAN - INTEREST","12-3136-0000123-92 006 INTEREST",-512.34"#,
        );
        let principal = one(
            r#"2023/06/01,2023060101,LOAN PRIN,,"LOAN - PRINCIPAL","12-3136-0000123-92 006 PRINCIPAL",-311.11"#,
        );

        let interest = interest.category.expect("a hint");
        assert_eq!(interest.name, "Loan interest");
        assert_eq!(interest.kind, Some(CategoryKind::Expense));

        let principal = principal.category.expect("a hint");
        assert_eq!(principal.name, "Loan principal");
        assert_eq!(
            principal.kind,
            Some(CategoryKind::Transfer),
            "principal is not spending"
        );
    }

    /// `BANK FEE` is recognised but deliberately left unclassified, because ASB uses the one
    /// label for a $2.00 account fee and for a $50,000 mortgage drawdown to a solicitor. Both
    /// are negative and nothing else in the row separates them, so a "Bank fees" expense would
    /// put a house purchase in the spending report.
    #[test]
    fn a_bank_fee_is_recognised_but_left_uncategorised() {
        for row in [
            r#"2021/03/01,2021030101,BANK FEE,,"ACTIVITY FEE","",-2.00"#,
            r#"2025/09/04,2025090401,BANK FEE,,"","Advance to Solicitor",-50000.00"#,
        ] {
            let out = one(row);
            assert!(out.category.is_none(), "{row}");
            // Still imported, still described, and no longer reported as an unknown type.
            assert!(!out.description.is_empty(), "{row}");
        }
        let out = export(&[r#"2021/03/01,2021030101,BANK FEE,,"ACTIVITY FEE","",-2.00"#]);
        assert!(
            !out.warnings.iter().any(|w| w.contains("unfamiliar")),
            "{:?}",
            out.warnings
        );
    }

    /// Their payees are fixed labels restating the type, so none of them may mint a merchant —
    /// the description still carries the text for rules to match on.
    #[test]
    fn interest_fee_and_loan_rows_name_no_merchant() {
        for row in [
            r#"2020/01/01,2020010101,INT,,"ASB BANK - INTEREST","CR.INT TO 01/01/2020",1.23"#,
            r#"2021/03/01,2021030101,BANK FEE,,"ACTIVITY FEE","",-5.00"#,
            r#"2023/06/01,2023060101,LOAN INT,,"LOAN - INTEREST","12-3136-0000123-92 006 INTEREST",-1.00"#,
            r#"2023/06/01,2023060101,LOAN PRIN,,"LOAN - PRINCIPAL","12-3136-0000123-92 006 PRINCIPAL",-2.00"#,
        ] {
            let out = one(row);
            assert_eq!(out.merchant, None, "{row}");
            // …and the label is still in the description.
            assert!(!out.description.is_empty(), "{row}");
        }
    }

    /// Their memo is the Particulars/Code/Reference triple, not a card descriptor: the split
    /// account number is rejoined, and the space before `006` — a separate subfield — is kept.
    #[test]
    fn a_loan_memos_subfields_are_not_welded_together() {
        let out = one(
            r#"2023/06/01,2023060101,LOAN INT,,"LOAN - INTEREST","12-3136- 0000123-92 006 INTEREST",-1.00"#,
        );
        assert_eq!(
            out.description,
            "LOAN - INTEREST 12-3136-0000123-92 006 INTEREST"
        );
    }

    /// The point of the whole exercise: these four no longer warn, so a savings or loan export
    /// imports categorised instead of as a wall of uncategorised rows.
    #[test]
    fn the_interest_and_loan_types_no_longer_warn() {
        let out = export(&[
            r#"2020/01/01,2020010101,INT,,"ASB BANK - INTEREST","CR.INT TO 01/01/2020",1.23"#,
            r#"2021/03/01,2021030101,BANK FEE,,"ACTIVITY FEE","",-5.00"#,
            r#"2023/06/01,2023060102,LOAN INT,,"LOAN - INTEREST","12-3136-0000123-92 006 INTEREST",-1.00"#,
            r#"2023/06/01,2023060103,LOAN PRIN,,"LOAN - PRINCIPAL","12-3136-0000123-92 006 PRINCIPAL",-2.00"#,
        ]);
        assert_eq!(out.transactions.len(), 4);
        assert!(
            !out.warnings.iter().any(|w| w.contains("unfamiliar")),
            "{:?}",
            out.warnings
        );
        // Every one but `BANK FEE`, which is recognised and deliberately unclassified.
        assert_eq!(
            out.transactions
                .iter()
                .filter(|t| t.category.is_some())
                .count(),
            3
        );
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

    /// A label this parser doesn't know must not block a seven-year import — and that this
    /// happens is not hypothetical: `INT`, `BANK FEE`, `LOAN INT` and `LOAN PRIN` all arrived
    /// here first, from the savings and loan accounts a chequing-only export never shows. So the
    /// example is a label ASB does not currently emit, not one of those.
    #[test]
    fn an_unfamiliar_transaction_type_warns_but_still_imports() {
        let out = export(&[r#"2024/06/01,2024060101,CHQ,,"CHEQUE","DEPOSIT 001",1.23"#]);
        assert_eq!(out.transactions.len(), 1);
        let row = &out.transactions[0];
        assert_eq!(row.amount_minor, 1_23);
        assert_eq!(row.description, "CHEQUE DEPOSIT 001");
        assert_eq!(row.merchant, None, "an unknown type names no merchant");
        assert!(row.category.is_none(), "and carries no category hint");
        assert!(
            out.warnings.iter().any(|w| w.contains("'CHQ'")),
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
        let cases: [(&str, String, &str); 12] = [
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
            // The one that `%Y/%m/%d` alone lets through: `%Y` takes one to four digits, so
            // this parses as 0020-01-20 unless the year's shape is checked first. It is what
            // Excel writes back on a `d/mm/yy` machine after someone opens the export.
            (
                "a two-digit-year day/month-first date",
                file(&[r#"20/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#]),
                "YYYY/MM/DD",
            ),
            // Four digits and still not a statement date, so the shape check can't catch it.
            (
                "a four-digit year before the epoch",
                file(&[r#"0020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#]),
                "not a plausible statement date",
            ),
            (
                "a four-digit year in the future",
                file(&[r#"3020/01/20,2020012001,EFTPOS,,"S","EFTPOS",-5.00"#]),
                "not a plausible statement date",
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

    // ---------------------------------------------------------------- decoding

    /// The common case must not copy: a valid export is borrowed straight out of the request
    /// body, and only bad bytes pay for a replacement string.
    #[test]
    fn a_valid_body_is_decoded_without_copying_it() {
        assert!(matches!(decode(b"Date,Unique Id\r\n"), Cow::Borrowed(_)));
        // Excel writes a BOM when it saves a CSV; it is stripped off the bytes, so this is
        // still a borrow rather than a re-allocated string.
        let with_bom = b"\xef\xbb\xbfDate,Unique Id\r\n";
        assert!(matches!(decode(with_bom), Cow::Borrowed(_)));
        assert_eq!(decode(with_bom), "Date,Unique Id\r\n");
    }

    /// Lossy on purpose — one odd byte in a merchant name must not sink a seven-year import.
    #[test]
    fn an_odd_byte_in_a_description_is_replaced_not_fatal() {
        let mut bytes =
            file(&[r#"2020/01/20,2020012001,EFTPOS,,"SHOP","EFTPOS",-5.00"#]).into_bytes();
        let at = bytes
            .windows(4)
            .position(|w| w == b"SHOP")
            .expect("the payee");
        bytes[at + 1] = 0xFF; // a byte no UTF-8 sequence can start
        let out = only(parse_upload(&bytes).expect("parses"));
        assert_eq!(out.rows_total, 1);
        assert_eq!(out.transactions[0].description, "S\u{fffd}OP");
    }

    /// The parser's own ceiling, so it holds even if the route in front of it loses its body
    /// limit. Checked before the bytes are decoded, which is where an all-invalid body would
    /// otherwise cost three times its size again.
    #[test]
    fn an_upload_past_the_byte_ceiling_is_refused() {
        let err = parse_upload(&vec![0xFF; limits::UPLOAD_BYTES + 1])
            .expect_err("refused")
            .to_string();
        assert!(err.contains("at most"), "{err:?}");
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

    /// ASB exports one file per account and a zip carries the lot, so aborting on the first bad
    /// one costs every *good* account in it an import — and reports a single filename as if it
    /// were the only problem. The bad file is named and skipped; the rest goes through.
    #[test]
    fn a_bad_file_inside_a_zip_is_named_and_skipped_not_fatal() {
        let bytes = zip(&[
            ("good.csv", &file(&[A])),
            (
                "broken.csv",
                &file(&[r#"2020/01/20,2020012002,EFTPOS,,"X","EFTPOS",twelve"#]),
            ),
        ]);
        let out = parse_upload(&bytes).expect("the good file still parses");
        assert_eq!(out.exports.len(), 1);
        assert_eq!(out.exports[0].sources, ["good.csv"]);
        assert_eq!(out.exports[0].rows_total, 1);
        let named: Vec<&String> = out
            .warnings
            .iter()
            .filter(|w| w.contains("broken.csv"))
            .collect();
        assert_eq!(named.len(), 1, "{:?}", out.warnings);
        assert!(named[0].contains("as an amount"), "{named:?}");
        assert!(named[0].contains("skipped"), "{named:?}");
    }

    /// …but a zip in which *nothing* can be read is still an error, not a cheerful empty
    /// result — and that is the same path a single unreadable CSV takes, so a bare upload
    /// behaves exactly as it did before.
    #[test]
    fn a_zip_where_no_file_can_be_read_is_refused() {
        let broken = file(&[r#"2020/01/20,2020012002,EFTPOS,,"X","EFTPOS",twelve"#]);
        let err = parse_upload(&zip(&[("a.csv", &broken), ("b.csv", &broken)]))
            .expect_err("refused")
            .to_string();
        assert!(err.contains("a.csv"), "{err:?}");
        assert!(err.contains("b.csv"), "{err:?}");
        assert!(err.contains("as an amount"), "{err:?}");
    }

    /// The mixed real-world zip: eight running-balance exports, a plain one, and one file that
    /// cannot be read at all. Everything readable imports.
    #[test]
    fn a_zip_mixing_both_shapes_reads_every_account_it_can() {
        let bytes = zip(&[
            (
                "plain.csv",
                &file_for("0000123-50", "100.00", "20260803", &[A]),
            ),
            (
                "balance.csv",
                &balance_file(
                    "0.00",
                    &[(
                        r#"9/18/20,2020091801,EFTPOS,,"S","EFTPOS",100.00"#,
                        "100.00",
                    )],
                    "100.00",
                )
                .replace("0000123-50", "0000123-51"),
            ),
            ("junk.csv", "not an export at all"),
        ]);
        let out = parse_upload(&bytes).expect("parses");
        assert_eq!(out.exports.len(), 2);
        assert_eq!(out.exports[0].account, "12-3136-0000123-50");
        assert_eq!(out.exports[1].account, "12-3136-0000123-51");
        // The plain one states no opening balance; the running-balance one does.
        assert_eq!(out.exports[0].stated_opening_minor, None);
        assert_eq!(out.exports[1].stated_opening_minor, Some(0));
        assert!(
            out.warnings.iter().any(|w| w.contains("junk.csv")),
            "{:?}",
            out.warnings
        );
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
