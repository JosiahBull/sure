//! Reference [`TransactionProvider`] that imports from CSV text supplied as the sync
//! payload. Columns (case-insensitive): `date`, `amount` (required); `description`,
//! `merchant`, `external_id`, `currency` (optional). No credentials needed, so it's
//! ideal for exercising the provider machinery end-to-end.

use async_trait::async_trait;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use sure_app::ports::{ProviderTransaction, SyncContext, TransactionProvider};

/// Beyond this an amount isn't money, it's bad data. Same ceiling the ASB and myIR importers
/// use, and it is what keeps a hostile payload from writing a quadrillion-dollar row.
const MAX_ABS_MINOR: i64 = 1_000_000_000_000_00;

pub struct CsvProvider;

#[async_trait]
impl TransactionProvider for CsvProvider {
    fn kind(&self) -> &'static str {
        "csv"
    }

    fn description(&self) -> &'static str {
        "Paste or upload rows exported from your bank as CSV"
    }

    fn accepts_payload(&self) -> bool {
        true
    }

    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>> {
        let payload = ctx
            .payload
            .ok_or_else(|| anyhow::anyhow!("CSV provider requires a payload"))?;

        let mut reader = csv::ReaderBuilder::new()
            .trim(csv::Trim::All)
            .flexible(true)
            .from_reader(payload.as_bytes());

        let headers = reader.headers()?.clone();
        let col = |name: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(name));
        let i_date = col("date").ok_or_else(|| anyhow::anyhow!("missing 'date' column"))?;
        let i_amount = col("amount").ok_or_else(|| anyhow::anyhow!("missing 'amount' column"))?;
        let i_desc = col("description");
        let i_merchant = col("merchant");
        let i_ext = col("external_id");
        let i_ccy = col("currency");

        let get = |rec: &csv::StringRecord, i: Option<usize>| -> Option<String> {
            i.and_then(|i| rec.get(i))
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        };

        let mut out = Vec::new();
        for record in reader.records() {
            let record = record?;
            let date = record.get(i_date).unwrap_or_default().to_string();
            if date.is_empty() {
                continue; // skip blank rows
            }
            let amount_str = record.get(i_amount).unwrap_or("0");
            let amount_minor = parse_amount(amount_str)?;
            let description = get(&record, i_desc).unwrap_or_default();
            let external_id =
                get(&record, i_ext).unwrap_or_else(|| format!("{date}|{amount_str}|{description}"));
            out.push(ProviderTransaction {
                external_id,
                posted_at: date,
                amount_minor,
                currency_code: get(&record, i_ccy).map(|s| s.to_uppercase()),
                description,
                merchant: get(&record, i_merchant),
                category: None,
            });
        }
        Ok(out)
    }
}

/// Parse a human-written amount (`-1,234.56`, `$5.00`) into 2-dp minor units.
///
/// `Decimal`, not `f64`, and bounded — the payload is arbitrary text from a request body.
/// Parsing as a float accepted `1e400` and `inf` (both saturating to `i64::MAX`, i.e. a
/// $92-quadrillion transaction) and `NaN` (silently zero); `Decimal` rejects all three, and
/// the range check catches whatever is merely absurd. Mirrors `asb::parse_minor` and
/// `myir::parse_minor`, which guard the same way.
fn parse_amount(s: &str) -> anyhow::Result<i64> {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, '$' | ',' | ' ' | '£' | '€'))
        .collect();
    let value: Decimal = cleaned
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid amount '{s}'"))?;
    let out_of_range = || anyhow::anyhow!("amount '{s}' is out of range");
    // `Decimal`'s multiply panics on overflow rather than returning an error, so it's checked.
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

    #[test]
    fn reads_a_human_written_amount_exactly() {
        for (text, want) in [
            ("-1,234.56", -123_456),
            ("$5.00", 500),
            // The case a float gets wrong: 329.36 * 100.0 is 32935.999… in binary.
            ("329.36", 32_936),
            ("0", 0),
        ] {
            assert_eq!(parse_amount(text).unwrap(), want, "amount {text:?}");
        }
    }

    /// What an arbitrary request body can contain. None of these may become a transaction.
    #[test]
    fn refuses_an_amount_that_isnt_money() {
        for text in [
            "1e400",
            "-1e400",
            "inf",
            "-inf",
            "NaN",
            "",
            "twelve",
            "1.2.3",
            "--5",
            "99999999999999999999",
            "9e18",
        ] {
            assert!(
                parse_amount(text).is_err(),
                "amount {text:?} should be refused"
            );
        }
    }
}
