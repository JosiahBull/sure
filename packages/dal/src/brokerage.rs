//! Brokerage holdings ledger, computed positions/wallet balances, dividend detail, and
//! the bulk-import writer. The price-lookup/FX side that turns positions into a valued
//! snapshot lives in `sure_api::brokerage` (it needs the stock-price provider); this
//! module is pure persistence, mirroring the split used by `equity`.

use sqlx::FromRow;
use sure_core::{AppError, AppResult};
pub use sure_core::{
    Dividend, DividendDetail, DividendWithholding, HoldingLot, SaveHoldingLot,
};

use crate::Db;

// ---- holdings CRUD -------------------------------------------------------

pub async fn list_holdings(db: &Db, account_id: i64) -> AppResult<Vec<HoldingLot>> {
    Ok(sqlx::query_as::<_, HoldingLot>(
        "SELECT * FROM holdings WHERE account_id=?1 ORDER BY date(trade_date), id",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?)
}

pub async fn create_holding(db: &Db, account_id: i64, input: SaveHoldingLot) -> AppResult<HoldingLot> {
    let ticker = input.ticker.trim().to_uppercase();
    if ticker.is_empty() {
        return Err(AppError::validation("ticker is required"));
    }
    if input.quantity == 0.0 {
        return Err(AppError::validation("quantity must be non-zero"));
    }
    sqlx::query_as::<_, HoldingLot>(
        "INSERT INTO holdings
            (account_id, ticker, exchange, name, currency_code, trade_date, quantity,
             unit_price, fee_minor, kind)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) RETURNING *",
    )
    .bind(account_id)
    .bind(&ticker)
    .bind(input.exchange.trim())
    .bind(&input.name)
    .bind(input.currency_code.to_uppercase())
    .bind(input.trade_date.trim())
    .bind(input.quantity)
    .bind(input.unit_price)
    .bind(input.fee_minor)
    .bind(&input.kind)
    .fetch_one(db)
    .await
    .map_err(map_fk)
}

pub async fn delete_holding(db: &Db, id: i64) -> AppResult<()> {
    let res = sqlx::query("DELETE FROM holdings WHERE id=?1")
        .bind(id)
        .execute(db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound("holding"));
    }
    Ok(())
}

// ---- dividends -----------------------------------------------------------

pub async fn list_dividends(db: &Db, account_id: i64) -> AppResult<Vec<DividendDetail>> {
    let dividends = sqlx::query_as::<_, Dividend>(
        "SELECT * FROM dividends WHERE account_id=?1 ORDER BY date(paid_date) DESC, id DESC",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?;
    let mut out = Vec::with_capacity(dividends.len());
    for dividend in dividends {
        let withholdings = sqlx::query_as::<_, DividendWithholding>(
            "SELECT * FROM dividend_withholdings WHERE dividend_id=?1 ORDER BY id",
        )
        .bind(dividend.id)
        .fetch_all(db)
        .await?;
        out.push(DividendDetail { dividend, withholdings });
    }
    Ok(out)
}

// ---- computed positions / wallet balances --------------------------------

/// A ticker's net held quantity as of a date (before pricing — see `sure_api::brokerage`).
#[derive(FromRow)]
pub struct PositionRow {
    pub ticker: String,
    pub exchange: String,
    pub currency_code: String,
    pub name: Option<String>,
    pub quantity: f64,
}

/// Net quantity held per `(ticker, exchange)` as of `as_of`, dropping fully-exited
/// positions (and float dust from fractional buy/sell rounding).
pub async fn positions_at(db: &Db, account_id: i64, as_of: &str) -> AppResult<Vec<PositionRow>> {
    Ok(sqlx::query_as::<_, PositionRow>(
        "SELECT ticker, exchange, currency_code, MAX(name) AS name, SUM(quantity) AS quantity
         FROM holdings
         WHERE account_id=?1 AND date(trade_date) <= date(?2)
         GROUP BY ticker, exchange
         HAVING ABS(SUM(quantity)) > 0.0000001
         ORDER BY ticker",
    )
    .bind(account_id)
    .bind(as_of)
    .fetch_all(db)
    .await?)
}

#[derive(FromRow)]
pub struct WalletRow {
    pub currency_code: String,
    pub amount_minor: i64,
}

/// Cash balance per currency as of `as_of` — every transaction on a brokerage account is
/// a wallet-cash movement (imported or manual), summed per currency (a brokerage account
/// legitimately holds several currencies at once).
pub async fn wallet_balances_at(db: &Db, account_id: i64, as_of: &str) -> AppResult<Vec<WalletRow>> {
    Ok(sqlx::query_as::<_, WalletRow>(
        "SELECT currency_code, CAST(SUM(amount_minor) AS INTEGER) AS amount_minor
         FROM transactions
         WHERE account_id=?1 AND date(posted_at) <= date(?2)
         GROUP BY currency_code
         HAVING SUM(amount_minor) <> 0
         ORDER BY currency_code",
    )
    .bind(account_id)
    .bind(as_of)
    .fetch_all(db)
    .await?)
}

/// Every distinct `(ticker, exchange)` ever traded on this account — the set the
/// historical backfill bulk-fetches a full price series for (one upstream call each),
/// including tickers fully sold before today that still held value in the past.
pub async fn account_tickers(db: &Db, account_id: i64) -> AppResult<Vec<(String, String)>> {
    Ok(sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT ticker, exchange FROM holdings WHERE account_id=?1",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?)
}

/// The earliest date this account has any activity (a trade or a wallet transaction), as
/// a `YYYY-MM-DD` string — the start point for the historical valuation backfill. `None`
/// if the account is empty.
pub async fn earliest_activity_date(db: &Db, account_id: i64) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar::<_, Option<String>>(
        "SELECT MIN(d) FROM (
            SELECT MIN(date(trade_date)) AS d FROM holdings WHERE account_id=?1
            UNION ALL
            SELECT MIN(date(posted_at)) AS d FROM transactions WHERE account_id=?1
         )",
    )
    .bind(account_id)
    .fetch_one(db)
    .await?)
}

// ---- bulk import ---------------------------------------------------------

/// A parsed holding lot ready to persist (the API layer maps the provider's parse output
/// into this so the DAL stays independent of `sure-providers`).
pub struct HoldingImport {
    pub ticker: String,
    pub exchange: String,
    pub name: Option<String>,
    pub currency_code: String,
    pub trade_date: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: String,
    pub external_id: String,
}

pub struct WithholdingImport {
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

pub struct DividendImport {
    pub ticker: String,
    pub exchange: String,
    pub record_date: Option<String>,
    pub paid_date: String,
    pub shares_held: Option<f64>,
    pub gross_amount_minor: i64,
    pub net_amount_minor: i64,
    pub currency_code: String,
    pub external_id: String,
    pub withholdings: Vec<WithholdingImport>,
}

/// Counts from persisting one parsed export (transfers/backfill are handled by the caller).
#[derive(Default)]
pub struct ImportCounts {
    pub transactions_imported: i64,
    pub transactions_skipped: i64,
    pub holdings_imported: i64,
    pub holdings_skipped: i64,
    pub dividends_imported: i64,
    pub dividends_skipped: i64,
}

/// Persist a parsed brokerage export: wallet transactions (via the shared
/// `import_transactions`, so they dedupe/categorize like any other provider), holding
/// lots, and dividends (+ withholdings). Every write dedupes on `(provider, external_id)`
/// so re-importing the same (or an overlapping) export is idempotent — a partial run left
/// by an error is simply completed by re-running, which is why holdings/dividends don't
/// need to share a transaction with the wallet import.
pub async fn import_export(
    db: &Db,
    account_id: i64,
    account_currency: &str,
    provider_tag: &str,
    wallet_rows: &[crate::providers::ImportRow],
    holdings: &[HoldingImport],
    dividends: &[DividendImport],
) -> AppResult<ImportCounts> {
    let (transactions_imported, transactions_skipped) = crate::providers::import_transactions(
        db,
        account_id,
        account_currency,
        provider_tag,
        wallet_rows,
    )
    .await?;
    let mut counts = ImportCounts {
        transactions_imported,
        transactions_skipped,
        ..Default::default()
    };

    // Holdings + dividends in one transaction: reliable `last_insert_rowid()` for the
    // withholdings foreign key, and an all-or-nothing write for the ledger half.
    let mut tx = db.begin().await?;
    for h in holdings {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO holdings
                (account_id, ticker, exchange, name, currency_code, trade_date, quantity,
                 unit_price, fee_minor, kind, external_id, provider)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )
        .bind(account_id)
        .bind(&h.ticker)
        .bind(&h.exchange)
        .bind(&h.name)
        .bind(&h.currency_code)
        .bind(&h.trade_date)
        .bind(h.quantity)
        .bind(h.unit_price)
        .bind(h.fee_minor)
        .bind(&h.kind)
        .bind(&h.external_id)
        .bind(provider_tag)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() > 0 {
            counts.holdings_imported += 1;
        } else {
            counts.holdings_skipped += 1;
        }
    }
    for d in dividends {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO dividends
                (account_id, ticker, exchange, record_date, paid_date, shares_held,
                 gross_amount_minor, net_amount_minor, currency_code, external_id, provider)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        )
        .bind(account_id)
        .bind(&d.ticker)
        .bind(&d.exchange)
        .bind(&d.record_date)
        .bind(&d.paid_date)
        .bind(d.shares_held)
        .bind(d.gross_amount_minor)
        .bind(d.net_amount_minor)
        .bind(&d.currency_code)
        .bind(&d.external_id)
        .bind(provider_tag)
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() > 0 {
            let dividend_id = res.last_insert_rowid();
            for w in &d.withholdings {
                sqlx::query(
                    "INSERT INTO dividend_withholdings
                        (dividend_id, owed_to, tax_amount_minor, tax_credit_minor, currency_code)
                     VALUES (?1,?2,?3,?4,?5)",
                )
                .bind(dividend_id)
                .bind(&w.owed_to)
                .bind(w.tax_amount_minor)
                .bind(w.tax_credit_minor)
                .bind(&w.currency_code)
                .execute(&mut *tx)
                .await?;
            }
            counts.dividends_imported += 1;
        } else {
            counts.dividends_skipped += 1;
        }
    }
    tx.commit().await?;
    Ok(counts)
}

fn map_fk(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::Database(ref db) if db.is_foreign_key_violation() => {
            AppError::validation("referenced account or currency does not exist")
        }
        other => AppError::from(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sure_core::{AccountKind, AccountMetadata, BrokerageMeta, SaveAccount};

    async fn test_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        pool
    }

    async fn brokerage_account(db: &Db) -> i64 {
        crate::accounts::create(
            db,
            SaveAccount {
                name: "Sharesies".to_string(),
                kind: AccountKind::Brokerage,
                currency_code: "NZD".to_string(),
                institution: Some("Sharesies".to_string()),
                metadata: Some(AccountMetadata::Brokerage(BrokerageMeta::default())),
                archived: false,
                sort_order: 0,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn holding(external_id: &str, ticker: &str, trade_date: &str, qty: f64) -> HoldingImport {
        HoldingImport {
            ticker: ticker.to_string(),
            exchange: "NZX".to_string(),
            name: Some(format!("{ticker} Ltd")),
            currency_code: "NZD".to_string(),
            trade_date: trade_date.to_string(),
            quantity: qty,
            unit_price: Some(1.0),
            fee_minor: 0,
            kind: "buy".to_string(),
            external_id: external_id.to_string(),
        }
    }

    fn wallet(external_id: &str, posted_at: &str, amount_minor: i64, ccy: &str) -> crate::providers::ImportRow {
        crate::providers::ImportRow {
            external_id: external_id.to_string(),
            posted_at: posted_at.to_string(),
            amount_minor,
            currency_code: Some(ccy.to_string()),
            description: "wallet".to_string(),
            merchant: None,
            category_name: None,
            category_group: None,
            category_kind: None,
        }
    }

    #[tokio::test]
    async fn positions_and_wallet_balances_sum_over_time() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        import_export(
            &db,
            account_id,
            "NZD",
            "sharesies#1",
            &[
                wallet("w1", "2026-01-01", 100_000, "NZD"),
                wallet("w2", "2026-01-05", -30_000, "NZD"),
                wallet("w3", "2026-02-01", 5_000, "USD"),
            ],
            &[
                holding("h1", "MEL", "2026-01-02", 100.0),
                holding("h2", "MEL", "2026-01-10", 50.0),
                holding("h3", "AIR", "2026-01-11", -10.0), // net-negative alone → excluded
            ],
            &[],
        )
        .await
        .unwrap();

        // As of 2026-01-05: only the first MEL buy has settled; AIR sell hasn't.
        let early = positions_at(&db, account_id, "2026-01-05").await.unwrap();
        assert_eq!(early.len(), 1);
        assert_eq!(early[0].ticker, "MEL");
        assert_eq!(early[0].quantity, 100.0);

        // As of today: MEL nets 150; AIR nets -10 which is dropped by the caller's
        // interpretation but here it's a real (short) net — still returned since != 0.
        let now = positions_at(&db, account_id, "2026-12-31").await.unwrap();
        let mel = now.iter().find(|p| p.ticker == "MEL").unwrap();
        assert_eq!(mel.quantity, 150.0);

        // Wallet balances split by currency.
        let wallets = wallet_balances_at(&db, account_id, "2026-12-31").await.unwrap();
        let nzd = wallets.iter().find(|w| w.currency_code == "NZD").unwrap();
        assert_eq!(nzd.amount_minor, 70_000);
        let usd = wallets.iter().find(|w| w.currency_code == "USD").unwrap();
        assert_eq!(usd.amount_minor, 5_000);
    }

    #[tokio::test]
    async fn reimport_is_idempotent() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        let rows = [wallet("w1", "2026-01-01", 100_000, "NZD")];
        let holds = [holding("h1", "MEL", "2026-01-02", 100.0)];
        let divs = [DividendImport {
            ticker: "MEL".to_string(),
            exchange: "NZX".to_string(),
            record_date: Some("2026-03-01".to_string()),
            paid_date: "2026-03-10".to_string(),
            shares_held: Some(100.0),
            gross_amount_minor: 5_000,
            net_amount_minor: 4_100,
            currency_code: "NZD".to_string(),
            external_id: "d1".to_string(),
            withholdings: vec![WithholdingImport {
                owed_to: "NZ_IRD".to_string(),
                tax_amount_minor: 900,
                tax_credit_minor: Some(0),
                currency_code: "NZD".to_string(),
            }],
        }];

        let first = import_export(&db, account_id, "NZD", "sharesies#1", &rows, &holds, &divs)
            .await
            .unwrap();
        assert_eq!(first.transactions_imported, 1);
        assert_eq!(first.holdings_imported, 1);
        assert_eq!(first.dividends_imported, 1);

        let second = import_export(&db, account_id, "NZD", "sharesies#1", &rows, &holds, &divs)
            .await
            .unwrap();
        assert_eq!(second.transactions_imported, 0);
        assert_eq!(second.transactions_skipped, 1);
        assert_eq!(second.holdings_imported, 0);
        assert_eq!(second.holdings_skipped, 1);
        assert_eq!(second.dividends_imported, 0);
        assert_eq!(second.dividends_skipped, 1);

        // Withholdings weren't duplicated on the second run.
        let detail = list_dividends(&db, account_id).await.unwrap();
        assert_eq!(detail.len(), 1);
        assert_eq!(detail[0].withholdings.len(), 1);
    }

    #[tokio::test]
    async fn earliest_activity_spans_holdings_and_transactions() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;
        import_export(
            &db,
            account_id,
            "NZD",
            "sharesies#1",
            &[wallet("w1", "2026-02-01", 100_000, "NZD")],
            &[holding("h1", "MEL", "2026-01-15", 100.0)],
            &[],
        )
        .await
        .unwrap();
        assert_eq!(
            earliest_activity_date(&db, account_id).await.unwrap().as_deref(),
            Some("2026-01-15")
        );
    }
}
