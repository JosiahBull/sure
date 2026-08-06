//! The [`ImportAdapter`] implementations, and the registry that decides which one a blob is.
//!
//! Each adapter is a thin shell over a parser that already existed: it says which source it is,
//! how to recognise a file cheaply, and how to shape the parse into the [`ParsedUpload`] every
//! import shares. Nothing here parses anything itself — the reading lives in [`crate::asb`],
//! [`crate::myir`], [`crate::sharesies`] and [`crate::csv`], where its tests are.
//!
//! **Order matters.** Every format here is, or can be, a zip, and a bare CSV reader will happily
//! accept a bank export it has no business claiming. So [`ImportRegistry::detect`] asks the
//! specific sources first and the general one last, and each `sniff` looks for something only
//! its own format has rather than something its format merely permits.

use sure_app::ports::{
    ImportAdapter, ImportRegistry as ImportRegistryPort, ImportRow, ParsedExtras, ParsedItem,
    ParsedUpload, ProviderTransaction,
};
use sure_core::ImportSource;

use crate::zipfile;

/// The adapters, in detection order. Built by the composition root and injected, like
/// [`crate::Registry`] — nothing in the application core names a concrete parser.
pub struct ImportRegistry {
    adapters: Vec<Box<dyn ImportAdapter>>,
}

impl ImportRegistry {
    /// Specific before general. `CsvUpload` is last on purpose: its `sniff` only asks for a
    /// `date` and an `amount` column, which an ASB export also has, so putting it anywhere
    /// earlier would silently import a bank export without its cutover, its opening balance or
    /// its account routing — an import that looks like it worked and is quietly wrong.
    pub fn new() -> Self {
        Self {
            adapters: vec![
                Box::new(SharesiesAdapter),
                Box::new(MyIrAdapter),
                Box::new(AsbAdapter),
                Box::new(CsvUploadAdapter),
            ],
        }
    }
}

impl Default for ImportRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportRegistryPort for ImportRegistry {
    fn get(&self, source: ImportSource) -> Option<&dyn ImportAdapter> {
        self.adapters
            .iter()
            .find(|a| a.source() == source)
            .map(|b| b.as_ref())
    }

    fn detect(&self, bytes: &[u8]) -> Option<&dyn ImportAdapter> {
        self.adapters
            .iter()
            .find(|a| a.sniff(bytes))
            .map(|b| b.as_ref())
    }
}

/// One parsed row in the shape the import pipeline writes.
fn row(t: ProviderTransaction, is_one_off: bool) -> ImportRow {
    ImportRow {
        external_id: t.external_id,
        posted_at: t.posted_at,
        amount_minor: t.amount_minor,
        currency_code: t.currency_code,
        description: t.description,
        merchant: t.merchant,
        category_name: t.category.as_ref().map(|c| c.name.clone()),
        category_kind: t.category.as_ref().and_then(|c| c.kind),
        category_group: t.category.and_then(|c| c.group),
        is_one_off,
    }
}

// --------------------------------------------------------------------------------------
// ASB
// --------------------------------------------------------------------------------------

pub struct AsbAdapter;

impl ImportAdapter for AsbAdapter {
    fn source(&self) -> ImportSource {
        ImportSource::AsbCsv
    }

    /// A zip holding `.csv` entries, or text whose first lines carry ASB's preamble. The
    /// preamble check is what keeps this from claiming any CSV at all: `Bank`/`Account`/`Ledger
    /// Balance` lines are ASB's own, and a hand-written CSV has none of them.
    fn sniff(&self, bytes: &[u8]) -> bool {
        if let Some(names) = zipfile::entry_names(bytes) {
            return names.iter().any(|n| is_csv_entry(n));
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
        head.contains("Ledger Balance")
            || (head.contains("Bank ") && head.contains("Account ") && head.contains("Branch "))
    }

    fn parse(&self, bytes: &[u8]) -> anyhow::Result<ParsedUpload> {
        let upload = crate::asb::parse_upload(bytes)?;
        Ok(ParsedUpload {
            source: self.source(),
            items: upload
                .exports
                .into_iter()
                .map(|mut export| ParsedItem {
                    source_account: export.account.clone(),
                    label: export.product.clone(),
                    sources: export.sources.clone(),
                    covered_from: export.covered_from.clone(),
                    covered_to: export.covered_to.clone(),
                    stated_closing_minor: export.ledger_balance_minor,
                    // Derived here, before the pipeline can hold anything back: it runs
                    // backwards from the stated closing balance over every row in the file.
                    opening_balance: export.opening_balance_row().map(|t| row(t, true)),
                    warnings: std::mem::take(&mut export.warnings),
                    rows: export
                        .transactions
                        .into_iter()
                        .map(|t| row(t, false))
                        .collect(),
                    extras: ParsedExtras::None,
                })
                .collect(),
            warnings: upload.warnings,
        })
    }

    /// `asb:<account>:<row id>` — the account is everything between the prefix and the last
    /// colon, because an ASB account number contains colons in no shape but the row id might.
    fn source_account_of(&self, external_id: &str) -> Option<String> {
        let rest = external_id.strip_prefix("asb:")?;
        let (account, _) = rest.rsplit_once(':')?;
        Some(account.to_string())
    }
}

fn is_csv_entry(name: &str) -> bool {
    let name = name.rsplit('/').next().unwrap_or(name);
    !name.starts_with('.') && name.to_ascii_lowercase().ends_with(".csv")
}

// --------------------------------------------------------------------------------------
// myIR
// --------------------------------------------------------------------------------------

pub struct MyIrAdapter;

impl ImportAdapter for MyIrAdapter {
    fn source(&self) -> ImportSource {
        ImportSource::MyirSls
    }

    /// A zip of `.xlsx` files, or a bare workbook. An `.xlsx` *is* a zip, so "is this a zip?"
    /// answers nothing: a bare workbook is recognised by the OOXML part every one of them has
    /// (`xl/workbook.xml`), and a zip of workbooks by holding entries named `.xlsx`. Same
    /// discrimination `crate::myir::read_workbooks` makes, and for the same reason.
    fn sniff(&self, bytes: &[u8]) -> bool {
        let Some(names) = zipfile::entry_names(bytes) else {
            return false;
        };
        names
            .iter()
            .any(|n| n.to_ascii_lowercase().ends_with(".xlsx"))
            || names.iter().any(|n| n == "xl/workbook.xml")
    }

    fn parse(&self, bytes: &[u8]) -> anyhow::Result<ParsedUpload> {
        let export = crate::myir::parse_export(bytes)?;
        Ok(ParsedUpload {
            source: self.source(),
            items: vec![ParsedItem {
                source_account: export.account_id,
                label: None,
                // A loan's exports are the whole upload, so the file names add nothing a
                // preview would show; the window is what says which downloads these were.
                sources: Vec::new(),
                covered_from: export.covered_from,
                covered_to: export.covered_to,
                // myIR states no closing balance: its exports are transaction lists, and the
                // loan's balance comes from the feed. So nothing to reconcile against, and no
                // opening balance to work back from — a loan's exports reach origination.
                stated_closing_minor: None,
                opening_balance: None,
                rows: export
                    .transactions
                    .into_iter()
                    .map(|t| row(t, false))
                    .collect(),
                extras: ParsedExtras::None,
                warnings: export.warnings,
            }],
            warnings: Vec::new(),
        })
    }

    /// `ir-sls:<date>:<minor>:<slug>:<n>` — the SLS account isn't in the id at all, so a repeat
    /// upload routes by the other tiers (an assignment, or the only student loan there is).
    fn source_account_of(&self, _external_id: &str) -> Option<String> {
        None
    }
}

// --------------------------------------------------------------------------------------
// Sharesies
// --------------------------------------------------------------------------------------

pub struct SharesiesAdapter;

/// What the export names its wallet file. The one entry a Sharesies zip must contain, which
/// makes it the thing to recognise it by.
const WALLET_ENTRY: &str = "wallet-transactions.json";

/// The one thing a Sharesies export describes. Its files name no account, so unlike a bank
/// export there is nothing to echo back — this stands in as the key an assignment addresses and
/// the previous-import tier matches on.
const SHARESIES_ACCOUNT: &str = "sharesies";

/// The parser's holding shape into the port's. Two structs with the same fields, because
/// `sure-app` owns the port's and may not depend on this crate — see `ports`' module docs.
fn holding(h: crate::sharesies::ParsedHolding) -> sure_app::ports::HoldingImport {
    sure_app::ports::HoldingImport {
        ticker: h.ticker,
        exchange: h.exchange,
        name: h.name,
        currency_code: h.currency_code,
        trade_date: h.trade_date,
        quantity: h.quantity,
        unit_price: h.unit_price,
        fee_minor: h.fee_minor,
        kind: h.kind,
        external_id: h.external_id,
    }
}

fn dividend(d: crate::sharesies::ParsedDividend) -> sure_app::ports::DividendImport {
    sure_app::ports::DividendImport {
        ticker: d.ticker,
        exchange: d.exchange,
        record_date: d.record_date,
        paid_date: d.paid_date,
        shares_held: d.shares_held,
        gross_amount_minor: d.gross_amount_minor,
        net_amount_minor: d.net_amount_minor,
        currency_code: d.currency_code,
        external_id: d.external_id,
        withholdings: d
            .withholdings
            .into_iter()
            .map(|w| sure_app::ports::WithholdingImport {
                owed_to: w.owed_to,
                tax_amount_minor: w.tax_amount_minor,
                tax_credit_minor: w.tax_credit_minor,
                currency_code: w.currency_code,
            })
            .collect(),
    }
}

impl ImportAdapter for SharesiesAdapter {
    fn source(&self) -> ImportSource {
        ImportSource::SharesiesZip
    }

    /// First in the order, and the most specific sniff of the four: a zip containing
    /// `wallet-transactions.json`. Tolerates a folder prefix, the way the parser does, because
    /// unzipping and re-zipping an export on the way here adds one.
    fn sniff(&self, bytes: &[u8]) -> bool {
        zipfile::entry_names(bytes).is_some_and(|names| {
            names
                .iter()
                .any(|n| n.rsplit('/').next().unwrap_or(n) == WALLET_ENTRY)
        })
    }

    fn parse(&self, bytes: &[u8]) -> anyhow::Result<ParsedUpload> {
        let export = crate::sharesies::parse_export(bytes)?;
        Ok(ParsedUpload {
            source: self.source(),
            items: vec![ParsedItem {
                source_account: SHARESIES_ACCOUNT.to_string(),
                label: None,
                sources: Vec::new(),
                covered_from: None,
                covered_to: None,
                stated_closing_minor: None,
                opening_balance: None,
                rows: export
                    .wallet_transactions
                    .into_iter()
                    .map(|t| row(t, false))
                    .collect(),
                extras: ParsedExtras::Brokerage {
                    holdings: export.holdings.into_iter().map(holding).collect(),
                    dividends: export.dividends.into_iter().map(dividend).collect(),
                },
                warnings: export.warnings,
            }],
            warnings: Vec::new(),
        })
    }

    /// Every id this source writes belongs to the one thing it describes, so any of them
    /// identifies it — which is what lets a re-upload find the account it went to last time.
    fn source_account_of(&self, _external_id: &str) -> Option<String> {
        Some(SHARESIES_ACCOUNT.to_string())
    }
}

// --------------------------------------------------------------------------------------
// a plain CSV someone assembled
// --------------------------------------------------------------------------------------

pub struct CsvUploadAdapter;

/// A hand-written CSV names no account, so it is always routed by an assignment. One key, like
/// Sharesies, rather than a made-up one per upload — otherwise every re-upload would look like a
/// new account and need assigning again.
const CSV_ACCOUNT: &str = "csv";

impl ImportAdapter for CsvUploadAdapter {
    fn source(&self) -> ImportSource {
        ImportSource::CsvUpload
    }

    /// Last in the order. Text (not a zip) with a `date` and an `amount` column — which an ASB
    /// export also has, hence last: [`AsbAdapter`] gets to claim its own files first.
    fn sniff(&self, bytes: &[u8]) -> bool {
        if zipfile::is_zip(bytes) {
            return false;
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]);
        crate::csv::has_required_columns(&head)
    }

    fn parse(&self, bytes: &[u8]) -> anyhow::Result<ParsedUpload> {
        let text = std::str::from_utf8(bytes)?;
        let rows = crate::csv::parse_rows(text)?;
        let dates = || rows.iter().filter_map(|r| r.posted_at.get(..10));
        Ok(ParsedUpload {
            source: self.source(),
            items: vec![ParsedItem {
                source_account: CSV_ACCOUNT.to_string(),
                label: None,
                sources: Vec::new(),
                covered_from: dates().min().map(str::to_string),
                covered_to: dates().max().map(str::to_string),
                stated_closing_minor: None,
                opening_balance: None,
                rows: rows.into_iter().map(|t| row(t, false)).collect(),
                extras: ParsedExtras::None,
                warnings: Vec::new(),
            }],
            warnings: Vec::new(),
        })
    }

    fn source_account_of(&self, _external_id: &str) -> Option<String> {
        Some(CSV_ACCOUNT.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A STORE-only zip, enough to exercise the sniffs. The real archives come from the
    /// parsers' own fixtures.
    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, body) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(body).unwrap();
            }
            w.finish().unwrap();
        }
        out
    }

    const ASB_CSV: &[u8] = b"Created date / time : 3 August 2026 / 16:27:53\r\n\
Bank 12; Branch 3136; Account 0000123-50 (Streamline)\r\n\
From date 20190101\r\n\
To date 20260803\r\n\
Ledger Balance : 100.00 as of 20260803\r\n\
\r\n\
Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n\
2026/07/27,2026072701,EFTPOS,,\"SHOP\",\"EFTPOS\",-5.00\r\n";

    const PLAIN_CSV: &[u8] = b"date,amount,description\n2026-01-05,-12.50,Coffee\n";

    fn detected(bytes: &[u8]) -> Option<ImportSource> {
        ImportRegistry::new().detect(bytes).map(|a| a.source())
    }

    #[test]
    fn each_source_is_recognised_from_its_own_file() {
        assert_eq!(detected(ASB_CSV), Some(ImportSource::AsbCsv));
        assert_eq!(detected(PLAIN_CSV), Some(ImportSource::CsvUpload));
        assert_eq!(
            detected(&zip_of(&[("export.csv", ASB_CSV)])),
            Some(ImportSource::AsbCsv)
        );
        assert_eq!(
            detected(&zip_of(&[("sls_2019.xlsx", b"PK\x03\x04nonsense")])),
            Some(ImportSource::MyirSls)
        );
        assert_eq!(
            detected(&zip_of(&[("xl/workbook.xml", b"<workbook/>")])),
            Some(ImportSource::MyirSls),
            "a bare workbook is a zip too, and must not be read as one *of* workbooks"
        );
        assert_eq!(
            detected(&zip_of(&[(WALLET_ENTRY, b"[]"), ("activity.json", b"[]"),])),
            Some(ImportSource::SharesiesZip)
        );
    }

    /// Two independent guards keep a bank export away from the plain CSV reader, which would
    /// import it without its cutover, its opening balance or its account routing — and report
    /// success.
    ///
    /// The first is that an ASB export's header is not its first line, and
    /// `csv::has_required_columns` only looks there. The second is the registry's order, and it
    /// is the one that has to hold: the moment a bank's export *does* lead with its header, the
    /// first guard is gone and only being asked last keeps the CSV reader from claiming it.
    #[test]
    fn a_file_both_readers_would_take_goes_to_the_specific_one() {
        assert!(
            !CsvUploadAdapter.sniff(ASB_CSV),
            "ASB's header sits under a preamble, so the plain reader passes on it"
        );

        // The same export with its header first: now both sniffs say yes, and the order decides.
        let header_first = b"Date,Unique Id,Tran Type,Cheque Number,Payee,Memo,Amount\r\n\
2026/07/27,2026072701,EFTPOS,,\"SHOP\",\"EFTPOS\",-5.00\r\n\
Ledger Balance : 100.00 as of 20260803\r\n";
        assert!(CsvUploadAdapter.sniff(header_first));
        assert!(AsbAdapter.sniff(header_first));
        assert_eq!(detected(header_first), Some(ImportSource::AsbCsv));
    }

    /// …and the same hazard between the two zip sources: a Sharesies export unzipped and
    /// re-zipped by a file manager gains a folder prefix, and must still be itself.
    #[test]
    fn a_folder_prefixed_sharesies_export_is_still_a_sharesies_export() {
        let bytes = zip_of(&[
            ("export/", b""),
            (&format!("export/{WALLET_ENTRY}"), b"[]"),
            ("export/activity.json", b"[]"),
        ]);
        assert_eq!(detected(&bytes), Some(ImportSource::SharesiesZip));
    }

    #[test]
    fn something_that_is_no_export_at_all_is_recognised_as_nothing() {
        assert_eq!(detected(b"hello"), None);
        assert_eq!(detected(b""), None);
        assert_eq!(detected(&zip_of(&[("notes.txt", b"hi")])), None);
        // A CSV with no amount column is not a transaction list.
        assert_eq!(detected(b"date,payee\n2026-01-05,Shop\n"), None);
    }

    #[test]
    fn every_source_has_an_adapter_reachable_by_name() {
        let registry = ImportRegistry::new();
        for source in [
            ImportSource::AsbCsv,
            ImportSource::MyirSls,
            ImportSource::SharesiesZip,
            ImportSource::CsvUpload,
        ] {
            let adapter = registry
                .get(source)
                .unwrap_or_else(|| panic!("no adapter for {}", source.as_str()));
            assert_eq!(adapter.source(), source);
        }
    }

    /// The tier that makes a re-upload route itself: the ids an import wrote have to give back
    /// the key it was routed under.
    #[test]
    fn an_asb_row_id_gives_back_the_account_it_was_written_for() {
        assert_eq!(
            AsbAdapter.source_account_of("asb:12-3456-0000123-50:2026072701"),
            Some("12-3456-0000123-50".to_string())
        );
        assert_eq!(
            AsbAdapter.source_account_of("asb:12-3456-0000123-50:opening"),
            Some("12-3456-0000123-50".to_string())
        );
        // Another source's id, and a malformed one, yield nothing rather than a wrong answer.
        assert_eq!(
            AsbAdapter.source_account_of("ir-sls:2026-01-01:100:x:0"),
            None
        );
        assert_eq!(AsbAdapter.source_account_of("asb:no-colon"), None);
    }

    #[test]
    fn an_asb_export_parses_into_one_item_with_its_opening_balance() {
        let upload = AsbAdapter.parse(ASB_CSV).expect("parses");
        assert_eq!(upload.source, ImportSource::AsbCsv);
        assert_eq!(upload.items.len(), 1);
        let item = &upload.items[0];
        assert_eq!(item.source_account, "12-3136-0000123-50");
        assert_eq!(item.label.as_deref(), Some("Streamline"));
        assert_eq!(item.rows.len(), 1);
        assert_eq!(item.stated_closing_minor, Some(100_00));
        // 100.00 closing after spending 5.00 means it opened at 105.00.
        let opening = item.opening_balance.as_ref().expect("an opening balance");
        assert_eq!(opening.amount_minor, 105_00);
        assert!(
            opening.is_one_off,
            "it moves value without being spend or income"
        );
    }

    #[test]
    fn a_plain_csv_parses_into_one_item_spanning_its_dates() {
        let upload = CsvUploadAdapter
            .parse(b"date,amount,description\n2026-01-05,-12.50,Coffee\n2026-02-01,20,Refund\n")
            .expect("parses");
        let item = &upload.items[0];
        assert_eq!(item.rows.len(), 2);
        assert_eq!(item.covered_from.as_deref(), Some("2026-01-05"));
        assert_eq!(item.covered_to.as_deref(), Some("2026-02-01"));
        // Nothing to reconcile and nothing to open from: a hand-written list says neither.
        assert!(item.stated_closing_minor.is_none());
        assert!(item.opening_balance.is_none());
    }
}
