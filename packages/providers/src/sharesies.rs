//! Parses a Sharesies personal-export zip (see `scripts/fetch-sharesies-data.sh`) into
//! normalized wallet transactions, holding lots, and dividends. Pure parsing only — no
//! DB, no `TransactionProvider` impl, since there's exactly one implementation of this
//! ever (see `docs/ARCHITECTURE.md`'s "plain functions where polymorphism isn't real").
//! `sure-api` unzips the upload, calls [`parse_export`], and persists the result via
//! `sure_dal::brokerage`.

use std::collections::HashMap;
use std::io::Cursor;

use chrono::DateTime;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;

use sure_app::ports::{ProviderCategory, ProviderTransaction};
use sure_core::{CategoryKind, LotKind};

/// A resolved instrument from `lookup.json` (fund_id -> ticker/name/exchange/currency).
#[derive(Debug, Clone)]
pub struct InstrumentInfo {
    pub symbol: String,
    pub name: String,
    pub exchange: String,
    pub currency: String,
}

/// One buy/sell/corporate-action lot, ready to insert into `holdings`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedHolding {
    pub ticker: String,
    pub exchange: String,
    pub name: Option<String>,
    pub currency_code: String,
    pub trade_date: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: LotKind,
    pub external_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedWithholding {
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

/// One cash distribution, ready to insert into `dividends` (+ its withholdings).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedDividend {
    pub ticker: String,
    pub exchange: String,
    pub record_date: Option<String>,
    pub paid_date: String,
    pub shares_held: Option<f64>,
    pub gross_amount_minor: i64,
    pub net_amount_minor: i64,
    pub currency_code: String,
    pub external_id: String,
    pub withholdings: Vec<ParsedWithholding>,
}

/// The fully-parsed export, ready for `sure_dal::brokerage::import_export`. Each wallet
/// transaction carries a `ProviderCategory` whose `kind` steers reporting: internal money
/// movement (funding a trade, a wallet ↔ bank transfer, an FX conversion) gets a
/// `transfer`-kind category so it's excluded from spend/income reports; dividends/
/// distributions get `income`; a `None` category is left for manual review.
#[derive(Debug, Default)]
pub struct SharesiesExport {
    pub wallet_transactions: Vec<ProviderTransaction>,
    pub holdings: Vec<ParsedHolding>,
    pub dividends: Vec<ParsedDividend>,
    /// Per-record issues that were skipped rather than aborting the whole import.
    pub warnings: Vec<String>,
}

/// Unzips `zip_bytes` and parses whichever of `lookup.json` / `wallet-transactions.json`
/// / `activity.json` it contains (tolerating a folder prefix, e.g.
/// `sharesies-export/activity.json`). CPU-bound (zip decompression + JSON parsing) —
/// callers should run this on a blocking thread (`tokio::task::spawn_blocking`).
pub fn parse_export(zip_bytes: &[u8]) -> anyhow::Result<SharesiesExport> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes))?;
    if archive.len() > crate::zipfile::ENTRIES {
        anyhow::bail!(
            "the zip holds {} files; at most {} are read at once",
            archive.len(),
            crate::zipfile::ENTRIES
        );
    }
    // The three files below are JSON an upload chose the size of, and `serde_json` will
    // happily parse whatever it is handed — so the ceilings have to be applied on the way
    // out of the archive. See `crate::zipfile`.
    let mut budget = crate::zipfile::Budget::default();

    let lookup_bytes = read_entry(&mut archive, &mut budget, "lookup.json")?;
    let wallet_bytes = read_entry(&mut archive, &mut budget, "wallet-transactions.json")?
        .ok_or_else(|| anyhow::anyhow!("zip is missing wallet-transactions.json"))?;
    let activity_bytes = read_entry(&mut archive, &mut budget, "activity.json")?
        .ok_or_else(|| anyhow::anyhow!("zip is missing activity.json"))?;

    let lookup = match &lookup_bytes {
        Some(b) => parse_lookup(b)?,
        None => HashMap::new(),
    };

    let wallet_transactions = parse_wallet_transactions(&wallet_bytes)?;
    let mut export = SharesiesExport {
        wallet_transactions,
        ..Default::default()
    };
    parse_activity(&activity_bytes, &lookup, &mut export)?;
    if lookup_bytes.is_none() {
        export
            .warnings
            .push("lookup.json missing — tickers fall back to raw fund ids".to_string());
    }
    Ok(export)
}

fn read_entry(
    archive: &mut zip::ZipArchive<Cursor<&[u8]>>,
    budget: &mut crate::zipfile::Budget,
    filename: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        if name == filename || name.ends_with(&format!("/{filename}")) {
            let declared = file.size();
            return Ok(Some(budget.read(&name, declared, &mut file)?));
        }
    }
    Ok(None)
}

// --------------------------------------------------------------------------------------
// lookup.json
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct LookupEntry {
    symbol: String,
    name: String,
    exchange: String,
    currency: String,
}

pub fn parse_lookup(bytes: &[u8]) -> anyhow::Result<HashMap<String, InstrumentInfo>> {
    let raw: HashMap<String, LookupEntry> = serde_json::from_slice(bytes)?;
    Ok(raw
        .into_iter()
        .map(|(fund_id, e)| {
            (
                fund_id,
                InstrumentInfo {
                    symbol: e.symbol,
                    name: e.name,
                    exchange: e.exchange,
                    currency: e.currency.to_uppercase(),
                },
            )
        })
        .collect())
}

// --------------------------------------------------------------------------------------
// wallet-transactions.json
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Quantum {
    #[serde(rename = "$quantum")]
    quantum: i64,
}

#[derive(Debug, Deserialize)]
struct WalletDetail {
    #[serde(rename = "type")]
    kind: Option<String>,
    // Present on `fx_order` rows: the source (debit) side of a currency conversion. The
    // row's own `amount`/`currency` are the *target* (credit); this leg is otherwise not
    // emitted as a standalone wallet row. `source_amount` is the gross amount removed from
    // the source wallet (the conversion fee is baked into it — the difference between it
    // and `net_source_amount`).
    source_amount: Option<String>,
    source_currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WalletTxRaw {
    amount: String,
    currency: String,
    description: Option<String>,
    reason: Option<String>,
    key: String,
    timestamp: Quantum,
    #[serde(default)]
    detail: Option<WalletDetail>,
}

pub fn parse_wallet_transactions(bytes: &[u8]) -> anyhow::Result<Vec<ProviderTransaction>> {
    let rows: Vec<WalletTxRaw> = serde_json::from_slice(bytes)?;
    let mut out = Vec::with_capacity(rows.len());

    for r in rows {
        let reason = r.reason.unwrap_or_default().to_lowercase();
        let description = r
            .description
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| reason.clone());
        if is_cancelled_order(&reason, &description) {
            // A cancelled limit-buy holds funds ("holding funds for share purchase", a
            // debit) then returns them ("cancelling buy order", a matching credit) — two
            // wallet rows that net to zero and represent no real activity. Dropping both
            // keeps the balance correct and removes the noise the user saw.
            continue;
        }
        let detail = r.detail;
        let detail_kind = detail
            .as_ref()
            .and_then(|d| d.kind.as_deref())
            .unwrap_or("");

        // A currency conversion records only its *target* (credit) side as the row's
        // amount/currency; the matching *source* (debit) leg is buried in `detail` and is
        // never emitted as its own wallet row. Add it here as a separate transaction so
        // the per-currency balances net out — otherwise every conversion credits one
        // wallet without ever debiting the other, massively inflating the total.
        if detail_kind == "fx_order" {
            if let Some(d) = detail.as_ref() {
                if let (Some(source_amount), Some(source_currency)) =
                    (d.source_amount.as_deref(), d.source_currency.as_deref())
                {
                    out.push(ProviderTransaction {
                        external_id: format!("{}:src", r.key),
                        posted_at: millis_to_rfc3339(r.timestamp.quantum)?,
                        amount_minor: -decimal_to_minor(source_amount)?,
                        currency_code: Some(source_currency.to_uppercase()),
                        description: description.clone(),
                        merchant: None,
                        category: Some(transfer_category()),
                    });
                }
            }
        }

        // The row itself (the target/credit side for an fx_order, or an ordinary movement
        // otherwise). Its external_id/amount/currency are unchanged, so a prior import
        // dedupes to it and only the new source leg above is added on re-import.
        out.push(ProviderTransaction {
            external_id: r.key,
            posted_at: millis_to_rfc3339(r.timestamp.quantum)?,
            amount_minor: decimal_to_minor(&r.amount)?,
            currency_code: Some(r.currency.to_uppercase()),
            description,
            merchant: None,
            category: wallet_category(&reason, detail_kind),
        });
    }
    Ok(out)
}

/// The two legs of a cancelled buy order (see [`parse_wallet_transactions`]). `reason` is
/// already lowercased. The refund leg is unambiguous by reason; the fund-holding leg is
/// only cancelled when Sharesies labels its description "… cancelled" (a filled buy's hold
/// says "Market buy" / "Share purchase plan buy"), so it's scoped to that reason to avoid
/// dropping a real purchase.
fn is_cancelled_order(reason: &str, description: &str) -> bool {
    reason == "cancelling buy order"
        || (reason == "holding funds for share purchase"
            && description.to_lowercase().contains("cancel"))
}

fn transfer_category() -> ProviderCategory {
    ProviderCategory {
        name: "Transfers".to_string(),
        group: None,
        kind: Some(CategoryKind::Transfer),
    }
}
fn income_category(name: &str) -> ProviderCategory {
    ProviderCategory {
        name: name.to_string(),
        group: Some("Investment income".to_string()),
        kind: Some(CategoryKind::Income),
    }
}

/// Classifies a wallet row by its `reason`/`detail.type`. Internal money movement
/// (funding/settling a trade, a wallet ↔ bank transfer, an FX conversion) → a
/// `transfer`-kind category, so it's excluded from spend/income reports and is eligible
/// to be auto-linked to the matching bank transaction. Dividends/distributions → income.
/// An unrecognised reason is left `None` (uncategorized) for manual review.
fn wallet_category(reason: &str, detail_kind: &str) -> Option<ProviderCategory> {
    match detail_kind {
        "withdrawal" | "buy" | "sell" | "fx_order" => return Some(transfer_category()),
        _ => {}
    }
    if reason.contains("customer deposit") {
        return Some(transfer_category());
    }
    if reason.contains("dividend") {
        return Some(income_category("Dividends"));
    }
    if reason.contains("corporate action cash distribution") {
        return Some(income_category("Distributions"));
    }
    if reason.contains("corporate credit") {
        return Some(income_category("Other investment income"));
    }
    if reason.contains("subscription") {
        return Some(ProviderCategory {
            name: "Subscriptions".to_string(),
            group: None,
            kind: Some(CategoryKind::Expense),
        });
    }
    None
}

// --------------------------------------------------------------------------------------
// activity.json
// --------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Trade {
    contract_note_number: String,
    trade_datetime: Quantum,
    share_price: String,
    volume: String,
}

#[derive(Debug, Deserialize)]
struct ActivityItem {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    fund_id: Option<String>,
    state: Option<String>,
    #[serde(default)]
    trades: Vec<Trade>,
    total_transaction_fee: Option<String>,
    // corporate_action (legacy single-record) fields
    action_timestamp: Option<Quantum>,
    action_type: Option<String>,
    order_shares: Option<String>,
    share_price: Option<String>,
    // corporate_action_v2 fields
    record_date: Option<String>,
    settlement_date: Option<String>,
    #[serde(default)]
    outcome_records: Vec<OutcomeRecord>,
}

#[derive(Debug, Deserialize)]
struct OutcomeRecord {
    id: String,
    fund_id: Option<String>,
    currency: Option<String>,
    gross_amount: String,
    net_amount: Option<String>,
    cost_basis_per_share: Option<String>,
    eligibility_record_id: Option<String>,
    #[serde(default)]
    tax_records: Vec<TaxRecord>,
}

#[derive(Debug, Deserialize)]
struct TaxRecord {
    owed_to: String,
    tax_amount: String,
    tax_credit_amount: Option<String>,
    currency: String,
}

pub fn parse_activity(
    bytes: &[u8],
    lookup: &HashMap<String, InstrumentInfo>,
    export: &mut SharesiesExport,
) -> anyhow::Result<()> {
    let items: Vec<ActivityItem> = serde_json::from_slice(bytes)?;
    for item in items {
        if let Err(e) = parse_activity_item(item, lookup, export) {
            export.warnings.push(e.to_string());
        }
    }
    Ok(())
}

fn instrument(
    lookup: &HashMap<String, InstrumentInfo>,
    fund_id: &str,
) -> (String, String, Option<String>, String) {
    match lookup.get(fund_id) {
        Some(i) => (
            i.symbol.clone(),
            i.exchange.clone(),
            Some(i.name.clone()),
            i.currency.clone(),
        ),
        None => (fund_id.to_string(), String::new(), None, "NZD".to_string()),
    }
}

fn parse_activity_item(
    item: ActivityItem,
    lookup: &HashMap<String, InstrumentInfo>,
    export: &mut SharesiesExport,
) -> anyhow::Result<()> {
    match item.kind.as_str() {
        "buy" | "sell" => {
            if item.state.as_deref() != Some("fulfilled") {
                return Ok(()); // cancelled/failed orders never touch holdings
            }
            let fund_id = item
                .fund_id
                .ok_or_else(|| anyhow::anyhow!("activity {} missing fund_id", item.id))?;
            let (ticker, exchange, name, currency_code) = instrument(lookup, &fund_id);
            let sign = if item.kind == "buy" { 1.0 } else { -1.0 };
            let fee_minor = item
                .total_transaction_fee
                .as_deref()
                .map(decimal_to_minor)
                .transpose()?
                .unwrap_or(0);
            for (idx, trade) in item.trades.iter().enumerate() {
                export.holdings.push(ParsedHolding {
                    ticker: ticker.clone(),
                    exchange: exchange.clone(),
                    name: name.clone(),
                    currency_code: currency_code.clone(),
                    trade_date: millis_to_rfc3339(trade.trade_datetime.quantum)?,
                    quantity: sign * parse_f64(&trade.volume)?,
                    unit_price: Some(parse_f64(&trade.share_price)?),
                    // Attribute the whole order's fee to its first fill only, to avoid
                    // double-counting across multiple fills of the same order.
                    fee_minor: if idx == 0 { fee_minor } else { 0 },
                    kind: if item.kind == "buy" {
                        LotKind::Buy
                    } else {
                        LotKind::Sell
                    },
                    external_id: format!("activity:{}:{}", item.id, trade.contract_note_number),
                });
            }
        }
        "corporate_action" => {
            if item.action_type.as_deref() != Some("share_purchase") {
                return Ok(());
            }
            let fund_id = item
                .fund_id
                .ok_or_else(|| anyhow::anyhow!("activity {} missing fund_id", item.id))?;
            let (ticker, exchange, name, currency_code) = instrument(lookup, &fund_id);
            let ts = item
                .action_timestamp
                .ok_or_else(|| anyhow::anyhow!("activity {} missing action_timestamp", item.id))?;
            let shares = item
                .order_shares
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("activity {} missing order_shares", item.id))?;
            export.holdings.push(ParsedHolding {
                ticker,
                exchange,
                name,
                currency_code,
                trade_date: millis_to_rfc3339(ts.quantum)?,
                quantity: parse_f64(shares)?,
                unit_price: item.share_price.as_deref().map(parse_f64).transpose()?,
                fee_minor: 0,
                kind: LotKind::Corporate,
                external_id: format!("corp:{}", item.id),
            });
        }
        "corporate_action_v2" => {
            if item.action_type.as_deref() == Some("VOTE") {
                return Ok(()); // no cash/share impact
            }
            let paid_date = item
                .settlement_date
                .clone()
                .or_else(|| item.record_date.clone());
            for record in item.outcome_records {
                match &record.currency {
                    Some(currency) => {
                        let fund_id = record
                            .fund_id
                            .as_deref()
                            .or(item.fund_id.as_deref())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "activity {} outcome record missing fund_id",
                                    item.id
                                )
                            })?;
                        let (ticker, exchange, _name, _ccy) = instrument(lookup, fund_id);
                        let paid_date = paid_date.clone().ok_or_else(|| {
                            anyhow::anyhow!("activity {} missing settlement/record date", item.id)
                        })?;
                        let gross_amount_minor = decimal_to_minor(&record.gross_amount)?;
                        let net_amount_minor = record
                            .net_amount
                            .as_deref()
                            .map(decimal_to_minor)
                            .transpose()?
                            .unwrap_or(gross_amount_minor);
                        let withholdings = record
                            .tax_records
                            .iter()
                            .map(|t| {
                                Ok(ParsedWithholding {
                                    owed_to: t.owed_to.clone(),
                                    tax_amount_minor: decimal_to_minor(&t.tax_amount)?,
                                    tax_credit_minor: t
                                        .tax_credit_amount
                                        .as_deref()
                                        .map(decimal_to_minor)
                                        .transpose()?,
                                    currency_code: t.currency.to_uppercase(),
                                })
                            })
                            .collect::<anyhow::Result<Vec<_>>>()?;
                        export.dividends.push(ParsedDividend {
                            ticker,
                            exchange,
                            record_date: item.record_date.clone(),
                            paid_date,
                            shares_held: None,
                            gross_amount_minor,
                            net_amount_minor,
                            currency_code: currency.to_uppercase(),
                            external_id: format!("dividend:{}:{}", item.id, record.id),
                            withholdings,
                        });
                    }
                    None => {
                        // Non-cash unit adjustment (e.g. a rights issue / share dividend):
                        // `gross_amount` is a share quantity, not money.
                        let Some(fund_id) = record.fund_id.as_deref() else {
                            continue; // nothing to attribute the units to
                        };
                        let (ticker, exchange, name, currency_code) = instrument(lookup, fund_id);
                        let trade_date = paid_date.clone().ok_or_else(|| {
                            anyhow::anyhow!("activity {} missing settlement/record date", item.id)
                        })?;
                        export.holdings.push(ParsedHolding {
                            ticker,
                            exchange,
                            name,
                            currency_code,
                            trade_date,
                            quantity: parse_f64(&record.gross_amount)?,
                            unit_price: record
                                .cost_basis_per_share
                                .as_deref()
                                .map(parse_f64)
                                .transpose()?,
                            fee_minor: 0,
                            kind: LotKind::Corporate,
                            external_id: format!(
                                "corp-unit:{}:{}",
                                item.id,
                                record
                                    .eligibility_record_id
                                    .as_deref()
                                    .unwrap_or(&record.id)
                            ),
                        });
                    }
                }
            }
        }
        // `type` is Sharesies' own open-ended vocabulary (new activity types can appear
        // upstream without warning), so a catch-all is the legitimate escape hatch here
        // (CLAUDE.md rule 2) rather than a closed domain enum — but silently dropping an
        // unrecognised one would lose real activity with no trace, so it's noted the same
        // way a parse error on a *recognised* type already is (see `parse_activity`'s
        // caller loop, which pushes the `Err` case's message here).
        other => {
            export
                .warnings
                .push(format!("activity {}: unrecognised type '{other}'", item.id));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------------------

fn millis_to_rfc3339(millis: i64) -> anyhow::Result<String> {
    Ok(DateTime::from_timestamp_millis(millis)
        .ok_or_else(|| anyhow::anyhow!("invalid timestamp {millis}"))?
        .to_rfc3339())
}

/// Parse a decimal dollar amount out of an export cell into minor units (cents).
///
/// Both halves have to be checked, and for different reasons. `Decimal`'s `Mul` **panics** on
/// overflow (`panic!("Multiplication overflowed")`; `checked_mul` is the non-panicking form),
/// and `Decimal::MAX` — `79228162514264337593543950335` — parses out of a cell perfectly
/// happily before panicking on the scale-up, so the plain `d * Decimal::from(100)` turned one
/// hostile or corrupt cell in a user-supplied file into an unwind. That is caught by
/// `CatchPanicLayer` rather than killing the process, but it surfaces as an opaque 500 where
/// the caller deserves a 422 naming the cell. `to_i64` then catches the amounts that scale
/// without overflowing `Decimal` but still don't fit an `i64` of cents. Both land on the same
/// "out of range" error the caller already reports per row.
fn decimal_to_minor(s: &str) -> anyhow::Result<i64> {
    let d: Decimal = s
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid amount '{s}'"))?;
    d.checked_mul(Decimal::from(100))
        .and_then(|scaled| scaled.round().to_i64())
        .ok_or_else(|| anyhow::anyhow!("amount '{s}' out of range"))
}

fn parse_f64(s: &str) -> anyhow::Result<f64> {
    s.trim()
        .parse::<f64>()
        .map_err(|_| anyhow::anyhow!("invalid number '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn lookup_fixture() -> HashMap<String, InstrumentInfo> {
        let mut m = HashMap::new();
        m.insert(
            "fund-aapl".to_string(),
            InstrumentInfo {
                symbol: "AAPL".to_string(),
                name: "Apple Inc".to_string(),
                exchange: "NASDAQ".to_string(),
                currency: "USD".to_string(),
            },
        );
        m.insert(
            "fund-airrg".to_string(),
            InstrumentInfo {
                symbol: "AIRRG".to_string(),
                name: "Air New Zealand Rights".to_string(),
                exchange: "NZX".to_string(),
                currency: "NZD".to_string(),
            },
        );
        m
    }

    #[test]
    fn parses_a_multi_fill_buy_order() {
        let json = r#"[{
            "id": "order-1", "type": "buy", "state": "fulfilled", "fund_id": "fund-aapl",
            "total_transaction_fee": "5.00",
            "trades": [
                {"contract_note_number": "cn1", "trade_datetime": {"$quantum": 1700000000000}, "share_price": "150.00", "volume": "10"},
                {"contract_note_number": "cn2", "trade_datetime": {"$quantum": 1700000001000}, "share_price": "150.10", "volume": "0.5"}
            ]
        }]"#;
        let mut export = SharesiesExport::default();
        parse_activity(json.as_bytes(), &lookup_fixture(), &mut export).unwrap();
        assert_eq!(export.holdings.len(), 2);
        assert_eq!(export.holdings[0].quantity, 10.0);
        assert_eq!(export.holdings[0].fee_minor, 500);
        assert_eq!(export.holdings[1].quantity, 0.5);
        assert_eq!(export.holdings[1].fee_minor, 0); // fee only attributed to the first fill
        assert!(export.warnings.is_empty());
    }

    #[test]
    fn excludes_cancelled_orders() {
        let json = r#"[{
            "id": "order-2", "type": "sell", "state": "cancelled", "fund_id": "fund-aapl",
            "trades": []
        }]"#;
        let mut export = SharesiesExport::default();
        parse_activity(json.as_bytes(), &lookup_fixture(), &mut export).unwrap();
        assert!(export.holdings.is_empty());
    }

    #[test]
    fn parses_a_cash_dividend_with_tax_withholding() {
        let json = r#"[{
            "id": "ca-1", "type": "corporate_action_v2", "action_type": "DIVIDEND",
            "record_date": "2025-08-21", "settlement_date": "2025-09-11", "fund_id": "fund-aapl",
            "outcome_records": [{
                "id": "rec-1", "fund_id": "fund-aapl", "currency": "usd",
                "gross_amount": "32.81", "net_amount": "21.96",
                "tax_records": [
                    {"owed_to": "NZ_IRD", "tax_amount": "9.95", "tax_credit_amount": "8.29", "currency": "nzd"},
                    {"owed_to": "US_IRS", "tax_amount": "4.92", "tax_credit_amount": null, "currency": "usd"}
                ]
            }]
        }]"#;
        let mut export = SharesiesExport::default();
        parse_activity(json.as_bytes(), &lookup_fixture(), &mut export).unwrap();
        assert!(export.holdings.is_empty());
        assert_eq!(export.dividends.len(), 1);
        let d = &export.dividends[0];
        assert_eq!(d.ticker, "AAPL");
        assert_eq!(d.gross_amount_minor, 3281);
        assert_eq!(d.net_amount_minor, 2196);
        assert_eq!(d.withholdings.len(), 2);
        assert_eq!(d.withholdings[0].owed_to, "NZ_IRD");
    }

    #[test]
    fn parses_a_non_cash_rights_issue_as_a_holding() {
        let json = r#"[{
            "id": "ca-2", "type": "corporate_action_v2", "action_type": "SHARE_DIVIDEND",
            "record_date": "2022-04-05", "settlement_date": "2022-04-06", "fund_id": "fund-airrg",
            "outcome_records": [{
                "id": "rec-2", "fund_id": "fund-airrg", "currency": null,
                "gross_amount": "1274.518903", "cost_basis_per_share": "0.318264",
                "eligibility_record_id": "elig-1"
            }]
        }]"#;
        let mut export = SharesiesExport::default();
        parse_activity(json.as_bytes(), &lookup_fixture(), &mut export).unwrap();
        assert!(export.dividends.is_empty());
        assert_eq!(export.holdings.len(), 1);
        assert_eq!(export.holdings[0].ticker, "AIRRG");
        assert_eq!(export.holdings[0].quantity, 1274.518903);
        assert_eq!(export.holdings[0].kind, LotKind::Corporate);
    }

    #[test]
    fn skips_vote_records_entirely() {
        let json = r#"[{
            "id": "ca-3", "type": "corporate_action_v2", "action_type": "VOTE", "fund_id": "fund-aapl"
        }]"#;
        let mut export = SharesiesExport::default();
        parse_activity(json.as_bytes(), &lookup_fixture(), &mut export).unwrap();
        assert!(export.holdings.is_empty());
        assert!(export.dividends.is_empty());
        assert!(export.warnings.is_empty());
    }

    #[test]
    fn an_unrecognised_activity_type_is_skipped_with_a_warning_not_dropped_silently() {
        let json = r#"[{
            "id": "future-1", "type": "some_new_activity_type", "fund_id": "fund-aapl"
        }]"#;
        let mut export = SharesiesExport::default();
        parse_activity(json.as_bytes(), &lookup_fixture(), &mut export).unwrap();
        assert!(export.holdings.is_empty());
        assert!(export.dividends.is_empty());
        assert_eq!(export.warnings.len(), 1);
        assert!(export.warnings[0].contains("future-1"));
        assert!(export.warnings[0].contains("some_new_activity_type"));
    }

    #[test]
    fn wallet_transactions_categorize_transfers_and_income() {
        let json = r#"[
            {"amount": "-100.00", "currency": "nzd", "description": "Withdrawal", "reason": "holding funds for withdrawal", "key": "k1", "timestamp": {"$quantum": 1700000000000}, "detail": {"type": "withdrawal"}},
            {"amount": "21.96", "currency": "usd", "description": null, "reason": "dividend payout", "key": "k2", "timestamp": {"$quantum": 1700000001000}}
        ]"#;
        let txns = parse_wallet_transactions(json.as_bytes()).unwrap();
        assert_eq!(txns.len(), 2);
        // A withdrawal is internal money movement → transfer-kind (excluded from reports,
        // eligible for auto-linking to the bank side).
        let w = txns[0].category.as_ref().unwrap();
        assert_eq!(w.name, "Transfers");
        assert_eq!(w.kind, Some(CategoryKind::Transfer));
        // A dividend payout is real investment income.
        let d = txns[1].category.as_ref().unwrap();
        assert_eq!(d.name, "Dividends");
        assert_eq!(d.kind, Some(CategoryKind::Income));
        assert_eq!(txns[1].amount_minor, 2196);
    }

    #[test]
    fn wallet_transactions_drop_both_legs_of_a_cancelled_buy_order() {
        // A cancelled limit buy: funds held then returned (nets to zero), plus a real
        // filled buy and a top-up that must survive. Sharesies labels both cancelled legs
        // "Limit buy – cancelled"; the refund also carries the "cancelling buy order"
        // reason.
        let json = r#"[
            {"amount": "1000.00", "currency": "nzd", "description": "Wallet top up", "reason": "customer deposit", "key": "k1", "timestamp": {"$quantum": 1700000000000}},
            {"amount": "-736.20", "currency": "nzd", "description": "Limit buy – cancelled", "reason": "holding funds for share purchase", "key": "k2", "timestamp": {"$quantum": 1700000001000}, "detail": {"type": "buy"}},
            {"amount": "736.20", "currency": "nzd", "description": "Limit buy – cancelled", "reason": "cancelling buy order", "key": "k3", "timestamp": {"$quantum": 1700000002000}, "detail": {"type": "buy"}},
            {"amount": "-500.00", "currency": "nzd", "description": "Market buy", "reason": "holding funds for share purchase", "key": "k4", "timestamp": {"$quantum": 1700000003000}, "detail": {"type": "buy"}}
        ]"#;
        let txns = parse_wallet_transactions(json.as_bytes()).unwrap();
        // Only the top-up and the real filled buy survive; both cancelled legs are gone.
        let keys: Vec<&str> = txns.iter().map(|t| t.external_id.as_str()).collect();
        assert_eq!(keys, vec!["k1", "k4"]);
        // The surviving buy is a real cash movement out of the wallet.
        assert_eq!(txns[1].amount_minor, -50_000);
    }

    #[test]
    fn fx_orders_emit_both_currency_legs_so_balances_net_out() {
        // A USD→NZD conversion: the export row carries only the +NZD target credit; the
        // −USD source debit must be synthesized so the two wallets net correctly.
        let json = r#"[
            {"amount": "41827.35094612", "currency": "nzd", "description": "Exchange money USD to NZD", "reason": "foreign exchange order", "key": "fx1", "timestamp": {"$quantum": 1700000000000}, "detail": {"type": "fx_order", "source_amount": "24153.67", "source_currency": "usd", "target_amount": "41827.35094612", "target_currency": "nzd"}}
        ]"#;
        let txns = parse_wallet_transactions(json.as_bytes()).unwrap();
        assert_eq!(txns.len(), 2);
        // Source debit leg (new): −24,153.67 USD, distinct external_id.
        assert_eq!(txns[0].external_id, "fx1:src");
        assert_eq!(txns[0].amount_minor, -2_415_367);
        assert_eq!(txns[0].currency_code.as_deref(), Some("USD"));
        // Target credit leg (unchanged): +41,827.35 NZD, original key so a prior import
        // dedupes to it.
        assert_eq!(txns[1].external_id, "fx1");
        assert_eq!(txns[1].amount_minor, 4_182_735);
        assert_eq!(txns[1].currency_code.as_deref(), Some("NZD"));
        // Both legs are internal transfers, excluded from spend/income reports.
        assert_eq!(
            txns[0].category.as_ref().unwrap().kind,
            Some(CategoryKind::Transfer)
        );
        assert_eq!(
            txns[1].category.as_ref().unwrap().kind,
            Some(CategoryKind::Transfer)
        );
    }

    #[test]
    fn parse_export_reads_a_folder_prefixed_zip() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("sharesies-export/lookup.json", opts)
                .unwrap();
            zip.write_all(br#"{"fund-aapl":{"symbol":"AAPL","name":"Apple Inc","exchange":"NASDAQ","currency":"USD"}}"#).unwrap();
            zip.start_file("sharesies-export/wallet-transactions.json", opts)
                .unwrap();
            zip.write_all(br#"[{"amount":"10.00","currency":"nzd","description":"Test","reason":"corporate credit","key":"k1","timestamp":{"$quantum":1700000000000}}]"#).unwrap();
            zip.start_file("sharesies-export/activity.json", opts)
                .unwrap();
            zip.write_all(br#"[]"#).unwrap();
            zip.finish().unwrap();
        }
        let export = parse_export(&buf).unwrap();
        assert_eq!(export.wallet_transactions.len(), 1);
        assert!(export.holdings.is_empty());
        assert!(export.warnings.is_empty());
    }

    #[test]
    fn decimal_to_minor_converts_ordinary_amounts() {
        assert_eq!(decimal_to_minor("10.00").unwrap(), 1_000);
        assert_eq!(decimal_to_minor(" 1234.56 ").unwrap(), 123_456);
        assert_eq!(decimal_to_minor("-7.5").unwrap(), -750);
        assert_eq!(decimal_to_minor("0").unwrap(), 0);
        // Sub-cent precision rounds rather than truncating or erroring.
        assert_eq!(decimal_to_minor("0.006").unwrap(), 1);
        assert_eq!(decimal_to_minor("0.004").unwrap(), 0);
    }

    /// `Decimal::MAX` parses out of a cell fine and used to **panic** on `d * Decimal::from(100)`
    /// (`Multiplication overflowed`), turning a hostile export into a 500. It must be the same
    /// per-row error every other bad cell produces.
    #[test]
    fn a_decimal_max_cell_is_an_error_not_a_panic() {
        for cell in [
            "79228162514264337593543950335",
            "-79228162514264337593543950335",
            // Scales without overflowing `Decimal`, but no longer fits an i64 of cents.
            "92233720368547758.08",
        ] {
            let err = decimal_to_minor(cell).expect_err(cell).to_string();
            assert!(err.contains("out of range"), "{cell}: {err}");
            assert!(err.contains(cell), "{cell}: {err}");
        }
    }

    #[test]
    fn decimal_to_minor_rejects_a_non_numeric_cell() {
        let err = decimal_to_minor("not money").unwrap_err().to_string();
        assert!(err.contains("invalid amount"), "{err}");
    }
}
