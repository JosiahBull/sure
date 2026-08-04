//! Brokerage holdings ledger, computed positions/wallet balances, dividend detail, and
//! the bulk-import writer. The price-lookup/FX side that turns positions into a valued
//! snapshot lives in `sure_api::brokerage` (it needs the stock-price provider); this
//! module is pure persistence, mirroring the split used by `equity`.

use sqlx::FromRow;
use sure_core::{AppError, AppResult, LotKind};
pub use sure_core::{Dividend, DividendDetail, DividendWithholding, HoldingLot, SaveHoldingLot};

use crate::Db;

/// The largest share count one lot may carry. No real trade moves a trillion units — the
/// entire issued float of the largest listed company is around 1.5×10^10 — so a figure past
/// this is data entry (a minor-unit amount pasted into the quantity field, most often).
/// Mirrors `equity::MAX_GRANT_QUANTITY`, and is deliberately the *same* number the read path
/// uses to exclude an already-stored poisoned row, so the write edge and the read edge can
/// never disagree about which lots are walkable.
const MAX_LOT_QUANTITY: f64 = 1e12;

/// The largest per-share price one lot may carry, in whole currency units: a billion. The
/// most expensive share on earth (Berkshire A, ~$700k) sits three orders of magnitude under
/// it, so this only ever catches a slip. `unit_price` is informational as far as the schema
/// is concerned, but `sure_app::brokerage::cost_basis_by_ticker` multiplies it by quantity
/// into an f64 that is then `round() as i64` — and an `as` cast *saturates* silently, so an
/// absurd price becomes a plausible-looking cost basis rather than an error.
const MAX_UNIT_PRICE: f64 = 1e9;

/// Reject a float that JSON accepts and `f64` parses happily but no downstream arithmetic
/// survives. `1e308`, `Infinity` (as the literal `9e999`) and `NaN` all deserialize into a
/// [`SaveHoldingLot`] without complaint; once persisted they poison
/// `sure_app::brokerage::cost_basis_by_ticker`'s running total for the whole ticker, and —
/// because every comparison against `NaN` is false — make the position *silently vanish*
/// from the snapshot rather than erroring. There is no honest read-time repair for a number
/// nobody can recover, so it has to be refused at the one edge that still knows which field
/// the user typed.
fn check_amount(field: &str, value: f64, max: f64) -> AppResult<()> {
    if !value.is_finite() {
        return Err(AppError::validation(format!(
            "{field} must be a finite number"
        )));
    }
    if value.abs() > max {
        return Err(AppError::validation(format!(
            "{field} must be within +/-{max:e}"
        )));
    }
    Ok(())
}

/// Every numeric and sign invariant a holding lot must satisfy, shared by the manual-entry
/// path ([`create_holding`], which surfaces the message as a 422) and the bulk importer
/// ([`import_export`], which skips and warns) — a provider export must not be able to write
/// what a form is refused.
fn check_lot(kind: LotKind, quantity: f64, unit_price: Option<f64>) -> AppResult<()> {
    check_amount("quantity", quantity, MAX_LOT_QUANTITY)?;
    if quantity == 0.0 {
        return Err(AppError::validation("quantity must be non-zero"));
    }
    if let Some(price) = unit_price {
        check_amount("unit_price", price, MAX_UNIT_PRICE)?;
        if price < 0.0 {
            return Err(AppError::validation("unit_price must not be negative"));
        }
    }
    // `holdings.quantity` is signed by convention (0012_brokerage.sql: "+buy/corporate
    // credit, -sell") and the cost-basis walk depends on it: a `sell` stored positive *adds*
    // to the position it was meant to exit, and a `buy` stored negative silently shorts one.
    // Matched exhaustively per CLAUDE.md rule 2 — a fourth `LotKind` must state its sign rule
    // here rather than inherit "anything goes" from a wildcard arm.
    match kind {
        LotKind::Buy => {
            if quantity < 0.0 {
                return Err(AppError::validation(
                    "a buy lot's quantity must be positive",
                ));
            }
        }
        LotKind::Sell => {
            if quantity > 0.0 {
                return Err(AppError::validation(
                    "a sell lot's quantity must be negative (it exits the position)",
                ));
            }
        }
        // A corporate action legitimately goes either way: a split or bonus issue credits
        // units, a consolidation/reverse split debits them.
        LotKind::Corporate => {}
    }
    Ok(())
}

/// Whether an *already stored* lot's numbers can be walked without poisoning the result.
///
/// Deliberately narrower than [`check_lot`]: it re-checks only what breaks arithmetic
/// (finiteness and magnitude), not the sign convention, because a pre-`check_lot` row with an
/// oddly-signed quantity still sums to a meaningful position, whereas an `Infinity` does not.
/// Only ±Inf and absurd-but-finite values can actually be on disk — SQLite has no `NaN`, it
/// stores one as `NULL`, which `quantity REAL NOT NULL` refuses outright.
fn lot_amounts_usable(quantity: f64, unit_price: Option<f64>) -> bool {
    quantity.is_finite()
        && quantity.abs() <= MAX_LOT_QUANTITY
        && unit_price.is_none_or(|p| p.is_finite() && p.abs() <= MAX_UNIT_PRICE)
}

/// Parse a stored `kind` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `LotKind::as_str`, so an unparseable value means the row came
/// from something else entirely and deserves a real error, not a silent default.
fn parse_kind(kind: String) -> AppResult<LotKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

#[derive(Debug, FromRow)]
struct HoldingLotRow {
    id: i64,
    account_id: i64,
    ticker: String,
    exchange: String,
    name: Option<String>,
    currency_code: String,
    trade_date: String,
    quantity: f64,
    unit_price: Option<f64>,
    fee_minor: i64,
    kind: String,
    external_id: Option<String>,
    provider: Option<String>,
    created_at: String,
}

impl TryFrom<HoldingLotRow> for HoldingLot {
    type Error = AppError;

    fn try_from(r: HoldingLotRow) -> AppResult<Self> {
        Ok(HoldingLot {
            kind: parse_kind(r.kind)?,
            id: r.id,
            account_id: r.account_id,
            ticker: r.ticker,
            exchange: r.exchange,
            name: r.name,
            currency_code: r.currency_code,
            trade_date: r.trade_date,
            quantity: r.quantity,
            unit_price: r.unit_price,
            fee_minor: r.fee_minor,
            external_id: r.external_id,
            provider: r.provider,
            created_at: r.created_at,
        })
    }
}

// ---- holdings CRUD -------------------------------------------------------

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_holdings(db: &Db, account_id: i64) -> AppResult<Vec<HoldingLot>> {
    sqlx::query_as::<_, HoldingLotRow>(
        "SELECT * FROM holdings WHERE account_id=?1 ORDER BY date(trade_date), id",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(HoldingLot::try_from)
    .collect()
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn create_holding(
    db: &Db,
    account_id: i64,
    input: SaveHoldingLot,
) -> AppResult<HoldingLot> {
    let ticker = input.ticker.trim().to_uppercase();
    if ticker.is_empty() {
        return Err(AppError::validation("ticker is required"));
    }
    check_lot(input.kind, input.quantity, input.unit_price)?;
    // The same check the account create path runs (`accounts::validate`). `currency_code` does
    // carry a FK to `currencies(code)`, so an unknown code is already refused — but only as
    // `map_fk`'s generic "referenced account or currency does not exist", which never says
    // which of the two it was, and only *after* the write is attempted. Checking here names
    // the field and the value, and the FK stays as the backstop.
    let currency = input.currency_code.trim().to_uppercase();
    if !crate::currencies::exists(db, &currency).await? {
        return Err(AppError::validation(format!(
            "unknown currency '{currency}'"
        )));
    }
    sqlx::query_as::<_, HoldingLotRow>(
        "INSERT INTO holdings
            (account_id, ticker, exchange, name, currency_code, trade_date, quantity,
             unit_price, fee_minor, kind)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) RETURNING *",
    )
    .bind(account_id)
    .bind(&ticker)
    .bind(input.exchange.trim())
    .bind(&input.name)
    .bind(&currency)
    .bind(input.trade_date.to_string())
    .bind(input.quantity)
    .bind(input.unit_price)
    .bind(input.fee_minor)
    .bind(input.kind.as_str())
    .fetch_one(db)
    .await
    .map_err(map_fk)?
    .try_into()
}

#[tracing::instrument(level = "debug", skip_all)]
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

#[derive(Debug, FromRow)]
struct DividendRow {
    id: i64,
    account_id: i64,
    ticker: String,
    exchange: String,
    record_date: Option<String>,
    paid_date: String,
    shares_held: Option<f64>,
    gross_amount_minor: i64,
    net_amount_minor: i64,
    currency_code: String,
    external_id: Option<String>,
    provider: Option<String>,
    created_at: String,
}

impl From<DividendRow> for Dividend {
    fn from(r: DividendRow) -> Self {
        Dividend {
            id: r.id,
            account_id: r.account_id,
            ticker: r.ticker,
            exchange: r.exchange,
            record_date: r.record_date,
            paid_date: r.paid_date,
            shares_held: r.shares_held,
            gross_amount_minor: r.gross_amount_minor,
            net_amount_minor: r.net_amount_minor,
            currency_code: r.currency_code,
            external_id: r.external_id,
            provider: r.provider,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct DividendWithholdingRow {
    id: i64,
    dividend_id: i64,
    owed_to: String,
    tax_amount_minor: i64,
    tax_credit_minor: Option<i64>,
    currency_code: String,
}

impl From<DividendWithholdingRow> for DividendWithholding {
    fn from(r: DividendWithholdingRow) -> Self {
        DividendWithholding {
            id: r.id,
            dividend_id: r.dividend_id,
            owed_to: r.owed_to,
            tax_amount_minor: r.tax_amount_minor,
            tax_credit_minor: r.tax_credit_minor,
            currency_code: r.currency_code,
        }
    }
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn list_dividends(db: &Db, account_id: i64) -> AppResult<Vec<DividendDetail>> {
    let dividends: Vec<Dividend> = sqlx::query_as::<_, DividendRow>(
        "SELECT * FROM dividends WHERE account_id=?1 ORDER BY date(paid_date) DESC, id DESC",
    )
    .bind(account_id)
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Into::into)
    .collect();
    let mut out = Vec::with_capacity(dividends.len());
    for dividend in dividends {
        let withholdings: Vec<DividendWithholding> = sqlx::query_as::<_, DividendWithholdingRow>(
            "SELECT * FROM dividend_withholdings WHERE dividend_id=?1 ORDER BY id",
        )
        .bind(dividend.id)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
        out.push(DividendDetail {
            dividend,
            withholdings,
        });
    }
    Ok(out)
}

// ---- computed positions / wallet balances --------------------------------

/// A ticker's net held quantity as of a date (before pricing — see `sure_api::brokerage`).
#[derive(Debug, FromRow)]
pub struct PositionRow {
    pub ticker: String,
    pub exchange: String,
    pub currency_code: String,
    pub name: Option<String>,
    pub quantity: f64,
}

/// The raw row shape for [`CostLotRow`] — `kind` as stored, before parsing. Carries `id`,
/// which [`CostLotRow`] has no use for, purely so a row rejected by [`lot_amounts_usable`]
/// can be named in the WARN that explains why the panel is missing it.
#[derive(Debug, FromRow, Clone)]
struct CostLotRowRaw {
    id: i64,
    ticker: String,
    exchange: String,
    currency_code: String,
    quantity: f64,
    unit_price: Option<f64>,
    fee_minor: i64,
    kind: String,
}

/// One lot, reduced to what the average-cost-basis walk needs (see `sure_api::brokerage`).
#[derive(Debug, Clone)]
pub struct CostLotRow {
    pub ticker: String,
    pub exchange: String,
    pub currency_code: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: LotKind,
}

impl TryFrom<CostLotRowRaw> for CostLotRow {
    type Error = AppError;

    fn try_from(r: CostLotRowRaw) -> AppResult<Self> {
        Ok(CostLotRow {
            kind: parse_kind(r.kind)?,
            ticker: r.ticker,
            exchange: r.exchange,
            currency_code: r.currency_code,
            quantity: r.quantity,
            unit_price: r.unit_price,
            fee_minor: r.fee_minor,
        })
    }
}

/// Every lot up to `as_of`, ordered per-ticker by trade date — the cost-basis walk needs
/// them in that order and groups them by `(ticker, exchange)` itself.
///
/// Lots whose numbers can't be walked are dropped here rather than handed on, because the
/// walk has no way to contain them: one `Infinity` quantity makes the running `cost_minor`
/// for that whole ticker `Inf` (or `NaN`, once an `Inf` meets a subtraction), and `round() as
/// i64` then saturates that into a cost basis that looks like a number. [`check_lot`] keeps
/// such a row out of the table today, but rows written before it existed — or edited by hand
/// — are still on disk, and a read path that dies on one of them takes the whole brokerage
/// panel down for good. Each dropped lot gets a WARN naming its id so it can be found and
/// deleted; the position itself survives on its remaining lots.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn lots_at(db: &Db, account_id: i64, as_of: &str) -> AppResult<Vec<CostLotRow>> {
    let rows = sqlx::query_as::<_, CostLotRowRaw>(
        "SELECT id, ticker, exchange, currency_code, quantity, unit_price, fee_minor, kind
         FROM holdings
         WHERE account_id=?1 AND date(trade_date) <= date(?2)
         ORDER BY ticker, exchange, date(trade_date), id",
    )
    .bind(account_id)
    .bind(as_of)
    .fetch_all(db)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if !lot_amounts_usable(row.quantity, row.unit_price) {
            tracing::warn!(
                lot_id = row.id,
                ticker = %row.ticker,
                quantity = row.quantity,
                unit_price = ?row.unit_price,
                "excluding holding lot with an unusable quantity/price from the cost-basis walk"
            );
            continue;
        }
        out.push(CostLotRow::try_from(row)?);
    }
    Ok(out)
}

/// Rolling 30-days-to-`as_of` activity: an exact trade count, plus a heuristic
/// contributions/withdrawals split — see [`sure_core::BrokerageActivity30d`] doc comment
/// for why the latter is text-matched rather than category-driven.
#[derive(Debug, FromRow)]
pub struct Activity30dRow {
    pub contributions_minor: i64,
    pub withdrawals_minor: i64,
    pub trades: i64,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn activity_30d(db: &Db, account_id: i64, as_of: &str) -> AppResult<Activity30dRow> {
    let trades: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM holdings
         WHERE account_id=?1 AND kind IN ('buy','sell')
           AND date(trade_date) > date(?2, '-30 days') AND date(trade_date) <= date(?2)",
    )
    .bind(account_id)
    .bind(as_of)
    .fetch_one(db)
    .await?;

    let contributions_minor: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_minor),0) FROM transactions
         WHERE account_id=?1 AND amount_minor > 0
           AND (description LIKE 'Wallet top up%' OR description LIKE 'Deposit%')
           AND date(posted_at) > date(?2, '-30 days') AND date(posted_at) <= date(?2)",
    )
    .bind(account_id)
    .bind(as_of)
    .fetch_one(db)
    .await?;

    let withdrawals_minor: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(-amount_minor),0) FROM transactions
         WHERE account_id=?1 AND amount_minor < 0
           AND description LIKE 'Withdrawal%'
           AND date(posted_at) > date(?2, '-30 days') AND date(posted_at) <= date(?2)",
    )
    .bind(account_id)
    .bind(as_of)
    .fetch_one(db)
    .await?;

    Ok(Activity30dRow {
        contributions_minor,
        withdrawals_minor,
        trades,
    })
}

/// Net quantity held per `(ticker, exchange)` as of `as_of`, dropping fully-exited
/// positions (and float dust from fractional buy/sell rounding).
///
/// `ABS(quantity) <= ?3` excludes an unwalkable row from the sum for the same reason
/// [`lots_at`] drops it, and by the same [`MAX_LOT_QUANTITY`] bound (bound rather than
/// inlined so the two can't drift): `SUM` over a single `Infinity` is `Infinity` for the
/// whole ticker, and `ABS(Inf) > 0.0000001` holds, so the position would be reported with a
/// quantity that serialises to JSON `null` and prices into a saturated `i64`. SQLite compares
/// `Inf` numerically, so this one predicate covers both `±Inf` and absurd finite magnitudes;
/// there is no `NaN` to consider, as SQLite cannot store one in a `NOT NULL REAL` column. The
/// WARN naming the excluded lot comes from `lots_at`, which the snapshot always fetches
/// alongside this (see `sure_app::brokerage::BrokerageService::snapshot`).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn positions_at(db: &Db, account_id: i64, as_of: &str) -> AppResult<Vec<PositionRow>> {
    Ok(sqlx::query_as::<_, PositionRow>(
        "SELECT ticker, exchange, currency_code, MAX(name) AS name, SUM(quantity) AS quantity
         FROM holdings
         WHERE account_id=?1 AND date(trade_date) <= date(?2) AND ABS(quantity) <= ?3
         GROUP BY ticker, exchange
         HAVING ABS(SUM(quantity)) > 0.0000001
         ORDER BY ticker",
    )
    .bind(account_id)
    .bind(as_of)
    .bind(MAX_LOT_QUANTITY)
    .fetch_all(db)
    .await?)
}

#[derive(Debug, FromRow)]
pub struct WalletRow {
    pub currency_code: String,
    pub amount_minor: i64,
}

/// Cash balance per currency as of `as_of` — every transaction on a brokerage account is
/// a wallet-cash movement (imported or manual), summed per currency (a brokerage account
/// legitimately holds several currencies at once).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn wallet_balances_at(
    db: &Db,
    account_id: i64,
    as_of: &str,
) -> AppResult<Vec<WalletRow>> {
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
#[tracing::instrument(level = "debug", skip_all)]
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
#[tracing::instrument(level = "debug", skip_all)]
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
    pub kind: LotKind,
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
#[derive(Debug, Default)]
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
#[tracing::instrument(level = "debug", skip_all)]
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
        // A provider export must not be able to store what the manual form is refused: one
        // `Infinity`/`NaN`/absurd quantity from a mangled upstream row would otherwise poison
        // the cost basis of that ticker forever. Skipped (and counted as such) rather than
        // failing the whole import — the other few thousand rows in the export are fine, and
        // an all-or-nothing failure here means the user gets *nothing* and no way to see why.
        // `ImportCounts` has no per-row warning channel, so the reason goes to the log.
        if let Err(e) = check_lot(h.kind, h.quantity, h.unit_price) {
            tracing::warn!(
                external_id = %h.external_id,
                ticker = %h.ticker,
                quantity = h.quantity,
                unit_price = ?h.unit_price,
                error = %e,
                "skipping imported holding lot with unusable numbers"
            );
            counts.holdings_skipped += 1;
            continue;
        }
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
        .bind(h.kind.as_str())
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

// `sqlx::Error` is `#[non_exhaustive]` upstream, so a catch-all is the only option here
// (CLAUDE.md rule 2's escape hatch) — the arm above is exhaustive over our own types.
#[allow(clippy::wildcard_enum_match_arm)]
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
    use sure_core::{AccountKind, AccountMetadata, BrokerageMeta, IsoDate, SaveAccount};

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
                metadata: Some(AccountMetadata::Brokerage(BrokerageMeta {
                    broker: Some("Sharesies".to_string()),
                    ..Default::default()
                })),
                archived: false,
                sort_order: 0,
                // A brokerage account is the one kind with no opening balance: its value is
                // computed from the holdings ledger these tests populate.
                opening_balance_minor: None,
                opening_balance_date: None,
                // These tests don't care who owns the account; joint needs no person row.
                ownership: sure_core::Ownership::Joint,
            },
        )
        .await
        .unwrap()
        .id
    }

    /// A lot's `kind` follows the sign of its quantity, because `check_lot` now requires the
    /// two to agree (`holdings.quantity` is signed: +buy, -sell) — a fixture that pairs a
    /// negative quantity with `Buy` describes a row the importer would refuse.
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
            kind: if qty < 0.0 {
                LotKind::Sell
            } else {
                LotKind::Buy
            },
            external_id: external_id.to_string(),
        }
    }

    fn lot(qty: f64, unit_price: Option<f64>, kind: LotKind) -> SaveHoldingLot {
        SaveHoldingLot {
            ticker: "MEL".to_string(),
            exchange: "NZX".to_string(),
            name: Some("Meridian Energy".to_string()),
            currency_code: "NZD".to_string(),
            trade_date: IsoDate::parse("2026-01-02").unwrap(),
            quantity: qty,
            unit_price,
            fee_minor: 0,
            kind,
        }
    }

    fn validation_message<T: std::fmt::Debug>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::Validation(msg)) => msg,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    fn wallet(
        external_id: &str,
        posted_at: &str,
        amount_minor: i64,
        ccy: &str,
    ) -> crate::providers::ImportRow {
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
            is_one_off: false,
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
                holding("h3", "AIR", "2026-01-11", -10.0), // a sell; net-negative alone
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
        let wallets = wallet_balances_at(&db, account_id, "2026-12-31")
            .await
            .unwrap();
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
            earliest_activity_date(&db, account_id)
                .await
                .unwrap()
                .as_deref(),
            Some("2026-01-15")
        );
    }

    #[tokio::test]
    async fn a_non_finite_quantity_or_price_is_refused() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        // JSON has no `Infinity` token, but `1e999` overflows to one on parse, and a provider
        // payload or a hand-rolled client can send either. `f64` takes them without complaint.
        for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let message = validation_message(
                create_holding(&db, account_id, lot(bad, Some(1.0), LotKind::Buy)).await,
            );
            assert!(
                message.contains("quantity must be a finite number"),
                "got {message:?} for {bad}"
            );
        }
        for bad in [f64::INFINITY, f64::NAN] {
            let message = validation_message(
                create_holding(&db, account_id, lot(10.0, Some(bad), LotKind::Buy)).await,
            );
            assert!(
                message.contains("unit_price must be a finite number"),
                "got {message:?} for {bad}"
            );
        }

        // Nothing reached the table, so no later read has to cope with any of it.
        assert!(list_holdings(&db, account_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_absurd_but_finite_quantity_or_price_is_refused() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        // `1e308` is perfectly finite and still multiplies into an overflow two operations
        // later, which is why the ceiling exists on top of the finiteness check.
        let message = validation_message(
            create_holding(&db, account_id, lot(1e308, Some(1.0), LotKind::Buy)).await,
        );
        assert!(
            message.contains("quantity must be within"),
            "got {message:?}"
        );
        let message = validation_message(
            create_holding(&db, account_id, lot(-1e308, Some(1.0), LotKind::Sell)).await,
        );
        assert!(
            message.contains("quantity must be within"),
            "got {message:?}"
        );
        let message = validation_message(
            create_holding(&db, account_id, lot(10.0, Some(1e308), LotKind::Buy)).await,
        );
        assert!(
            message.contains("unit_price must be within"),
            "got {message:?}"
        );
        let message = validation_message(
            create_holding(&db, account_id, lot(10.0, Some(-1.0), LotKind::Buy)).await,
        );
        assert!(
            message.contains("unit_price must not be negative"),
            "got {message:?}"
        );
        // A share count right at the ceiling is still a (bizarre but) storable trade.
        create_holding(
            &db,
            account_id,
            lot(MAX_LOT_QUANTITY, Some(1.0), LotKind::Buy),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_lots_quantity_must_be_signed_to_match_its_kind() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        let message = validation_message(
            create_holding(&db, account_id, lot(-5.0, Some(1.0), LotKind::Buy)).await,
        );
        assert!(message.contains("buy lot's quantity"), "got {message:?}");
        let message = validation_message(
            create_holding(&db, account_id, lot(5.0, Some(1.0), LotKind::Sell)).await,
        );
        assert!(message.contains("sell lot's quantity"), "got {message:?}");
        let message =
            validation_message(create_holding(&db, account_id, lot(0.0, None, LotKind::Buy)).await);
        assert!(message.contains("non-zero"), "got {message:?}");

        // A sell exits the position, so it is negative; a corporate action goes either way (a
        // bonus issue credits units, a consolidation debits them) and is accepted both ways.
        create_holding(&db, account_id, lot(-5.0, Some(1.0), LotKind::Sell))
            .await
            .unwrap();
        create_holding(&db, account_id, lot(5.0, None, LotKind::Corporate))
            .await
            .unwrap();
        create_holding(&db, account_id, lot(-5.0, None, LotKind::Corporate))
            .await
            .unwrap();
        assert_eq!(list_holdings(&db, account_id).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn an_unknown_currency_is_named_rather_than_left_to_the_fk() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        let mut input = lot(10.0, Some(1.0), LotKind::Buy);
        input.currency_code = "XyZ".to_string();
        let message = validation_message(create_holding(&db, account_id, input).await);
        assert!(
            message.contains("unknown currency 'XYZ'"),
            "the message should name the field and the value it rejected, got {message:?}"
        );

        // A known code still stores, trimmed and upper-cased on the way in — the FK is on
        // `currencies(code)`, so an untrimmed ' nzd ' would otherwise be refused as a generic
        // foreign-key violation naming neither field.
        let mut input = lot(10.0, Some(1.0), LotKind::Buy);
        input.currency_code = " nzd ".to_string();
        let stored = create_holding(&db, account_id, input).await.unwrap();
        assert_eq!(stored.currency_code, "NZD");
    }

    #[tokio::test]
    async fn an_ordinary_lot_still_stores() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        let stored = create_holding(&db, account_id, lot(123.456, Some(6.78), LotKind::Buy))
            .await
            .unwrap();
        assert_eq!(stored.ticker, "MEL");
        assert_eq!(stored.quantity, 123.456);
        assert_eq!(stored.unit_price, Some(6.78));
        assert_eq!(stored.kind, LotKind::Buy);
        assert_eq!(
            positions_at(&db, account_id, "2026-12-31").await.unwrap()[0].quantity,
            123.456
        );
    }

    /// Insert a lot straight through SQL, bypassing [`check_lot`] — the only way to reproduce a
    /// row written before the guard existed (or edited by hand in `sqlite3`). SQLite parses
    /// `9e999` as `Inf`; it has no `NaN`, storing one as `NULL`, which `quantity REAL NOT NULL`
    /// refuses outright, so `Inf` and absurd-but-finite are the only two cases on disk.
    async fn insert_unvalidated(db: &Db, account_id: i64, ticker: &str, quantity_sql: &str) {
        sqlx::query(&format!(
            "INSERT INTO holdings
                (account_id, ticker, exchange, currency_code, trade_date, quantity, unit_price,
                 fee_minor, kind)
             VALUES (?1, ?2, 'NZX', 'NZD', '2026-01-03', {quantity_sql}, 1.0, 0, 'buy')"
        ))
        .bind(account_id)
        .bind(ticker)
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_stored_unusable_quantity_neither_panics_nor_vanishes_the_position() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;
        import_export(
            &db,
            account_id,
            "NZD",
            "sharesies#1",
            &[],
            &[holding("h1", "MEL", "2026-01-02", 100.0)],
            &[],
        )
        .await
        .unwrap();
        insert_unvalidated(&db, account_id, "MEL", "9e999").await; // +Inf
        insert_unvalidated(&db, account_id, "MEL", "1e308").await; // finite, still absurd

        // The raw ledger keeps showing every row: the bad ones have to stay visible to be
        // deleted, and this listing does no arithmetic on them.
        let all = list_holdings(&db, account_id).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|l| !l.quantity.is_finite()));

        // The cost-basis walk is handed only the walkable lot (each exclusion logs a WARN
        // naming the lot id) instead of an `Inf` that would poison MEL's whole running total.
        let lots = lots_at(&db, account_id, "2026-12-31").await.unwrap();
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].quantity, 100.0);

        // And the position survives on its remaining lots rather than being reported as `Inf`
        // (which serialises to JSON `null` and prices into a saturated i64) or disappearing —
        // before the guard, `SUM(quantity)` over these three rows was `Inf`.
        let positions = positions_at(&db, account_id, "2026-12-31").await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].ticker, "MEL");
        assert_eq!(positions[0].quantity, 100.0);
    }

    #[tokio::test]
    async fn the_importer_skips_lots_with_unusable_numbers() {
        let db = test_db().await;
        let account_id = brokerage_account(&db).await;

        let counts = import_export(
            &db,
            account_id,
            "NZD",
            "sharesies#1",
            &[],
            &[
                holding("h1", "MEL", "2026-01-02", 100.0),
                holding("h2", "AIR", "2026-01-03", f64::INFINITY),
                holding("h3", "CEN", "2026-01-04", 1e308),
            ],
            &[],
        )
        .await
        .unwrap();

        // The good row lands; the two unusable ones are skipped (with a WARN each) rather than
        // failing the whole export, which would leave the user with nothing and no explanation.
        assert_eq!(counts.holdings_imported, 1);
        assert_eq!(counts.holdings_skipped, 2);
        let positions = positions_at(&db, account_id, "2026-12-31").await.unwrap();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].ticker, "MEL");
    }
}
