//! Read-only loaders backing the report endpoints. This layer only fetches rows; all
//! the aggregation (running balances, currency normalisation, category roll-ups, flow
//! graphs) lives in the API crate, which calls these loaders and crunches the numbers.

use chrono::NaiveDate;
use sure_core::{AccountKind, AppError, AppResult, CategoryKind, Ownership, effective_ownership};

use crate::Db;

// Several queries below filter on "a stored date `sure_app::reports::parse_stored_date` can
// actually read":
//
//     col GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
//         AND date(col) = substr(col, 1, 10)
//
// Two halves, both needed:
//   * the `GLOB` is the ten-character zero-padded `YYYY-MM-DD` prefix `sure_core::IsoDate`
//     writes — a legacy `31/07/2026` fails it, and so does anything that would compare
//     nonsensically against a lexicographic window bound;
//   * `date(x) = substr(x, 1, 10)` is the calendar check `GLOB` can't do. SQLite's `date()`
//     *normalises*, so `date('2026-02-30')` is `'2026-03-02'` and the equality fails — which
//     is exactly the row `chrono` refuses too.
//
// Why it is needed at all: a row in any other shape is invisible to every report today
// (`parse_stored_date` drops it, loudly). The windowed reads below collapse a window's
// pre-history into a per-account *aggregate*, and an aggregate cannot drop a row after the
// fact — an unreadable row folded into that sum would silently move the balance sheet.
// Filtered here, it stays exactly as visible (and as invisible) as it was.
//
// It used to be one `readable_date(col)` helper, but the compile-time-checked query macros
// need a literal string and cannot take an interpolated one — so there is now one copy per
// query: over `posted_at` in `pre_window_rows`, `seed_aggregate` and
// `earliest_transaction_date`, over `as_of` in `valuations` and `earliest_valuation_date`.
// They must stay identical, and `an_unreadable_pre_window_date_is_left_out_of_the_seed` below
// is what notices if one drifts.

/// A stored date column, formatted for SQLite's *lexicographic* `TEXT` comparison. Every date
/// this system writes is zero-padded `YYYY-MM-DD` ([`sure_core::IsoDate`]), so `>=`/`<` on the
/// raw column is the same ordering as on the calendar — which is what lets a report window be
/// an indexed range scan (`idx_tx_posted`) rather than a `substr()` over every row.
fn bound(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// The lower bound of a ledger read. `None` — "no window", which only the forecast asks for —
/// becomes the empty string: it sorts below every possible stored value, so `posted_at >= ?1`
/// matches every row and `posted_at < ?1` matches none (nothing to seed). One statement
/// therefore covers both the windowed and the unwindowed read, with no branch to keep in step.
fn lower_bound(from: Option<NaiveDate>) -> String {
    match from {
        Some(d) => bound(d),
        None => String::new(),
    }
}

/// An *exclusive* upper bound one day past `to`, so a legacy row carrying a time
/// (`2026-08-04T09:30`) still counts on its own day — `sure_app::reports::parse_stored_date`
/// truncates to ten characters, and a SQL bound that is narrower than the Rust filter would
/// drop rows the report used to include. `succ_opt` is `None` only at [`NaiveDate::MAX`],
/// which no report window reaches; `~` sorts above every digit, so it bounds nothing out.
fn day_after(d: NaiveDate) -> String {
    match d.succ_opt() {
        Some(next) => bound(next),
        None => "~".to_string(),
    }
}

/// Parse a stored `kind` TEXT column into the domain enum, exactly like
/// `sure_dal::accounts::AccountRow`'s `TryFrom<AccountRow> for Account` does — every
/// writer goes through `AccountKind::as_str`, so an unparseable value means the row came
/// from something else entirely and deserves a real error, not a silent default.
fn parse_kind(kind: String) -> AppResult<AccountKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

/// Reassemble an account's stored ownership pair, with the same strictness as
/// `sure_dal::accounts` — a pair that doesn't reassemble is a real error, not a default.
fn parse_ownership(ownership: String, person_id: Option<i64>) -> AppResult<Ownership> {
    Ownership::from_stored(&ownership, person_id)
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

/// A currency's minor-unit scale, for converting minor units to major.
#[derive(Debug)]
pub struct CurrencyDecimals {
    pub code: String,
    pub decimal_places: i64,
}

/// An account and its currency (all accounts, including archived) — for net-worth history.
#[derive(Debug)]
pub struct AccountCurrencyRow {
    pub id: i64,
    pub currency_code: String,
    pub ownership: String,
    pub person_id: Option<i64>,
    pub excluded_from_net_worth: bool,
}

/// An account and its currency, plus who it belongs to — the net-worth series filters the
/// account set by person before walking any ledger.
#[derive(Debug)]
pub struct AccountCurrency {
    pub id: i64,
    pub currency_code: String,
    pub ownership: Ownership,
    /// Carried, not applied. The report decides what to do with it — see
    /// `sure_app::reports::ReportService::net_worth_inputs`.
    pub excluded_from_net_worth: bool,
}

impl TryFrom<AccountCurrencyRow> for AccountCurrency {
    type Error = AppError;

    fn try_from(r: AccountCurrencyRow) -> AppResult<Self> {
        Ok(AccountCurrency {
            ownership: parse_ownership(r.ownership, r.person_id)?,
            id: r.id,
            currency_code: r.currency_code,
            excluded_from_net_worth: r.excluded_from_net_worth,
        })
    }
}

/// The raw row shape for [`ActiveAccount`] — `kind` as stored, before parsing.
#[derive(Debug)]
struct ActiveAccountRow {
    id: i64,
    name: String,
    kind: String,
    currency_code: String,
    ownership: String,
    person_id: Option<i64>,
    excluded_from_net_worth: bool,
}

/// A non-archived account, for the current-balances report.
#[derive(Debug)]
pub struct ActiveAccount {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
    pub ownership: Ownership,
    /// The row is still listed when this is set — only the roll-up leaves it out.
    pub excluded_from_net_worth: bool,
}

impl TryFrom<ActiveAccountRow> for ActiveAccount {
    type Error = AppError;

    fn try_from(r: ActiveAccountRow) -> AppResult<Self> {
        Ok(ActiveAccount {
            kind: parse_kind(r.kind)?,
            ownership: parse_ownership(r.ownership, r.person_id)?,
            id: r.id,
            name: r.name,
            currency_code: r.currency_code,
            excluded_from_net_worth: r.excluded_from_net_worth,
        })
    }
}

/// A single asset account, for the equity-position report.
#[derive(Debug)]
pub struct AssetAccount {
    pub id: i64,
    pub name: String,
    pub currency_code: String,
}

/// The raw row shape for [`SecuredLiabilityAccount`] — `kind` as stored, before parsing.
#[derive(Debug)]
struct SecuredLiabilityAccountRow {
    id: i64,
    name: String,
    kind: String,
    currency_code: String,
}

/// A liability secured against an asset.
#[derive(Debug)]
pub struct SecuredLiabilityAccount {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub currency_code: String,
}

impl TryFrom<SecuredLiabilityAccountRow> for SecuredLiabilityAccount {
    type Error = AppError;

    fn try_from(r: SecuredLiabilityAccountRow) -> AppResult<Self> {
        Ok(SecuredLiabilityAccount {
            kind: parse_kind(r.kind)?,
            id: r.id,
            name: r.name,
            currency_code: r.currency_code,
        })
    }
}

/// A transaction reduced to what a running balance needs.
///
/// `currency_code` is part of "what a running balance needs" because an account is not
/// necessarily single-currency: a Sharesies-style brokerage holds an NZD, an AUD and a USD
/// wallet against one `accounts` row, and Akahu exposes each as its own upstream account
/// feeding the same one here. Summing those amounts without their currency is adding USD
/// cents to NZD cents.
#[derive(Debug)]
pub struct LedgerTx {
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
}

/// A point-in-time valuation reduced to what a running balance needs.
#[derive(Debug)]
pub struct LedgerValuation {
    pub account_id: i64,
    pub as_of: String,
    pub value_minor: i64,
    pub currency_code: String,
}

/// The raw row shape for [`Category`] — `kind` as stored, before parsing.
#[derive(Debug)]
struct CategoryRow {
    id: i64,
    parent_id: Option<i64>,
    name: String,
    color: Option<String>,
    kind: String,
}

/// A category's shape, for building the parent/name/colour/kind lookups.
#[derive(Debug)]
pub struct Category {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub kind: CategoryKind,
}

impl TryFrom<CategoryRow> for Category {
    type Error = AppError;

    fn try_from(r: CategoryRow) -> AppResult<Self> {
        Ok(Category {
            kind: parse_category_kind(r.kind)?,
            id: r.id,
            parent_id: r.parent_id,
            name: r.name,
            color: r.color,
        })
    }
}

/// Parse a stored `kind` TEXT column into the domain enum — see [`parse_kind`]'s doc.
fn parse_category_kind(kind: String) -> AppResult<CategoryKind> {
    kind.parse()
        .map_err(|e: String| AppError::Internal(anyhow::anyhow!(e)))
}

/// The raw row shape for [`SpendTransaction`] — `account_kind` as stored, before parsing.
#[derive(Debug)]
struct SpendTransactionRow {
    posted_at: String,
    amount_minor: i64,
    currency_code: String,
    category_id: Option<i64>,
    is_one_off: bool,
    linked_transaction_id: Option<i64>,
    account_id: i64,
    account_name: String,
    account_kind: String,
    merchant_id: Option<i64>,
    // The merchant record's name where the transaction has one, else whatever text the feed
    // wrote. Coalesced in SQL rather than resolved later: a row can carry a payee as free
    // text with no `merchant_id` at all, and grouping that reads only the id loses it.
    merchant: Option<String>,
    // The transaction's own override (both NULL when it has none) and its account's owner.
    tx_ownership: Option<String>,
    tx_person_id: Option<i64>,
    account_ownership: String,
    account_person_id: Option<i64>,
}

/// A transaction with the fields the spend reports (pie + sankey) filter and roll up.
#[derive(Debug)]
pub struct SpendTransaction {
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub linked_transaction_id: Option<i64>,
    pub account_id: i64,
    pub account_name: String,
    pub account_kind: AccountKind,
    pub merchant_id: Option<i64>,
    /// The merchant record's name, or the raw payee text where the row has no merchant.
    pub merchant: Option<String>,
    /// Who this spending belongs to, already resolved: the transaction's own override, or
    /// its account's owner. Resolved here so every spend report filters on one field
    /// rather than each re-deriving the rule.
    pub attribution: Ownership,
}

impl TryFrom<SpendTransactionRow> for SpendTransaction {
    type Error = AppError;

    fn try_from(r: SpendTransactionRow) -> AppResult<Self> {
        let account = parse_ownership(r.account_ownership, r.account_person_id)?;
        let over = match (r.tx_ownership, r.tx_person_id) {
            (None, None) => None,
            (None, Some(_)) => {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "transaction has a person_id but no ownership discriminant"
                )));
            }
            (Some(kind), person_id) => Some(parse_ownership(kind, person_id)?),
        };
        Ok(SpendTransaction {
            account_kind: parse_kind(r.account_kind)?,
            attribution: effective_ownership(over, account),
            posted_at: r.posted_at,
            amount_minor: r.amount_minor,
            currency_code: r.currency_code,
            category_id: r.category_id,
            is_one_off: r.is_one_off,
            linked_transaction_id: r.linked_transaction_id,
            account_id: r.account_id,
            account_name: r.account_name,
            merchant_id: r.merchant_id,
            merchant: r.merchant,
        })
    }
}

/// Every currency's decimal scale.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn currency_decimals(db: &Db) -> AppResult<Vec<CurrencyDecimals>> {
    Ok(sqlx::query_as!(
        CurrencyDecimals,
        "SELECT code, decimal_places FROM currencies"
    )
    .fetch_all(db)
    .await?)
}

/// Every account's id + currency (net-worth history spans archived accounts too).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn account_currencies(db: &Db) -> AppResult<Vec<AccountCurrency>> {
    sqlx::query_as!(
        AccountCurrencyRow,
        r#"SELECT id AS "id!", currency_code, ownership, person_id,
                  excluded_from_net_worth AS "excluded_from_net_worth!: bool"
             FROM accounts"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(AccountCurrency::try_from)
    .collect()
}

/// Non-archived accounts in display order.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn active_accounts(db: &Db) -> AppResult<Vec<ActiveAccount>> {
    sqlx::query_as!(
        ActiveAccountRow,
        r#"SELECT id AS "id!", name, kind, currency_code, ownership, person_id,
                  excluded_from_net_worth AS "excluded_from_net_worth!: bool"
             FROM accounts WHERE archived=0 ORDER BY sort_order, name"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(ActiveAccount::try_from)
    .collect()
}

/// One account by id (NotFound if it doesn't exist).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn account(db: &Db, id: i64) -> AppResult<AssetAccount> {
    sqlx::query_as!(
        AssetAccount,
        r#"SELECT id AS "id!", name, currency_code FROM accounts WHERE id=?1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound("account"))
}

/// Liabilities secured against `asset_id`, in display order.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn secured_liabilities(
    db: &Db,
    asset_id: i64,
) -> AppResult<Vec<SecuredLiabilityAccount>> {
    sqlx::query_as!(
        SecuredLiabilityAccountRow,
        r#"SELECT id AS "id!", name, kind, currency_code FROM accounts
             WHERE secured_by_account_id=?1 ORDER BY sort_order, name"#,
        asset_id
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(SecuredLiabilityAccount::try_from)
    .collect()
}

/// The raw row shape of the transaction *seed*: one per (account, currency), collapsing every
/// readable row before the window into a single (latest date, running total) pair.
///
/// Per *currency*, not per account: the sum is a plain SQL `SUM(amount_minor)`, which is only
/// a balance if every row it adds is denominated the same way. See [`LedgerTx`].
#[derive(Debug)]
struct LedgerSeedRow {
    account_id: i64,
    /// The latest readable `posted_at` before the window — `None` when every pre-window row
    /// of this account carries an unreadable one, in which case there is nothing to seed.
    posted_at: Option<String>,
    /// The sum of those rows' amounts: the account's opening balance for this window, in
    /// `currency_code`.
    amount_minor: i64,
    currency_code: String,
    /// How many pre-window rows were left out because their date is unreadable. Non-zero
    /// means legacy data — same posture as `parse_stored_date`: excluded, but never silently.
    unreadable: i64,
}

/// One synthetic transaction per account carrying everything posted before `from`.
///
/// This is the whole reason a report can stop loading the ledger from the beginning of time.
/// [`sure_app::reports::account_value_at`]'s running balance is `sum(amount) where date <= d`,
/// so a window that simply dropped the pre-history would report every cash account as though
/// it had opened on `from` — a wrong balance, which is far worse than the memory it saves. The
/// seed's *date* is the latest pre-window posting rather than an invented sentinel, so the
/// "has this account been opened yet?" test in the valuation-anchor branch (case 2) still
/// answers the same way: that branch only ever asks whether the first posting is on or before
/// a date inside the window, and both the true first posting and the seed are before `from`.
///
/// Returns nothing at all when `from` is the "no window" sentinel — `posted_at < ''` matches
/// no row, so the unwindowed read is unchanged.
///
/// The one way the aggregate can fail is SQLite's own `integer overflow` on `SUM` — a pre-window
/// total past `i64`, which needs rows written outside [`sure_core::Money`] (legacy data, a
/// provider import, a snapshot restore). That must not become a 500 on the balance sheet, so it
/// falls back to the individual rows and lets `sure_app::reports`' `i128` aggregation saturate
/// them loudly, exactly as it did before there was a seed at all.
async fn transaction_seed(db: &Db, from: &str) -> AppResult<Vec<LedgerTx>> {
    let rows = match seed_aggregate(db, from).await {
        Ok(rows) => rows,
        Err(e) if is_integer_overflow(&e) => {
            tracing::warn!(
                "an account's pre-window transactions do not sum inside i64: loading them \
                 individually instead of as one opening balance — some row holds an amount past \
                 sure_core::MAX_MONEY_MINOR (legacy data, a provider import or a snapshot \
                 restore); find it and repair it"
            );
            return pre_window_rows(db, from).await;
        }
        Err(e) => return Err(e.into()),
    };

    seed_rows(rows)
}

/// Whether `e` is SQLite refusing to keep an integer `SUM` exact.
///
/// `sqlx::Error` is `#[non_exhaustive]` upstream and only its `Database` variant carries the
/// engine's message, so this is an `if let` rather than a match with an arm for every variant
/// (which would also break the moment sqlx adds one).
fn is_integer_overflow(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db) = e {
        db.message().contains("integer overflow")
    } else {
        false
    }
}

/// Every readable transaction before the window, uncollapsed — the fallback above, and nothing
/// else. Costs the memory the seed exists to avoid, which is the right trade for a ledger that
/// cannot be summed at all.
async fn pre_window_rows(db: &Db, from: &str) -> AppResult<Vec<LedgerTx>> {
    Ok(sqlx::query_as!(
        LedgerTx,
        "SELECT account_id, posted_at, amount_minor, currency_code FROM transactions
          WHERE posted_at < ?1
            AND posted_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
                    AND date(posted_at) = substr(posted_at, 1, 10)",
        from
    )
    .fetch_all(db)
    .await?)
}

/// Turn the aggregate rows into seed transactions, warning about anything left out.
fn seed_rows(rows: Vec<LedgerSeedRow>) -> AppResult<Vec<LedgerTx>> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        if r.unreadable > 0 {
            tracing::warn!(
                account_id = r.account_id,
                currency = %r.currency_code,
                rows = r.unreadable,
                "transactions with an unreadable posted_at are not in this account's opening \
                 balance — legacy rows written before the date was a validated type; repair \
                 them to make them count again"
            );
        }
        if let Some(posted_at) = r.posted_at {
            out.push(LedgerTx {
                account_id: r.account_id,
                posted_at,
                amount_minor: r.amount_minor,
                currency_code: r.currency_code,
            });
        }
    }
    Ok(out)
}

/// One row per (account, currency) with pre-window history: its latest readable date, the exact
/// sum of those rows' amounts, and how many were left out as unreadable. `sqlx::Error` rather
/// than `AppError` so [`transaction_seed`] can recognise SQLite's `integer overflow` before it
/// is wrapped.
///
/// The `currency_code` in the `GROUP BY` is load-bearing, not cosmetic: without it this returns
/// one opening balance per account that has added every currency's minor units together, and
/// the whole windowed ledger inherits that number. An account with a single currency — every
/// account but a multi-currency brokerage — still collapses to exactly one row, so the memory
/// win the seed exists for is unchanged.
async fn seed_aggregate(db: &Db, from: &str) -> Result<Vec<LedgerSeedRow>, sqlx::Error> {
    sqlx::query_as!(
        LedgerSeedRow,
        r#"SELECT account_id AS "account_id!", currency_code AS "currency_code!",
                  MAX(CASE WHEN posted_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
                          AND date(posted_at) = substr(posted_at, 1, 10)
                           THEN posted_at END) AS "posted_at: String",
                  SUM(CASE WHEN posted_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
                          AND date(posted_at) = substr(posted_at, 1, 10)
                           THEN amount_minor ELSE 0 END) AS "amount_minor!: i64",
                  SUM(CASE WHEN posted_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
                          AND date(posted_at) = substr(posted_at, 1, 10)
                           THEN 0 ELSE 1 END) AS "unreadable!: i64"
             FROM transactions
            WHERE posted_at < ?1
            GROUP BY account_id, currency_code"#,
        from
    )
    .fetch_all(db)
    .await
}

/// Transactions a report needs to value accounts on any date from `from` onwards: every row
/// posted on or after it, plus [`transaction_seed`]'s one row per account for everything
/// before it. `None` loads the whole table (the forecast, which fits trends over all history).
///
/// **There is deliberately no upper bound.** The obvious `posted_at BETWEEN from AND to` is
/// wrong: `account_value_at`'s case 2 reconstructs a provider-synced liability's historical
/// balance *backwards* from its earliest valuation by subtracting the movements between the
/// reported date and that valuation — and that valuation is frequently after `to` (a mortgage
/// whose feed only knows today's balance, viewed over last summer). Cutting the rows at `to`
/// would quietly change every one of those balances. Nothing is normally posted in the future,
/// so the bound would save nothing anyway; the 500k rows are all *below* `from`, which is the
/// end this does bound.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn transactions(db: &Db, from: Option<NaiveDate>) -> AppResult<Vec<LedgerTx>> {
    let from = lower_bound(from);
    let mut rows = transaction_seed(db, &from).await?;
    rows.extend(
        sqlx::query_as!(
            LedgerTx,
            "SELECT account_id, posted_at, amount_minor, currency_code FROM transactions
              WHERE posted_at >= ?1",
            from
        )
        .fetch_all(db)
        .await?,
    );
    Ok(rows)
}

/// Valuations a report needs from `from` onwards: every row as of it or later, plus the single
/// latest readable one before it per account. `None` loads the whole table.
///
/// The pre-window seed here is a *real row*, not an aggregate — a valuation is a level, not a
/// movement, and `account_value_at`'s case 1 answers with the most recent one on or before the
/// date asked about. That row can be years older than the window (a house valued once at
/// purchase), so dropping it would read as an account worth nothing.
///
/// Ordered by `(as_of, id)` because two valuations can legitimately share a date — creating a
/// property seeds one at its purchase price and another at its opening market value, which may
/// be the same day — and `sure_app::reports::account_value_at` picks the *last* of equally-dated
/// rows (`Iterator::max_by_key`). Without an ORDER BY, which of the two it reads is up to
/// SQLite; with it, the one entered last wins, every time. The seed is emitted first and its
/// `as_of` is strictly below the window's, so the two halves concatenate into exactly the order
/// a single `ORDER BY as_of, id` produced before — and the seed's own tie-break is explicit
/// (`ORDER BY as_of DESC, id DESC`) rather than SQLite's arbitrary choice among equal maxima.
///
/// No upper bound, for the same reason as [`transactions`]: case 2's anchor is by definition a
/// valuation *after* the date being reported on.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn valuations(db: &Db, from: Option<NaiveDate>) -> AppResult<Vec<LedgerValuation>> {
    let from = lower_bound(from);
    let mut rows = sqlx::query_as!(
        LedgerValuation,
        // The subquery does not carry `valuations`' NOT NULL through, so each column is
        // forced back — `WHERE rn = 1` only ever yields real rows.
        r#"SELECT account_id AS "account_id!", as_of AS "as_of!",
                  value_minor AS "value_minor!", currency_code AS "currency_code!"
             FROM (SELECT account_id, as_of, value_minor, currency_code,
                          ROW_NUMBER() OVER (
                              PARTITION BY account_id ORDER BY as_of DESC, id DESC
                          ) AS rn
                     FROM valuations
                    WHERE as_of < ?1
                      AND as_of GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
                          AND date(as_of) = substr(as_of, 1, 10))
            WHERE rn = 1"#,
        from
    )
    .fetch_all(db)
    .await?;
    rows.extend(
        sqlx::query_as!(
            LedgerValuation,
            "SELECT account_id, as_of, value_minor, currency_code FROM valuations
              WHERE as_of >= ?1 ORDER BY as_of, id",
            from
        )
        .fetch_all(db)
        .await?,
    );
    Ok(rows)
}

/// Every category's shape.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn categories(db: &Db) -> AppResult<Vec<Category>> {
    sqlx::query_as!(
        CategoryRow,
        r#"SELECT id AS "id!", parent_id, name, color, kind FROM categories"#
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(Category::try_from)
    .collect()
}

/// Transactions posted within `from ..= to`, with the fields the spend reports need to filter
/// and roll up.
///
/// This one *is* a plain window: the spend reports (category breakdown, money-flow graph) total
/// the movements inside the period and nothing else — no running balance, no seeding, no
/// reaching past either edge. The bounds are a deliberate superset of the report's own filter
/// (`sure_app::reports::load_spend` still checks each parsed date), so the rows that survive are
/// exactly the rows that survived before: `to` is bounded by the day *after* it, and a row whose
/// date doesn't parse is dropped by `load_spend` as it always was.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn spend_transactions(
    db: &Db,
    from: NaiveDate,
    to: NaiveDate,
) -> AppResult<Vec<SpendTransaction>> {
    let from = bound(from);
    let to = day_after(to);
    sqlx::query_as!(
        SpendTransactionRow,
        r#"SELECT t.posted_at, t.amount_minor, t.currency_code, t.category_id,
                  t.is_one_off AS "is_one_off!: bool", t.linked_transaction_id,
                  t.account_id, a.name AS account_name,
                  a.kind AS account_kind, t.merchant_id,
                  -- `COALESCE` over two nullable columns describes as having no type at
                  -- all, so the decode type has to be named; `?` keeps it nullable, which
                  -- it genuinely is when a row carries neither a merchant nor payee text.
                  COALESCE(m.name, t.merchant) AS "merchant?: String",
                  t.ownership AS tx_ownership,
                  t.person_id AS tx_person_id, a.ownership AS account_ownership,
                  a.person_id AS account_person_id
             FROM transactions t
             JOIN accounts a ON a.id = t.account_id
             LEFT JOIN merchants m ON m.id = t.merchant_id
            WHERE t.posted_at >= ?1 AND t.posted_at < ?2"#,
        from,
        to
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(SpendTransaction::try_from)
    .collect()
}

/// The earliest transaction date on record, for defaulting an unbounded report window.
///
/// Restricted to dates a report can read: an unreadable one is not in any figure, so letting it
/// become the default window start would stretch every chart's x-axis to a date nothing is
/// plotted at (`01/07/2026` sorts below every real ISO date, so it would win a bare `MIN`).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn earliest_transaction_date(db: &Db) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT MIN(posted_at) AS "earliest: String"
             FROM transactions
            WHERE posted_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
                     AND date(posted_at) = substr(posted_at, 1, 10)"#
    )
    .fetch_one(db)
    .await?)
}

/// The earliest valuation date on record — the other half of the net-worth series' data-driven
/// default window start.
///
/// Net worth used to derive that date by loading the entire ledger and taking the minimum of
/// it; this is the same date for a fraction of the memory, and it has to exist separately from
/// [`earliest_transaction_date`] because an account can be valued before it is ever transacted
/// on (a house bought for cash, valued at purchase, with no transactions at all).
#[tracing::instrument(level = "debug", skip_all)]
pub async fn earliest_valuation_date(db: &Db) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar!(
        r#"SELECT MIN(as_of) AS "earliest: String"
             FROM valuations
            WHERE as_of GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]*'
              AND date(as_of) = substr(as_of, 1, 10)"#
    )
    .fetch_one(db)
    .await?)
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A migrated in-memory database with one joint account per id in `accounts`. Straight SQL
    /// rather than `crate::accounts::create` because these tests are about what the *reads*
    /// return for rows in shapes the writers no longer produce (a legacy date, a datetime).
    async fn db_with_accounts(accounts: &[i64]) -> Db {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&db).await.unwrap();
        for id in accounts {
            sqlx::query!(
                "INSERT INTO accounts (id, name, kind, currency_code, metadata, ownership)
                 VALUES (?1, 'Test', 'bank', 'NZD', '{}', 'joint')",
                id
            )
            .execute(&db)
            .await
            .unwrap();
        }
        db
    }

    async fn insert_tx(db: &Db, account_id: i64, posted_at: &str, amount_minor: i64) {
        insert_tx_in(db, account_id, posted_at, amount_minor, "NZD").await;
    }

    async fn insert_tx_in(
        db: &Db,
        account_id: i64,
        posted_at: &str,
        amount_minor: i64,
        currency_code: &str,
    ) {
        sqlx::query!(
            "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code)
             VALUES (?1, ?2, ?3, ?4)",
            account_id,
            posted_at,
            amount_minor,
            currency_code
        )
        .execute(db)
        .await
        .unwrap();
    }

    async fn insert_val(db: &Db, account_id: i64, as_of: &str, value_minor: i64) {
        sqlx::query!(
            "INSERT INTO valuations (account_id, as_of, value_minor, currency_code)
             VALUES (?1, ?2, ?3, 'NZD')",
            account_id,
            as_of,
            value_minor
        )
        .execute(db)
        .await
        .unwrap();
    }

    /// The seed is what makes a windowed read safe: an account whose only activity predates the
    /// window still arrives carrying its balance, collapsed into a single row dated at its last
    /// posting. Without it the balance sheet would read as though every account opened on `from`.
    #[tokio::test]
    async fn a_windowed_read_seeds_each_account_with_its_pre_window_balance() {
        let db = db_with_accounts(&[1, 2]).await;
        // Account 1: two postings well before the window, one inside it.
        insert_tx(&db, 1, "2020-01-01", 1_000_00).await;
        insert_tx(&db, 1, "2024-06-30", -250_00).await;
        insert_tx(&db, 1, "2026-02-01", 7_00).await;
        // Account 2: nothing but pre-window history.
        insert_tx(&db, 2, "2019-05-05", 42_00).await;

        let rows = transactions(&db, Some(d("2026-01-01"))).await.unwrap();

        let seed_1 = rows
            .iter()
            .find(|t| t.account_id == 1 && t.posted_at == "2024-06-30")
            .expect("account 1 is seeded at its latest pre-window posting");
        assert_eq!(
            seed_1.amount_minor, 750_00,
            "the seed is the exact sum of everything before the window"
        );
        let seed_2 = rows
            .iter()
            .find(|t| t.account_id == 2)
            .expect("an account with only pre-window history is still present");
        assert_eq!(seed_2.posted_at, "2019-05-05");
        assert_eq!(seed_2.amount_minor, 42_00);
        // The window's own row is untouched, and nothing else came along for the ride: three
        // pre-window rows became two seeds.
        assert!(
            rows.iter().any(|t| t.account_id == 1
                && t.posted_at == "2026-02-01"
                && t.amount_minor == 7_00)
        );
        assert_eq!(
            rows.len(),
            3,
            "two seeds plus the one in-window row: {rows:?}"
        );
    }

    /// A multi-currency account gets one seed **per currency**, not one seed holding a
    /// meaningless sum of all of them.
    ///
    /// The seed is a plain SQL `SUM(amount_minor)`, so grouping by account alone would add
    /// US and Australian cents onto New Zealand ones and hand the whole windowed ledger a
    /// single wrong opening balance — with no currency left on the row for
    /// `sure_app::reports::account_value_at` to convert back out of. This is the ordinary
    /// Sharesies shape: one account, an NZD/AUD/USD wallet each.
    #[tokio::test]
    async fn a_multi_currency_account_is_seeded_once_per_currency() {
        let db = db_with_accounts(&[1]).await;
        insert_tx_in(&db, 1, "2020-01-01", 100_00, "NZD").await;
        insert_tx_in(&db, 1, "2020-02-01", 25_00, "NZD").await;
        insert_tx_in(&db, 1, "2020-03-01", 50_00, "USD").await;
        insert_tx_in(&db, 1, "2020-04-01", 40_00, "AUD").await;

        let rows = transactions(&db, Some(d("2026-01-01"))).await.unwrap();

        let by_ccy = |c: &str| {
            rows.iter()
                .find(|t| t.currency_code == c)
                .unwrap_or_else(|| panic!("a {c} seed: {rows:?}"))
        };
        assert_eq!(rows.len(), 3, "one seed per currency held: {rows:?}");
        assert_eq!(by_ccy("NZD").amount_minor, 125_00);
        assert_eq!(by_ccy("USD").amount_minor, 50_00);
        assert_eq!(by_ccy("AUD").amount_minor, 40_00);
        // Each seed is dated at that currency's own latest pre-window posting, which is still
        // before the window — all case 2 asks of it.
        assert_eq!(by_ccy("NZD").posted_at, "2020-02-01");
        assert_eq!(by_ccy("USD").posted_at, "2020-03-01");
    }

    /// The ordinary account is unaffected: one currency still collapses to exactly one row, so
    /// the memory the seed exists to save is still saved.
    #[tokio::test]
    async fn a_single_currency_account_still_collapses_to_one_seed() {
        let db = db_with_accounts(&[1]).await;
        for day in ["2020-01-01", "2020-02-01", "2020-03-01", "2020-04-01"] {
            insert_tx(&db, 1, day, 10_00).await;
        }

        let rows = transactions(&db, Some(d("2026-01-01"))).await.unwrap();

        assert_eq!(rows.len(), 1, "four rows, one seed: {rows:?}");
        assert_eq!(rows[0].amount_minor, 40_00);
        assert_eq!(rows[0].currency_code, "NZD");
    }

    /// A legacy date no report can read stays out of the seed *and* is counted in a WARN. Folding
    /// it into the opening balance would silently move a figure it has never been part of; an
    /// aggregate has no later chance to drop it, which is why the filter is in the SQL.
    #[tokio::test]
    async fn an_unreadable_pre_window_date_is_left_out_of_the_seed() {
        let db = db_with_accounts(&[1]).await;
        insert_tx(&db, 1, "2020-01-01", 100_00).await;
        // Day-first, and a day that doesn't exist. Both sort below the window bound, so both
        // would otherwise land in the seed's sum.
        insert_tx(&db, 1, "01/07/2020", 500_00).await;
        insert_tx(&db, 1, "2020-02-30", 900_00).await;

        let rows = transactions(&db, Some(d("2026-01-01"))).await.unwrap();
        assert_eq!(rows.len(), 1, "one seed, no window rows: {rows:?}");
        assert_eq!(
            rows[0].amount_minor, 100_00,
            "only the readable row is in the opening balance"
        );
        assert_eq!(rows[0].posted_at, "2020-01-01");
    }

    /// A row past `i64` in aggregate — only reachable outside `sure_core::Money` — makes SQLite
    /// refuse the `SUM`. That must not be a 500 on the balance sheet: the read falls back to the
    /// individual rows, which `sure_app::reports` sums in `i128` and saturates loudly.
    #[tokio::test]
    async fn a_pre_window_total_past_i64_falls_back_to_the_individual_rows() {
        let db = db_with_accounts(&[1]).await;
        insert_tx(&db, 1, "2026-01-05", i64::MAX).await;
        insert_tx(&db, 1, "2026-01-06", i64::MAX).await;

        let rows = transactions(&db, Some(d("2026-08-04"))).await.unwrap();
        assert_eq!(
            rows.len(),
            2,
            "both rows arrive uncollapsed rather than the read failing: {rows:?}"
        );
        assert!(rows.iter().all(|t| t.amount_minor == i64::MAX));
    }

    /// A valuation is a level, not a movement: the seed is the single latest *real* row before
    /// the window (a house valued once at purchase is still worth that), and equally-dated rows
    /// tie-break by id — the same "last one entered wins" the full-table read guaranteed with
    /// `ORDER BY as_of, id`.
    #[tokio::test]
    async fn the_valuation_seed_is_the_latest_row_before_the_window_tie_broken_by_id() {
        let db = db_with_accounts(&[1, 2]).await;
        insert_val(&db, 1, "2020-01-01", 700_000_00).await;
        // Two on the same day: a purchase price and an opening market value.
        insert_val(&db, 1, "2024-01-01", 800_000_00).await;
        insert_val(&db, 1, "2024-01-01", 810_000_00).await;
        insert_val(&db, 1, "2026-03-01", 900_000_00).await;
        // Unreadable, and the only pre-window row this account has.
        insert_val(&db, 2, "01/07/2020", 5_00).await;

        let rows = valuations(&db, Some(d("2026-01-01"))).await.unwrap();
        let seeds: Vec<_> = rows
            .iter()
            .filter(|v| v.as_of.as_str() < "2026-01-01")
            .collect();
        assert_eq!(
            seeds.len(),
            1,
            "one seed per account with history: {rows:?}"
        );
        assert_eq!(seeds[0].account_id, 1);
        assert_eq!(seeds[0].as_of, "2024-01-01");
        assert_eq!(
            seeds[0].value_minor, 810_000_00,
            "the later-entered of two equally-dated valuations wins"
        );
        // The seed comes first, so each account's rows are still in ascending date order.
        assert_eq!(rows[0].as_of, "2024-01-01");
        assert_eq!(rows[1].as_of, "2026-03-01");
        assert_eq!(rows.len(), 2, "the unreadable row is not a seed: {rows:?}");
    }

    /// `None` is the forecast's read: the whole table, exactly as before the window existed —
    /// every row individually, including the ones no report can date (which the caller drops,
    /// loudly, as it always has).
    #[tokio::test]
    async fn an_unwindowed_read_returns_every_row_uncollapsed() {
        let db = db_with_accounts(&[1]).await;
        insert_tx(&db, 1, "2020-01-01", 100_00).await;
        insert_tx(&db, 1, "2024-06-30", 200_00).await;
        insert_tx(&db, 1, "01/07/2020", 300_00).await;
        insert_val(&db, 1, "2020-01-01", 1).await;
        insert_val(&db, 1, "2024-06-30", 2).await;

        assert_eq!(transactions(&db, None).await.unwrap().len(), 3);
        assert_eq!(valuations(&db, None).await.unwrap().len(), 2);
    }

    /// A window is a window: the spend read hands back the period and nothing else, with both
    /// edges inclusive of the whole day — a legacy row carrying a *time* still counts on its own
    /// day, because the caller's filter truncates to ten characters and this must not be tighter.
    #[tokio::test]
    async fn the_spend_read_covers_whole_days_at_both_edges() {
        let db = db_with_accounts(&[1]).await;
        insert_tx(&db, 1, "2025-12-31", 1_00).await;
        insert_tx(&db, 1, "2026-01-01", 2_00).await;
        insert_tx(&db, 1, "2026-06-30T23:59:00", 3_00).await;
        insert_tx(&db, 1, "2026-07-01", 4_00).await;

        let rows = spend_transactions(&db, d("2026-01-01"), d("2026-06-30"))
            .await
            .unwrap();
        let mut amounts: Vec<i64> = rows.iter().map(|t| t.amount_minor).collect();
        amounts.sort_unstable();
        assert_eq!(amounts, vec![2_00, 3_00], "got {rows:?}");
    }

    /// The two window defaults. Both ignore a date no figure is plotted at: `01/07/2020` sorts
    /// below every real ISO date, so a bare `MIN` would stretch every chart back to a day nothing
    /// happened on.
    #[tokio::test]
    async fn the_earliest_dates_ignore_rows_no_report_can_read() {
        let db = db_with_accounts(&[1]).await;
        insert_tx(&db, 1, "01/07/2020", 1_00).await;
        insert_tx(&db, 1, "2022-03-04", 2_00).await;
        insert_val(&db, 1, "01/07/2019", 1).await;
        insert_val(&db, 1, "2021-02-03", 2).await;

        assert_eq!(
            earliest_transaction_date(&db).await.unwrap().as_deref(),
            Some("2022-03-04")
        );
        assert_eq!(
            earliest_valuation_date(&db).await.unwrap().as_deref(),
            Some("2021-02-03")
        );
        // An empty table is still `None`, which is what makes a report fall back to `to`.
        let empty = db_with_accounts(&[]).await;
        assert_eq!(earliest_transaction_date(&empty).await.unwrap(), None);
        assert_eq!(earliest_valuation_date(&empty).await.unwrap(), None);
    }
}
