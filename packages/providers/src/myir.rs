//! Parses myIR "TAP SLS Transactions" exports into normalized student-loan transactions.
//! Pure parsing only — no DB, no `TransactionProvider` impl, since there's exactly one
//! implementation of this ever (see `docs/ARCHITECTURE.md`'s "plain functions where
//! polymorphism isn't real"), mirroring [`crate::sharesies`]. `sure-api` takes the upload,
//! calls [`parse_export`], and persists the result via `sure_dal::providers`.
//!
//! myIR caps a single export at about two years, so reaching a loan's origination takes
//! several downloads whose windows overlap. Rather than importing each separately and
//! leaning on the database to dedupe, [`parse_export`] takes them all at once — a zip of
//! `.xlsx` files, or one bare `.xlsx` — and reconciles them into a single ledger. That
//! ordering matters: the cross-file checks in [`check_invariants`] can only be made while
//! every export is in hand, and each describes a failure the database could not detect
//! afterwards.
//!
//! The one substantive transformation is the **sign**. IR writes its ledger with a debt
//! increase positive; Sure stores a liability's balance negative, so a repayment — which
//! *reduces* the debt — has to become a positive transaction. Every row is negated, with
//! no per-type special-casing, so a transaction type this parser has never seen still
//! lands the right way round (it is reported through [`MyIrExport::warnings`] so it gets a
//! human glance rather than silent trust).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Cursor;

use calamine::{Data, Reader, Xlsx};
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use sure_app::ports::ProviderTransaction;

/// Every row in an SLS export should carry this account type. Anything else means the
/// download came from a different myIR view and may mix in another tax type, which must
/// never be summed into a student loan's ledger.
const EXPECTED_ACCOUNT_TYPE: &str = "Student loan";

/// Transaction description stems seen in real exports. Not a filter — an unrecognised
/// label is still imported, under the same sign rule — but it is surfaced as a warning so
/// a new kind of movement gets looked at before it is trusted. Stems, because IR appends
/// detail to some of them ("Compulsory course fee - University of Auckland").
const KNOWN_TRANSACTIONS: [&str; 9] = [
    "Repayment deduction",
    "Living costs",
    "Payment",
    "Course related costs",
    "Compulsory course fee",
    "Establishment fee",
    "Administration fee",
    "Direct credit refund",
    "Transfer",
];

/// Ceilings on what an upload may expand to. A myIR export is tens of kilobytes and a
/// handful of them covers a whole loan, so these are orders of magnitude above any honest
/// upload — they exist so a hostile one fails fast instead of exhausting memory or CPU.
/// The HTTP body limit bounds what arrives; these bound what it turns into, which is the
/// part a zip bomb attacks.
mod limits {
    /// Workbooks per upload, and the byte ceilings — shared with the other zip-taking
    /// importers, see [`crate::zipfile`].
    pub use crate::zipfile::{ENTRIES, ENTRY_BYTES};
    /// Transaction rows per workbook. Twenty years of weekly drawdowns is ~1,000.
    pub const ROWS: usize = 100_000;
}

/// One parsed export file, with the window it is authoritative for.
///
/// Rows are indexed by day rather than kept as a flat list: reconciliation asks "what does
/// this export say about day D" once per day per file, and a linear scan there made both
/// the agreement check and the merge quadratic in row count — which a single large but
/// otherwise valid workbook could exploit.
#[derive(Debug, Clone)]
struct Workbook {
    name: String,
    account_id: String,
    /// The `Name:` preamble row — the borrower, as IR writes it (`Surname, Given Names`).
    /// Optional: it is not load-bearing for a single-loan household, and an export shape
    /// that omitted it should still import.
    holder: Option<String>,
    window_from: NaiveDate,
    window_to: NaiveDate,
    by_day: BTreeMap<NaiveDate, Vec<Row>>,
}

impl Workbook {
    fn covers(&self, day: NaiveDate) -> bool {
        self.window_from <= day && day <= self.window_to
    }

    fn rows_on(&self, day: NaiveDate) -> &[Row] {
        self.by_day.get(&day).map_or(&[], Vec::as_slice)
    }

    fn days(&self) -> impl Iterator<Item = NaiveDate> + '_ {
        self.by_day.keys().copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    date: NaiveDate,
    description: String,
    /// Signed minor units exactly as IR writes them: positive *increases* what you owe.
    ir_minor: i64,
}

/// The reconciled result of every export in one upload.
#[derive(Debug, Default, Clone)]
pub struct MyIrExport {
    pub transactions: Vec<ProviderTransaction>,
    /// The SLS account the exports are for, for display.
    pub account_id: String,
    /// Whose loan it is, as IR names them (`Surname, Given Names`) — the only thing in the
    /// file that distinguishes one household member's loan from another's, since the SLS
    /// account id appears nowhere in Sure. Routed on by `sure_app::import::routing`.
    pub holder: Option<String>,
    /// The union of every export's window — what this ledger is complete for.
    pub covered_from: Option<String>,
    pub covered_to: Option<String>,
    /// Non-fatal observations worth a human glance.
    pub warnings: Vec<String>,
}

/// Parse an upload into one reconciled ledger.
///
/// Accepts either a zip of `.xlsx` exports or a single bare `.xlsx` — an xlsx *is* a zip,
/// so the two are told apart by looking for `.xlsx` entries inside, rather than by trusting
/// a filename or a content type.
///
/// Returns **every** row the upload describes. The seam against
/// `sure_app::tasks::balance_delta` — which derives everything from the cutover onward out of
/// the daily balance feed, and would otherwise post the same movement twice — is applied by
/// `sure_app::import`, not here: the cutover belongs to the *target account*, which a parser
/// has no way to know. Holding back afterwards is also what keeps
/// [`external_ids`] stable, since it numbers repeated rows over the merged union and a
/// pre-filtered union would number them differently.
pub fn parse_export(bytes: &[u8]) -> anyhow::Result<MyIrExport> {
    let workbooks = read_workbooks(bytes)?;
    let Some(first) = workbooks.first() else {
        anyhow::bail!("no myIR .xlsx exports found in the upload");
    };
    check_invariants(&workbooks)?;

    let merged = merge(&workbooks);
    let ids = external_ids(&merged);
    let mut warnings = Vec::new();

    let mut unknown: Vec<&str> = merged
        .iter()
        .map(|r| r.description.as_str())
        .filter(|d| !KNOWN_TRANSACTIONS.iter().any(|k| d.starts_with(k)))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        warnings.push(format!(
            "transaction types not seen before, imported under the usual sign rule — check \
             them against the balance: {}",
            unknown.join(", ")
        ));
    }

    let mut transactions = Vec::new();
    for (row, external_id) in merged.iter().zip(ids) {
        transactions.push(ProviderTransaction {
            external_id,
            // Midday UTC matches every other `posted_at` this app writes, so an imported row
            // sorts sensibly against a derived one on the same day.
            posted_at: format!("{}T12:00:00+00:00", iso(row.date)),
            // The sign flip — see the module docs.
            amount_minor: -row.ir_minor,
            currency_code: Some("NZD".to_string()),
            description: row.description.clone(),
            merchant: None,
            category: None,
        });
    }

    Ok(MyIrExport {
        transactions,
        account_id: first.account_id.clone(),
        holder: first.holder.clone(),
        covered_from: workbooks.iter().map(|w| w.window_from).min().map(iso),
        covered_to: workbooks.iter().map(|w| w.window_to).max().map(iso),
        warnings,
    })
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

// --------------------------------------------------------------------------------------
// reading
// --------------------------------------------------------------------------------------

fn read_workbooks(bytes: &[u8]) -> anyhow::Result<Vec<Workbook>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("upload is not a .zip or .xlsx file: {e}"))?;

    let entries: Vec<String> = (0..archive.len())
        .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|name| {
            // Skip macOS's resource-fork shadows, which zipping a Finder selection includes
            // and which are not readable workbooks.
            name.to_lowercase().ends_with(".xlsx") && !name.starts_with("__MACOSX/")
        })
        .collect();

    // An .xlsx is itself a zip, so a bare workbook simply has no .xlsx entries inside it.
    if entries.is_empty() {
        check_expansion("the upload", bytes)?;
        return Ok(vec![read_workbook("export.xlsx", bytes)?]);
    }

    if entries.len() > limits::ENTRIES {
        anyhow::bail!(
            "upload holds {} .xlsx files; at most {} are read at once",
            entries.len(),
            limits::ENTRIES
        );
    }

    let mut out = Vec::new();
    let mut budget = crate::zipfile::Budget::default();
    for name in entries {
        let mut entry = archive.by_name(&name)?;
        let declared = entry.size();
        let buf = budget.read(&name, declared, &mut entry)?;
        drop(entry);

        check_expansion(&name, &buf)?;
        out.push(read_workbook(&name, &buf)?);
    }
    Ok(out)
}

/// Reject a workbook whose *parts* would expand past the limit before handing it to
/// calamine, which decompresses the sheet XML without a ceiling of its own. An `.xlsx` is a
/// zip, so a few kilobytes of upload can declare gigabytes of `sheet1.xml`.
fn check_expansion(label: &str, workbook: &[u8]) -> anyhow::Result<()> {
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(workbook)) else {
        return Ok(()); // not a zip at all — calamine will reject it with a clearer message
    };
    let mut total = 0u64;
    for i in 0..archive.len() {
        let Ok(part) = archive.by_index(i) else {
            continue;
        };
        total = total.saturating_add(part.size());
        if total > limits::ENTRY_BYTES {
            anyhow::bail!(
                "{label} expands to more than {} bytes once decompressed",
                limits::ENTRY_BYTES
            );
        }
    }
    Ok(())
}

/// Pull the preamble and the transaction table out of one workbook.
///
/// The preamble is what makes merging several exports safe: `Account ID` guards against
/// mixing two different loans, and `From`/`To` are the authoritative window — a file is the
/// authority for *every* day inside it, including days on which nothing happened.
fn read_workbook(name: &str, bytes: &[u8]) -> anyhow::Result<Workbook> {
    let mut xlsx: Xlsx<_> = calamine::open_workbook_from_rs(Cursor::new(bytes))
        .map_err(|e| anyhow::anyhow!("{name}: not a readable .xlsx workbook: {e}"))?;
    let sheet_name = xlsx
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{name}: workbook has no sheets"))?;
    let sheet = xlsx.worksheet_range(&sheet_name)?;

    // Keep the preamble's values as typed cells rather than text: `From:`/`To:` carry a
    // date number-format, so calamine hands them over as `DateTime` and their `to_string`
    // is the raw Excel serial, not a date.
    let mut labels: HashMap<String, Data> = HashMap::new();
    let mut header_at = None;
    for (i, row) in sheet.rows().enumerate() {
        let first = cell_text(row.first());
        if first.ends_with(':')
            && row.len() > 1
            && let Some(value) = row.get(1)
        {
            labels.insert(first.trim_end_matches(':').to_lowercase(), value.clone());
        }
        if first == "Period ending" {
            header_at = Some(i);
            break;
        }
    }
    let header_at = header_at.ok_or_else(|| {
        anyhow::anyhow!("{name}: no 'Period ending' header row — is this a TAP SLS export?")
    })?;

    let label = |key: &str| -> anyhow::Result<&Data> {
        labels
            .get(key)
            .filter(|v| !cell_text(Some(v)).is_empty())
            .ok_or_else(|| anyhow::anyhow!("{name}: preamble is missing '{key}'"))
    };
    let account_id = cell_text(Some(label("account id")?));
    // Not `label(..)?`: a missing name costs one routing tier, not the import. `account id`,
    // `from` and `to` are all load-bearing and stay required.
    let holder = labels
        .get("name")
        .map(|v| cell_text(Some(v)))
        .filter(|s| !s.is_empty());
    let window_from = parse_cell_day(Some(label("from")?), name, "preamble 'From'")?;
    let window_to = parse_cell_day(Some(label("to")?), name, "preamble 'To'")?;
    if window_to < window_from {
        anyhow::bail!("{name}: preamble window {window_from}..{window_to} ends before it starts");
    }

    let header: Vec<String> = sheet
        .rows()
        .nth(header_at)
        .map(|r| r.iter().map(|c| cell_text(Some(c))).collect())
        .unwrap_or_default();
    let column = |wanted: &str| -> anyhow::Result<usize> {
        header
            .iter()
            .position(|h| h == wanted)
            .ok_or_else(|| anyhow::anyhow!("{name}: header row has no '{wanted}' column"))
    };
    let (i_type, i_date, i_txn, i_amount) = (
        column("Account type")?,
        column("Date")?,
        column("Transaction")?,
        column("Amount")?,
    );

    let mut by_day: BTreeMap<NaiveDate, Vec<Row>> = BTreeMap::new();
    let mut count = 0usize;
    for (offset, raw) in sheet.rows().skip(header_at + 1).enumerate() {
        let date_cell = raw.get(i_date);
        let amount = cell_text(raw.get(i_amount));
        if cell_text(date_cell).is_empty() || amount.is_empty() {
            continue; // trailing blank rows — an export carries hundreds
        }
        let where_ = format!("row {}", header_at + offset + 2);

        count += 1;
        if count > limits::ROWS {
            anyhow::bail!("{name}: more than {} transaction rows", limits::ROWS);
        }

        let account_type = cell_text(raw.get(i_type));
        if account_type != EXPECTED_ACCOUNT_TYPE {
            anyhow::bail!("{name}: {where_} has account type '{account_type}', not a student loan");
        }
        let row = Row {
            date: parse_cell_day(date_cell, name, &where_)?,
            description: cell_text(raw.get(i_txn)),
            ir_minor: parse_minor(&amount, name, &where_)?,
        };
        by_day.entry(row.date).or_default().push(row);
    }

    Ok(Workbook {
        name: name.to_string(),
        account_id,
        holder,
        window_from,
        window_to,
        by_day,
    })
}

fn cell_text(cell: Option<&Data>) -> String {
    match cell {
        None | Some(Data::Empty) => String::new(),
        Some(Data::String(s)) => s.trim().to_string(),
        Some(other) => other.to_string().trim().to_string(),
    }
}

/// A `Date` column arrives as `Data::DateTime` — calamine's `dates` feature resolves the
/// cell's number format for us — but a re-saved export can carry the same value as text.
fn parse_cell_day(cell: Option<&Data>, file: &str, where_: &str) -> anyhow::Result<NaiveDate> {
    if let Some(Data::DateTime(dt)) = cell
        && let Some(day) = dt.as_datetime().map(|d| d.date())
    {
        return Ok(day);
    }
    parse_day(&cell_text(cell), file, where_)
}

fn parse_day(text: &str, file: &str, where_: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(text.get(..10).unwrap_or(text), "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("{file}: {where_}: cannot read '{text}' as a date"))
}

/// Beyond this an amount isn't a student-loan movement, it's bad data. Bounding it here also
/// means the sign flip in [`parse_export`] can never overflow: negating `i64::MIN` panics.
const MAX_ABS_MINOR: i64 = 1_000_000_000_000_00;

/// Exact 2-dp minor units. Decimal, not float — `329.36` must not land as `32935`.
///
/// Every step is checked. A spreadsheet cell is arbitrary user input, and `Decimal`'s
/// multiply *panics* on overflow rather than returning an error, which turned a hostile
/// amount into a 500 instead of the 422 it should be.
fn parse_minor(text: &str, file: &str, where_: &str) -> anyhow::Result<i64> {
    let cleaned: String = text
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | ' '))
        .collect();
    let value: Decimal = cleaned
        .parse()
        .map_err(|_| anyhow::anyhow!("{file}: {where_}: cannot read '{text}' as an amount"))?;
    let out_of_range = || anyhow::anyhow!("{file}: {where_}: amount '{text}' is out of range");

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

// --------------------------------------------------------------------------------------
// reconciliation
// --------------------------------------------------------------------------------------

/// Every check here is fatal, because each describes a failure the database cannot detect
/// once the rows are in and the balance reconstruction would silently absorb.
fn check_invariants(workbooks: &[Workbook]) -> anyhow::Result<()> {
    // 1. One loan. A different SLS suffix is a different product, not more history.
    let mut ids: Vec<&str> = workbooks.iter().map(|w| w.account_id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() > 1 {
        anyhow::bail!("exports are for different accounts: {}", ids.join(", "));
    }

    // 2. Rows inside their own window, or the window metadata isn't trustworthy and checks
    //    3 and 4 rest on sand.
    for w in workbooks {
        for day in w.days() {
            if !w.covers(day) {
                anyhow::bail!(
                    "{}: row dated {} is outside its window {}..{}",
                    w.name,
                    day,
                    w.window_from,
                    w.window_to
                );
            }
        }
    }

    // 3. No gap in coverage. The dangerous one: a missing window looks exactly like a quiet
    //    period with no activity, and nothing downstream can tell them apart.
    let mut ordered: Vec<&Workbook> = workbooks.iter().collect();
    ordered.sort_by_key(|w| (w.window_from, w.window_to));
    let mut reach = ordered[0].window_to;
    for w in &ordered[1..] {
        if w.window_from > reach + Duration::days(1) {
            anyhow::bail!(
                "gap in coverage: nothing covers {} .. {}. Export that window from myIR and \
                 include it in the upload.",
                reach + Duration::days(1),
                w.window_from - Duration::days(1)
            );
        }
        reach = reach.max(w.window_to);
    }

    // 4. Overlapping windows must agree. Each file is authoritative for every day it
    //    covers, so a disagreement means IR restated something — and because the import is
    //    INSERT OR IGNORE with no update-on-conflict, a restated row would import
    //    *alongside* the stale one and double-count. This check is the only defence.
    for day in sorted_days(workbooks) {
        let covering: Vec<&Workbook> = workbooks.iter().filter(|w| w.covers(day)).collect();
        let Some(first) = covering.first() else {
            continue;
        };
        let expected = signature(first, day);
        for other in &covering[1..] {
            let found = signature(other, day);
            if found != expected {
                anyhow::bail!(
                    "exports disagree about {day}: {} has {:?}, {} has {:?}. Keep the rows from \
                     the export with the newest 'as at', delete the superseded transaction, \
                     then re-upload.",
                    first.name,
                    expected,
                    other.name,
                    found
                );
            }
        }
    }
    Ok(())
}

/// Every day any export has a row for, ascending.
fn sorted_days(workbooks: &[Workbook]) -> BTreeSet<NaiveDate> {
    workbooks.iter().flat_map(Workbook::days).collect()
}

/// A day's rows reduced to a comparable, order-independent shape.
fn signature(workbook: &Workbook, day: NaiveDate) -> Vec<(i64, String)> {
    let mut rows: Vec<(i64, String)> = workbook
        .rows_on(day)
        .iter()
        .map(|r| (r.ir_minor, r.description.clone()))
        .collect();
    rows.sort();
    rows
}

/// The union of every export, ordered by date.
///
/// Safe to take each day's rows from whichever export sorts first, because
/// [`check_invariants`] has already proved that every file covering that day reports
/// exactly the same rows.
fn merge(workbooks: &[Workbook]) -> Vec<Row> {
    let mut ordered: Vec<&Workbook> = workbooks.iter().collect();
    ordered.sort_by(|a, b| (a.window_from, &a.name).cmp(&(b.window_from, &b.name)));

    let mut out = Vec::new();
    for day in sorted_days(workbooks) {
        if let Some(source) = ordered.iter().find(|w| !w.rows_on(day).is_empty()) {
            out.extend(source.rows_on(day).iter().cloned());
        }
    }
    out
}

/// Content-derived, position-independent ids, so re-uploading an overlapping window
/// produces byte-identical ids and the `(provider, external_id)` unique index dedupes.
///
/// The trailing counter disambiguates two genuinely identical rows on one day, and is
/// counted over the *merged union*, never per file: numbering within a single export would
/// give the same row different ids in different exports if a window boundary ever split a
/// day, importing it twice.
fn external_ids(rows: &[Row]) -> Vec<String> {
    let mut seen: HashMap<(NaiveDate, i64, String), usize> = HashMap::new();
    rows.iter()
        .map(|r| {
            let key = (r.date, r.ir_minor, slug(&r.description));
            let n = seen.entry(key.clone()).or_default();
            *n += 1;
            format!("ir-sls:{}:{}:{}:{}", iso(key.0), key.1, key.2, n)
        })
        .collect()
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A cell in the synthetic workbook: text, or a date written the way a real export
    /// writes it — an Excel serial carrying a date number-format.
    enum Cell {
        Text(String),
        Date(NaiveDate),
        Number(&'static str),
    }
    fn t(s: &str) -> Cell {
        Cell::Text(s.to_string())
    }

    /// Build a minimal but real .xlsx. Faithful enough to exercise the path a myIR export
    /// actually takes: dates arrive as serials resolved through the style table, not text.
    fn xlsx(rows: Vec<Vec<Cell>>) -> Vec<u8> {
        let mut sheet = String::from(
            r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>"#,
        );
        for (r, cells) in rows.iter().enumerate() {
            sheet.push_str(&format!(r#"<row r="{}">"#, r + 1));
            for (c, cell) in cells.iter().enumerate() {
                let reference = format!("{}{}", (b'A' + c as u8) as char, r + 1);
                match cell {
                    Cell::Text(s) => sheet.push_str(&format!(
                        r#"<c r="{reference}" t="inlineStr"><is><t>{}</t></is></c>"#,
                        s.replace('&', "&amp;").replace('<', "&lt;")
                    )),
                    Cell::Number(n) => {
                        sheet.push_str(&format!(r#"<c r="{reference}"><v>{n}</v></c>"#))
                    }
                    // Style index 1 is the date format declared in styles.xml below.
                    Cell::Date(day) => {
                        let serial = (*day - d("1899-12-30")).num_days();
                        sheet.push_str(&format!(r#"<c r="{reference}" s="1"><v>{serial}</v></c>"#))
                    }
                }
            }
            sheet.push_str("</row>");
        }
        sheet.push_str("</sheetData></worksheet>");

        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            let mut put = |name: &str, body: &str| {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            };
            put(
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#,
            );
            put(
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            );
            put(
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Transactions" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            );
            put(
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
            );
            put(
                "xl/styles.xml",
                r#"<?xml version="1.0"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#,
            );
            put("xl/worksheets/sheet1.xml", &sheet);
            zip.finish().unwrap();
        }
        buf
    }

    /// One export: preamble, header, then `(date, transaction, ir_amount)` rows.
    fn export(account: &str, from: &str, to: &str, rows: &[(&str, &str, &str)]) -> Vec<u8> {
        export_for(account, Some(HOLDER), from, to, rows)
    }

    /// As [`export`], but says who the borrower is — `None` omits the `Name:` row entirely,
    /// which is the shape the holder tier has to tolerate rather than fail on.
    fn export_for(
        account: &str,
        holder: Option<&str>,
        from: &str,
        to: &str,
        rows: &[(&str, &str, &str)],
    ) -> Vec<u8> {
        let mut grid = vec![vec![t("Account ID:"), t(account)]];
        if let Some(holder) = holder {
            grid.push(vec![t("Name:"), t(holder)]);
        }
        grid.extend([
            // Date cells, not text: a real export number-formats these, so calamine hands
            // them over as serials.
            vec![t("From:"), Cell::Date(d(from))],
            vec![t("To:"), Cell::Date(d(to))],
            vec![t("")],
            vec![t(
                "Disclaimer: This information is correct as at 31-Jul-2026 12:08:50.",
            )],
            vec![
                t("Period ending"),
                t("Account type"),
                t("Date"),
                t("Transaction"),
                t("Amount"),
            ],
        ]);
        for (date, txn, amount) in rows {
            grid.push(vec![
                t("2026-03-31"),
                t(EXPECTED_ACCOUNT_TYPE),
                Cell::Date(d(date)),
                t(txn),
                Cell::Number(Box::leak(amount.to_string().into_boxed_str())),
            ]);
        }
        grid.push(vec![t(""), t(""), t(""), t(""), t("")]); // trailing blank, as real exports have
        xlsx(grid)
    }

    fn bundle(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            for (name, body) in files {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(body).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    const ACCT: &str = "012-345-678-SLS004";
    /// Surname-first with a middle initial, the way IR writes it — the shape the household
    /// matcher has to cope with, not any real person's name.
    const HOLDER: &str = "Reed, Ari K";

    /// The headline transformation: IR signs a debt increase positive, Sure signs a
    /// liability's balance negative, so every row is negated. Get this backwards and years
    /// of repayments invert on the net-worth line.
    #[test]
    fn ir_signs_are_flipped_for_a_liability() {
        let bytes = export(
            ACCT,
            "2024-07-31",
            "2026-07-31",
            &[
                ("2025-04-14", "Repayment deduction", "-400.00"),
                ("2025-04-01", "Administration fee", "40"),
                ("2025-03-10", "Living costs", "222.00"),
            ],
        );
        let out = parse_export(&bytes).unwrap();

        let by_desc: HashMap<&str, i64> = out
            .transactions
            .iter()
            .map(|t| (t.description.as_str(), t.amount_minor))
            .collect();
        // A repayment reduces the debt -> positive on a liability.
        assert_eq!(by_desc["Repayment deduction"], 400_00);
        // A fee and a drawdown increase it -> negative.
        assert_eq!(by_desc["Administration fee"], -40_00);
        assert_eq!(by_desc["Living costs"], -222_00);
        assert_eq!(out.account_id, ACCT);
        assert_eq!(out.covered_from.as_deref(), Some("2024-07-31"));
    }

    /// The `Name:` preamble is the only thing in a myIR export that says *whose* loan it is —
    /// the SLS account id matches no Sure field — so it is carried out of the parser rather
    /// than dropped with the rest of the preamble.
    #[test]
    fn the_borrower_is_read_off_the_preamble() {
        let rows = [("2025-04-14", "Repayment deduction", "-400.00")];
        let out = parse_export(&export(ACCT, "2024-07-31", "2026-07-31", &rows)).unwrap();
        assert_eq!(out.holder.as_deref(), Some(HOLDER));

        // …and an export without one still imports: it costs a routing tier, not the upload.
        let bare = export_for(ACCT, None, "2024-07-31", "2026-07-31", &rows);
        let out = parse_export(&bare).unwrap();
        assert_eq!(out.holder, None);
        assert_eq!(out.transactions.len(), 1);
    }

    /// A bare workbook and a zip of workbooks are both valid uploads, and an .xlsx is
    /// itself a zip — so the two are told apart by content, not by filename.
    #[test]
    fn accepts_a_bare_xlsx_or_a_zip_of_them() {
        let single = export(
            ACCT,
            "2024-07-31",
            "2026-07-31",
            &[("2025-04-14", "Payment", "-11.11")],
        );
        assert_eq!(parse_export(&single).unwrap().transactions.len(), 1);

        let zipped = bundle(&[("exports/sls_2024_2026.xlsx", single.clone())]);
        assert_eq!(parse_export(&zipped).unwrap().transactions.len(), 1);
    }

    /// Overlapping windows are the normal case, not an error: myIR caps an export at ~2
    /// years, so reaching origination means re-downloading windows that overlap.
    #[test]
    fn overlapping_exports_merge_without_duplicating() {
        // The windows overlap over 2023-07-31..2024-07-31, so both exports must report the
        // shared row in it — and it must be imported once, not twice.
        let shared = ("2024-01-15", "Repayment deduction", "-400.00");
        let older = export(
            ACCT,
            "2022-07-31",
            "2024-07-31",
            &[("2023-06-01", "Living costs", "222.00"), shared],
        );
        let newer = export(
            ACCT,
            "2023-07-31",
            "2026-07-31",
            &[shared, ("2025-04-14", "Repayment deduction", "-400.00")],
        );
        let out = parse_export(&bundle(&[("a.xlsx", older), ("b.xlsx", newer)])).unwrap();

        assert_eq!(out.transactions.len(), 3, "the shared row must appear once");
        let ids: HashSet<&str> = out
            .transactions
            .iter()
            .map(|t| t.external_id.as_str())
            .collect();
        assert_eq!(ids.len(), 3, "ids must be unique across the merged union");
        assert_eq!(out.covered_from.as_deref(), Some("2022-07-31"));
        assert_eq!(out.covered_to.as_deref(), Some("2026-07-31"));
    }

    /// Re-uploading the same export set must produce the same ids, or the unique
    /// (provider, external_id) index can't absorb the repeat and every upload would
    /// duplicate the whole ledger.
    #[test]
    fn ids_are_stable_across_uploads() {
        let bytes = export(
            ACCT,
            "2024-07-31",
            "2026-07-31",
            &[("2025-04-14", "Repayment deduction", "-400.00")],
        );
        let first = parse_export(&bytes).unwrap();
        let second = parse_export(&bytes).unwrap();
        assert_eq!(
            first.transactions[0].external_id,
            second.transactions[0].external_id
        );
        assert_eq!(
            first.transactions[0].external_id,
            "ir-sls:2025-04-14:-40000:repayment-deduction:1"
        );
    }

    /// Two genuinely identical rows on one day must not collapse into one id.
    #[test]
    fn identical_rows_on_one_day_get_distinct_ids() {
        let bytes = export(
            ACCT,
            "2024-07-31",
            "2026-07-31",
            &[
                ("2025-04-14", "Living costs", "222.00"),
                ("2025-04-14", "Living costs", "222.00"),
            ],
        );
        let out = parse_export(&bytes).unwrap();
        assert_eq!(out.transactions.len(), 2);
        assert_ne!(
            out.transactions[0].external_id,
            out.transactions[1].external_id
        );
    }

    /// A missing window looks exactly like a quiet period once the rows are in the
    /// database, so it has to be caught here or not at all.
    #[test]
    fn a_gap_between_windows_is_fatal() {
        let a = export(
            ACCT,
            "2021-01-01",
            "2022-01-01",
            &[("2021-06-01", "Living costs", "10")],
        );
        let b = export(
            ACCT,
            "2022-01-03",
            "2023-01-01",
            &[("2022-06-01", "Living costs", "10")],
        );
        let err = parse_export(&bundle(&[("a.xlsx", a), ("b.xlsx", b)])).unwrap_err();
        assert!(err.to_string().contains("gap in coverage"), "{err}");
    }

    /// Windows that merely touch are contiguous, not a gap.
    #[test]
    fn touching_windows_are_not_a_gap() {
        let a = export(
            ACCT,
            "2021-01-01",
            "2022-01-01",
            &[("2021-06-01", "Living costs", "10")],
        );
        let b = export(
            ACCT,
            "2022-01-02",
            "2023-01-01",
            &[("2022-06-01", "Living costs", "10")],
        );
        assert!(parse_export(&bundle(&[("a.xlsx", a), ("b.xlsx", b)])).is_ok());
    }

    /// A restatement: the import is INSERT OR IGNORE with no update-on-conflict, so a
    /// changed row would land beside the stale one and double-count.
    #[test]
    fn exports_that_disagree_about_an_overlapping_day_are_fatal() {
        let a = export(
            ACCT,
            "2021-01-01",
            "2023-01-01",
            &[("2022-06-01", "Living costs", "100")],
        );
        let b = export(
            ACCT,
            "2022-01-01",
            "2024-01-01",
            &[("2022-06-01", "Living costs", "250")],
        );
        let err = parse_export(&bundle(&[("a.xlsx", a), ("b.xlsx", b)])).unwrap_err();
        assert!(
            err.to_string().contains("disagree about 2022-06-01"),
            "{err}"
        );
    }

    /// A row present in one export but missing from another that also covers that day is
    /// the same class of failure as a changed amount.
    #[test]
    fn a_row_missing_from_one_overlapping_export_is_fatal() {
        let a = export(
            ACCT,
            "2021-01-01",
            "2023-01-01",
            &[("2022-06-01", "Living costs", "100")],
        );
        let b = export(ACCT, "2022-01-01", "2024-01-01", &[]);
        let err = parse_export(&bundle(&[("a.xlsx", a), ("b.xlsx", b)])).unwrap_err();
        assert!(
            err.to_string().contains("disagree about 2022-06-01"),
            "{err}"
        );
    }

    #[test]
    fn exports_for_different_loans_are_fatal() {
        let a = export(
            ACCT,
            "2021-01-01",
            "2023-01-01",
            &[("2022-06-01", "Living costs", "10")],
        );
        let b = export("012-345-678-SLS009", "2021-01-01", "2023-01-01", &[]);
        let err = parse_export(&bundle(&[("a.xlsx", a), ("b.xlsx", b)])).unwrap_err();
        assert!(err.to_string().contains("different accounts"), "{err}");
    }

    /// Older windows carry drawdowns and fees this parser has never seen. They are still
    /// imported — the sign rule is uniform — but they are called out for a human glance.
    #[test]
    fn an_unrecognised_transaction_type_is_imported_with_a_warning() {
        let bytes = export(
            ACCT,
            "2024-07-31",
            "2026-07-31",
            &[("2025-04-14", "Voluntary repayment bonus", "-100")],
        );
        let out = parse_export(&bytes).unwrap();

        assert_eq!(out.transactions.len(), 1, "it must not be dropped");
        assert_eq!(out.transactions[0].amount_minor, 100_00);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("Voluntary repayment bonus")),
            "{:?}",
            out.warnings
        );
    }

    /// The seam against the balance-delta task — rows it derives from the balance feed must not
    /// also be imported, or the same movement lands twice — is applied by `sure_app::import`,
    /// against the target account's own cutover. What has to hold *here* is the precondition
    /// that makes moving it there safe: an id depends only on the rows at or before it, so
    /// whether the tail is later held back cannot change the ids of the rows that are kept.
    ///
    /// If ids shifted with the row set, the same movement would import under one id today and a
    /// different one after the cutover moved — and dedupe, which is `(provider, external_id)`,
    /// would see two transactions.
    #[test]
    fn an_id_does_not_depend_on_the_rows_that_come_after_it() {
        let early = [("2026-07-28", "Repayment deduction", "-500.00")];
        let with_tail = [
            ("2026-07-28", "Repayment deduction", "-500.00"),
            ("2026-07-31", "Repayment deduction", "-500.00"),
            ("2026-08-14", "Repayment deduction", "-500.00"),
        ];

        let alone = parse_export(&export(ACCT, "2024-07-31", "2026-07-28", &early)).unwrap();
        let together = parse_export(&export(ACCT, "2024-07-31", "2026-08-31", &with_tail)).unwrap();

        assert_eq!(alone.transactions.len(), 1);
        assert_eq!(
            together.transactions.len(),
            3,
            "every row is returned; holding back is not this parser's job"
        );
        assert_eq!(
            alone.transactions[0].external_id, together.transactions[0].external_id,
            "the first row's id is the same whether or not the later ones came with it"
        );
    }

    /// An export taken from the wrong myIR view could mix in another tax type, which must
    /// never be summed into a student loan's ledger.
    #[test]
    fn a_foreign_account_type_is_fatal() {
        let grid = vec![
            vec![t("Account ID:"), t(ACCT)],
            vec![t("From:"), t("2024-07-31")],
            vec![t("To:"), t("2026-07-31")],
            vec![
                t("Period ending"),
                t("Account type"),
                t("Date"),
                t("Transaction"),
                t("Amount"),
            ],
            vec![
                t("2026-03-31"),
                t("Income tax"),
                Cell::Date(d("2025-04-14")),
                t("Payment"),
                Cell::Number("-100"),
            ],
        ];
        let err = parse_export(&xlsx(grid)).unwrap_err();
        assert!(err.to_string().contains("not a student loan"), "{err}");
    }

    /// A re-saved export can carry its preamble window as plain text rather than a
    /// number-formatted serial; both have to read.
    #[test]
    fn a_text_preamble_window_still_reads() {
        let grid = vec![
            vec![t("Account ID:"), t(ACCT)],
            vec![t("From:"), t("2024-07-31")],
            vec![t("To:"), t("2026-07-31")],
            vec![
                t("Period ending"),
                t("Account type"),
                t("Date"),
                t("Transaction"),
                t("Amount"),
            ],
            vec![
                t("2026-03-31"),
                t(EXPECTED_ACCOUNT_TYPE),
                Cell::Date(d("2025-04-14")),
                t("Repayment deduction"),
                Cell::Number("-400.00"),
            ],
        ];
        let out = parse_export(&xlsx(grid)).unwrap();
        assert_eq!(out.covered_from.as_deref(), Some("2024-07-31"));
        assert_eq!(out.transactions[0].amount_minor, 400_00);
    }

    /// Build a zip whose entries are deflated, so a test can make something that is small on
    /// the wire and enormous once expanded.
    fn deflated(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in files {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(body).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    /// A zip bomb: kilobytes on the wire, far more than the ceiling once expanded. The HTTP
    /// body limit can't see this — only a bound on what the upload turns into can.
    #[test]
    fn a_zip_bomb_entry_is_refused() {
        let bomb = deflated(&[("bomb.xlsx", vec![0u8; (limits::ENTRY_BYTES + 1) as usize])]);
        assert!(
            bomb.len() < 200_000,
            "fixture should be small on the wire, was {}",
            bomb.len()
        );

        let err = parse_export(&bomb).unwrap_err();
        assert!(err.to_string().contains("over the limit"), "{err}");
    }

    /// The same attack one level down: a workbook small enough to pass the entry check whose
    /// *sheet* expands without bound. calamine has no ceiling of its own, so this has to be
    /// caught before the bytes reach it.
    #[test]
    fn a_workbook_whose_sheet_expands_hugely_is_refused() {
        let huge_sheet = deflated(&[
            ("[Content_Types].xml", b"<Types/>".to_vec()),
            (
                "xl/worksheets/sheet1.xml",
                vec![b' '; (limits::ENTRY_BYTES + 1) as usize],
            ),
        ]);
        assert!(huge_sheet.len() < 200_000);

        let err = parse_export(&huge_sheet).unwrap_err();
        assert!(err.to_string().contains("once decompressed"), "{err}");
    }

    #[test]
    fn too_many_workbooks_are_refused() {
        let one = export(ACCT, "2024-07-31", "2026-07-31", &[]);
        let files: Vec<(String, Vec<u8>)> = (0..=limits::ENTRIES)
            .map(|i| (format!("export-{i}.xlsx"), one.clone()))
            .collect();
        let bundle = bundle(
            &files
                .iter()
                .map(|(n, b)| (n.as_str(), b.clone()))
                .collect::<Vec<_>>(),
        );

        let err = parse_export(&bundle).unwrap_err();
        assert!(err.to_string().contains("at most"), "{err}");
    }

    /// A genuinely large export must still parse — and quickly. Reconciliation asks each
    /// workbook about each day, so the row index matters here: the flat scan it replaced was
    /// quadratic, which a single big-but-valid upload was enough to trip.
    #[test]
    fn a_large_but_honest_export_parses() {
        let rows: Vec<(String, &str, &str)> = (0..3_000)
            .map(|i| {
                let day = d("2010-01-01") + Duration::days(i);
                (iso(day), "Living costs", "222.00")
            })
            .collect();
        let borrowed: Vec<(&str, &str, &str)> =
            rows.iter().map(|(a, b, c)| (a.as_str(), *b, *c)).collect();

        let out = parse_export(&export(ACCT, "2009-01-01", "2020-01-01", &borrowed)).unwrap();

        assert_eq!(out.transactions.len(), 3_000);
        assert_eq!(out.transactions[0].amount_minor, -222_00);
        assert_eq!(
            out.transactions
                .iter()
                .map(|t| &t.external_id)
                .collect::<HashSet<_>>()
                .len(),
            3_000
        );
    }

    /// A spreadsheet cell is arbitrary input. `Decimal`'s multiply panics on overflow, so
    /// each of these used to take down the parse with a 500 rather than a clean rejection.
    #[test]
    fn an_absurd_amount_is_rejected_not_panicked_on() {
        for amount in [
            "999999999999999999999999999", // overflows on the ×100
            "-999999999999999999999999999",
            "79228162514264337593543950335", // Decimal::MAX
            "12345678901234567.89",          // parses, but past any sane balance
        ] {
            let bytes = export(
                ACCT,
                "2024-07-31",
                "2026-07-31",
                &[("2025-04-14", "Repayment deduction", amount)],
            );
            let err = parse_export(&bytes).unwrap_err();
            assert!(
                err.to_string().contains("out of range")
                    || err.to_string().contains("as an amount"),
                "{amount}: {err}"
            );
        }
    }

    #[test]
    fn a_non_xlsx_upload_fails_clearly() {
        let err = parse_export(b"this is not a spreadsheet").unwrap_err();
        assert!(err.to_string().contains("not a .zip or .xlsx"), "{err}");
    }

    /// Amounts must not go near a float: 329.36 * 100 is 32935.999... in binary.
    #[test]
    fn amounts_are_exact_to_the_cent() {
        let bytes = export(
            ACCT,
            "2024-07-31",
            "2026-07-31",
            &[("2024-08-14", "Repayment deduction", "-329.36")],
        );
        let out = parse_export(&bytes).unwrap();
        assert_eq!(out.transactions[0].amount_minor, 329_36);
    }
}
