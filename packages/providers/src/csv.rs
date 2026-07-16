//! Reference [`TransactionProvider`] that imports from CSV text supplied as the sync
//! payload. Columns (case-insensitive): `date`, `amount` (required); `description`,
//! `merchant`, `external_id`, `currency` (optional). No credentials needed, so it's
//! ideal for exercising the provider machinery end-to-end.

use async_trait::async_trait;

use super::{ProviderTransaction, SyncContext, TransactionProvider};

pub struct CsvProvider;

#[async_trait]
impl TransactionProvider for CsvProvider {
    fn kind(&self) -> &'static str {
        "csv"
    }

    fn description(&self) -> &'static str {
        "Import from CSV text (columns: date, amount, description, [merchant], [external_id], [currency])"
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
            let external_id = get(&record, i_ext)
                .unwrap_or_else(|| format!("{date}|{amount_str}|{description}"));
            out.push(ProviderTransaction {
                external_id,
                posted_at: date,
                amount_minor,
                currency_code: get(&record, i_ccy).map(|s| s.to_uppercase()),
                description,
                merchant: get(&record, i_merchant),
            });
        }
        Ok(out)
    }
}

/// Parse a human-written amount (`-1,234.56`, `$5.00`) into 2-dp minor units.
fn parse_amount(s: &str) -> anyhow::Result<i64> {
    let cleaned: String = s.chars().filter(|c| !matches!(c, '$' | ',' | ' ' | '£' | '€')).collect();
    let value: f64 = cleaned
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid amount '{s}'"))?;
    Ok((value * 100.0).round() as i64)
}
