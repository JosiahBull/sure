//! Full config/data snapshot export & import. Export serialises every domain table
//! (ids preserved); import wipes the database and restores in one transaction with
//! `PRAGMA defer_foreign_keys=ON`. Pure audit/run tables are cleared but not restored.

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use sure_core::{AppError, AppResult};

use crate::Db;

pub const SNAPSHOT_VERSION: i64 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: i64,
    pub base_currency_code: String,
    pub currencies: Vec<CurrencyRow>,
    pub exchange_rates: Vec<ExchangeRateRow>,
    pub categories: Vec<CategoryRow>,
    pub merchants: Vec<MerchantRow>,
    /// The household. `#[serde(default)]` so a snapshot taken before people existed still
    /// imports — as an empty household, which is exactly what it had.
    #[serde(default)]
    pub people: Vec<PersonRow>,
    pub accounts: Vec<AccountRow>,
    pub transactions: Vec<TransactionRow>,
    pub valuations: Vec<ValuationRow>,
    pub rules: Vec<RuleRow>,
    pub crons: Vec<CronRow>,
    pub providers: Vec<ProviderRow>,
    pub equity_grants: Vec<GrantRow>,
    pub equity_exercises: Vec<ExerciseRow>,
    // Brokerage tables — `#[serde(default)]` so snapshots taken before these existed
    // still import (as empty), rather than failing to deserialize.
    #[serde(default)]
    pub holdings: Vec<HoldingRow>,
    #[serde(default)]
    pub dividends: Vec<DividendRow>,
    #[serde(default)]
    pub dividend_withholdings: Vec<DividendWithholdingRow>,
    // Forecast assumption overrides + known future events — `#[serde(default)]` so
    // snapshots taken before these tables existed still import (as empty).
    #[serde(default)]
    pub forecast_assumptions: Vec<ForecastAssumptionRow>,
    #[serde(default)]
    pub forecast_events: Vec<ForecastEventRow>,
    /// An event's effects and its ordering/conditional links. Restored with the events rather
    /// than left behind: the simulation branches on an event's *effects*, never on its kind
    /// (see `0022_forecast_events_unified.sql`), so an event restored without them is a row the
    /// UI shows and the projection ignores — which reads as a successful restore and is not one.
    #[serde(default)]
    pub forecast_event_effects: Vec<ForecastEventEffectRow>,
    #[serde(default)]
    pub forecast_event_relations: Vec<ForecastEventRelationRow>,
    // Per-person income streams and their dated pay-scale steps — `#[serde(default)]` so a
    // snapshot taken before 0021 still imports, as a household with no modelled income.
    #[serde(default)]
    pub income_streams: Vec<IncomeStreamRow>,
    #[serde(default)]
    pub income_stream_steps: Vec<IncomeStreamStepRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CurrencyRow {
    pub code: String,
    pub name: String,
    pub symbol: String,
    pub decimal_places: i64,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExchangeRateRow {
    pub base_code: String,
    pub quote_code: String,
    pub as_of: String,
    pub rate: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CategoryRow {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MerchantRow {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PersonRow {
    pub id: i64,
    pub name: String,
    pub color: Option<String>,
    pub sort_order: i64,
    #[serde(default)]
    pub placeholder: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
    pub institution: Option<String>,
    pub metadata: String,
    pub archived: bool,
    /// Defaulted for the same reason `ownership` is: a snapshot taken before 0034 has no
    /// such column, and `false` is exactly the state that database was in.
    #[serde(default)]
    pub excluded_from_net_worth: bool,
    pub sort_order: i64,
    pub secured_by_account_id: Option<i64>,
    // Ownership travels as its two stored columns rather than the `Ownership` enum: a
    // snapshot is a row-for-row copy of the database. Both default, so a snapshot from
    // before accounts had owners still deserialises; `ownership_columns` is what decides
    // where those rows land on the way back in.
    #[serde(default)]
    pub ownership: Option<String>,
    #[serde(default)]
    pub person_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl AccountRow {
    /// Whether this row arrived without a usable owner — either from a pre-household
    /// snapshot (no `ownership` at all) or carrying the `unattributed` state that 0016
    /// removed. Both need the placeholder.
    fn needs_placeholder_owner(&self) -> bool {
        !matches!(self.ownership.as_deref(), Some("person") | Some("joint"))
    }

    /// The `(ownership, person_id)` pair to store, given the placeholder person's id (which
    /// is present exactly when some row in this snapshot needed one).
    fn ownership_columns(&self, placeholder_id: Option<i64>) -> (&str, Option<i64>) {
        match (self.needs_placeholder_owner(), placeholder_id) {
            (false, _) => (self.ownership.as_deref().unwrap_or("joint"), self.person_id),
            (true, Some(id)) => ("person", Some(id)),
            // Unreachable: the placeholder is created whenever any row needs it. Joint is
            // the honest fallback rather than a panic on an import path — it names no
            // individual, so it can't misattribute anyone's money.
            (true, None) => ("joint", None),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransactionRow {
    pub id: i64,
    pub account_id: i64,
    pub posted_at: String,
    pub amount_minor: i64,
    pub currency_code: String,
    pub description: String,
    pub merchant: Option<String>,
    pub notes: Option<String>,
    pub category_id: Option<i64>,
    pub is_one_off: bool,
    pub linked_transaction_id: Option<i64>,
    pub provider: Option<String>,
    pub external_id: Option<String>,
    pub categorized_by_rule_id: Option<i64>,
    pub merchant_id: Option<i64>,
    // The per-transaction attribution override, as its two stored columns. Both default,
    // so a snapshot from before transactions had one restores as "inherit the account's
    // owner" — which is what those rows meant.
    #[serde(default)]
    pub ownership: Option<String>,
    #[serde(default)]
    pub person_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValuationRow {
    pub id: i64,
    pub account_id: i64,
    pub as_of: String,
    pub value_minor: i64,
    pub currency_code: String,
    pub source: String,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RuleRow {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub expression: String,
    pub set_category_id: Option<i64>,
    pub set_one_off: Option<bool>,
    pub overwrite_manual: bool,
    pub stop_on_match: bool,
    pub priority: i64,
    pub enabled: bool,
    pub set_merchant_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CronRow {
    pub id: i64,
    pub name: String,
    pub account_id: i64,
    pub kind: String,
    pub rate_bps: Option<i64>,
    pub amount_minor: Option<i64>,
    pub category_id: Option<i64>,
    pub frequency: String,
    pub day_of_month: i64,
    pub start_date: String,
    pub last_run_on: Option<String>,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProviderRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub account_id: i64,
    pub config: String,
    pub enabled: bool,
    pub last_synced_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GrantRow {
    pub id: i64,
    pub account_id: i64,
    pub company: String,
    pub grant_date: String,
    pub quantity: i64,
    pub strike_minor: i64,
    pub currency_code: String,
    pub vest_months: i64,
    pub cliff_months: i64,
    pub unit_value_minor: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExerciseRow {
    pub id: i64,
    pub grant_id: i64,
    pub exercise_date: String,
    pub quantity: i64,
    pub price_minor: i64,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HoldingRow {
    pub id: i64,
    pub account_id: i64,
    pub ticker: String,
    pub exchange: String,
    pub name: Option<String>,
    pub currency_code: String,
    pub trade_date: String,
    pub quantity: f64,
    pub unit_price: Option<f64>,
    pub fee_minor: i64,
    pub kind: String,
    pub external_id: Option<String>,
    pub provider: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DividendRow {
    pub id: i64,
    pub account_id: i64,
    pub ticker: String,
    pub exchange: String,
    pub record_date: Option<String>,
    pub paid_date: String,
    pub shares_held: Option<f64>,
    pub gross_amount_minor: i64,
    pub net_amount_minor: i64,
    pub currency_code: String,
    pub external_id: Option<String>,
    pub provider: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DividendWithholdingRow {
    pub id: i64,
    pub dividend_id: i64,
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ForecastAssumptionRow {
    pub id: i64,
    pub target_type: String,
    pub target_id: i64,
    pub annual_growth_bps: Option<i64>,
    pub annual_volatility_bps: Option<i64>,
    pub dividend_yield_bps: Option<i64>,
    /// `#[serde(default)]` so a snapshot taken before 0020 added the column still imports,
    /// as `NULL` — which is exactly the value that migration gives every existing row.
    #[serde(default)]
    pub long_run_growth_bps: Option<i64>,
    /// Added by 0024 — `#[serde(default)]` so an older snapshot imports as "fees not modelled",
    /// which is what it meant.
    #[serde(default)]
    pub annual_fee_bps: Option<i64>,
    #[serde(default)]
    pub annual_fixed_fee_minor: Option<i64>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IncomeStreamRow {
    pub id: i64,
    pub person_id: i64,
    pub label: String,
    pub employer: Option<String>,
    pub currency_code: String,
    pub annual_amount_minor: i64,
    pub basis: String,
    pub pay_frequency: String,
    pub first_payment_on: String,
    pub starts_on: String,
    pub ends_on: Option<String>,
    pub annual_increase_bps: i64,
    pub kiwisaver_bps: i64,
    pub student_loan: bool,
    pub take_home_bps: Option<i64>,
    pub linked_category_id: Option<i64>,
    pub enabled: bool,
    pub sort_order: i64,
    pub notes: Option<String>,
    /// Added by 0023 — `#[serde(default)]` so a snapshot taken before contributions could be routed
    /// still imports, with the money going nowhere exactly as it did then.
    #[serde(default)]
    pub employer_kiwisaver_bps: i64,
    #[serde(default)]
    pub kiwisaver_account_id: Option<i64>,
    #[serde(default)]
    pub student_loan_account_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IncomeStreamStepRow {
    pub id: i64,
    pub income_stream_id: i64,
    pub effective_on: String,
    pub annual_amount_minor: i64,
    pub label: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ForecastEventRow {
    pub id: i64,
    pub label: String,
    pub kind: String,
    pub person_id: Option<i64>,
    pub expected_on: String,
    pub timing_spread_months: i64,
    pub probability_bps: i64,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ForecastEventEffectRow {
    pub id: i64,
    pub event_id: i64,
    pub kind: String,
    pub sort_order: i64,
    pub income_stream_id: Option<i64>,
    pub person_id: Option<i64>,
    pub category_id: Option<i64>,
    pub account_id: Option<i64>,
    pub amount_minor: Option<i64>,
    pub rate_bps: Option<i64>,
    pub delay_months: Option<i64>,
    pub ramp_months: Option<i64>,
    pub duration_months: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ForecastEventRelationRow {
    pub id: i64,
    pub event_id: i64,
    pub depends_on_event_id: i64,
    pub kind: String,
    pub min_gap_months: i64,
    pub created_at: String,
}

/// Serialising the snapshot cannot fail on the data (every field is a plain scalar), so an
/// error here is a bug or a full disk on the way out to a `Vec`, not bad user input.
fn ser_failed(e: serde_json::Error) -> AppError {
    AppError::Internal(e.into())
}

/// The snapshot as JSON bytes, written one table at a time.
///
/// Byte-identical to `serde_json::to_vec(&export(db)?)` — the format is a user-facing backup
/// contract (an old snapshot must still import), so this changes *when* memory is held, never
/// what is in the blob. `export_bytes_matches_the_snapshot_struct` pins that equality, which is
/// what keeps this writer from drifting away from [`Snapshot`]'s field names.
///
/// What it avoids: `GET /api/config/export` used to hold three full copies of the database at
/// once — every table's rows as `Vec`s inside a [`Snapshot`], then `serde_json::to_value` of the
/// lot (the fattest of the three: a `Value` tree with a `String` key per field per row), then
/// the serialised response body — and it takes no parameters, so any caller can ask for that,
/// times the in-flight ceiling. Here each table's `Vec` is dropped as soon as its entry has
/// been written, so the peak is the finished bytes plus the single largest table.
///
/// Residual, stated plainly: the finished blob is still assembled in memory before the response
/// starts. Removing that means streaming the body while rows are still being read — a
/// `Body::from_stream` over a `sqlx` cursor — which the current `Snapshot`-shaped format can
/// carry (it is one object of arrays) but which would have to hold the transaction that reads
/// the tables open across the whole write, and give up the "either the whole snapshot or an
/// error" guarantee a backup wants. Not worth it for a self-hosted single-household app; the
/// per-request peak is now ~1.2 copies instead of ~3, and the handler admits one at a time.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn export_bytes(db: &Db) -> AppResult<Vec<u8>> {
    let base_currency_code =
        sqlx::query_scalar!("SELECT base_currency_code FROM settings WHERE id=1")
            .fetch_one(db)
            .await?;

    let mut out: Vec<u8> = Vec::new();
    let mut ser = serde_json::Serializer::new(&mut out);
    let mut map = ser.serialize_map(None).map_err(ser_failed)?;
    map.serialize_entry("version", &SNAPSHOT_VERSION)
        .map_err(ser_failed)?;
    map.serialize_entry("base_currency_code", &base_currency_code)
        .map_err(ser_failed)?;

    /// Read one table, write it straight into the buffer as `"<field>": [...]`, and drop the
    /// rows before the next table is read — the entire point of this function. The field name
    /// must match [`Snapshot`]'s, and the test named above is what proves it does.
    macro_rules! table {
        ($field:literal, $ty:ty, $sql:literal) => {{
            let rows: Vec<$ty> = sqlx::query_as!($ty, $sql).fetch_all(db).await?;
            map.serialize_entry($field, &rows).map_err(ser_failed)?;
        }};
    }

    table!(
        "currencies",
        CurrencyRow,
        r#"SELECT code, name, symbol, decimal_places, created_at
             FROM currencies ORDER BY code"#
    );
    table!(
        "exchange_rates",
        ExchangeRateRow,
        r#"SELECT base_code, quote_code, as_of, rate
             FROM exchange_rates"#
    );
    table!(
        "categories",
        CategoryRow,
        r#"SELECT id AS "id!", name, parent_id, kind, color, icon, sort_order, created_at
             FROM categories ORDER BY id"#
    );
    table!(
        "merchants",
        MerchantRow,
        r#"SELECT id AS "id!", name, category_id, note, created_at, updated_at
             FROM merchants ORDER BY id"#
    );
    table!(
        "people",
        PersonRow,
        r#"SELECT id AS "id!", name, color, sort_order, placeholder AS "placeholder!: bool",
                  created_at, updated_at
             FROM people ORDER BY id"#
    );
    table!(
        "accounts",
        AccountRow,
        r#"SELECT id AS "id!", name, kind, currency_code, institution, metadata,
                  archived AS "archived!: bool",
                  excluded_from_net_worth AS "excluded_from_net_worth!: bool",
                  sort_order, secured_by_account_id,
                  ownership AS "ownership?", person_id, created_at, updated_at
             FROM accounts ORDER BY id"#
    );
    table!(
        "transactions",
        TransactionRow,
        r#"SELECT id AS "id!", account_id, posted_at, amount_minor, currency_code, description,
                  merchant, notes, category_id, is_one_off AS "is_one_off!: bool",
                  linked_transaction_id, provider, external_id, categorized_by_rule_id,
                  merchant_id, ownership, person_id, created_at, updated_at
             FROM transactions ORDER BY id"#
    );
    table!(
        "valuations",
        ValuationRow,
        r#"SELECT id AS "id!", account_id, as_of, value_minor, currency_code, source, note,
                  created_at
             FROM valuations ORDER BY id"#
    );
    table!(
        "rules",
        RuleRow,
        r#"SELECT id AS "id!", name, description, expression, set_category_id,
                  set_one_off AS "set_one_off: bool",
                  overwrite_manual AS "overwrite_manual!: bool",
                  stop_on_match AS "stop_on_match!: bool", priority, enabled AS "enabled!: bool",
                  set_merchant_id, created_at, updated_at
             FROM rules ORDER BY id"#
    );
    table!(
        "crons",
        CronRow,
        r#"SELECT id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                  frequency, day_of_month, start_date, last_run_on, enabled AS "enabled!: bool",
                  created_at, updated_at
             FROM crons ORDER BY id"#
    );
    table!(
        "providers",
        ProviderRow,
        r#"SELECT id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                  last_synced_at, created_at, updated_at
             FROM providers ORDER BY id"#
    );
    table!(
        "equity_grants",
        GrantRow,
        r#"SELECT id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                  currency_code, vest_months, cliff_months, unit_value_minor, note, created_at,
                  updated_at
             FROM equity_grants ORDER BY id"#
    );
    table!(
        "equity_exercises",
        ExerciseRow,
        r#"SELECT id AS "id!", grant_id, exercise_date, quantity, price_minor, note, created_at
             FROM equity_exercises ORDER BY id"#
    );
    table!(
        "holdings",
        HoldingRow,
        r#"SELECT id AS "id!", account_id, ticker, exchange, name, currency_code, trade_date,
                  quantity, unit_price, fee_minor, kind, external_id, provider, created_at
             FROM holdings ORDER BY id"#
    );
    table!(
        "dividends",
        DividendRow,
        r#"SELECT id AS "id!", account_id, ticker, exchange, record_date, paid_date, shares_held,
                  gross_amount_minor, net_amount_minor, currency_code, external_id, provider,
                  created_at
             FROM dividends ORDER BY id"#
    );
    table!(
        "dividend_withholdings",
        DividendWithholdingRow,
        r#"SELECT id AS "id!", dividend_id, owed_to, tax_amount_minor, tax_credit_minor,
                  currency_code
             FROM dividend_withholdings ORDER BY id"#
    );
    table!(
        "forecast_assumptions",
        ForecastAssumptionRow,
        r#"SELECT id AS "id!", target_type, target_id, annual_growth_bps, annual_volatility_bps,
                  dividend_yield_bps, long_run_growth_bps, annual_fee_bps,
                  annual_fixed_fee_minor, notes, created_at, updated_at
             FROM forecast_assumptions ORDER BY id"#
    );
    table!(
        "income_streams",
        IncomeStreamRow,
        r#"SELECT id AS "id!", person_id, label, employer, currency_code, annual_amount_minor,
                  basis, pay_frequency, first_payment_on, starts_on, ends_on,
                  annual_increase_bps, kiwisaver_bps, student_loan AS "student_loan!: bool",
                  take_home_bps, linked_category_id, enabled AS "enabled!: bool", sort_order,
                  notes, employer_kiwisaver_bps, kiwisaver_account_id, student_loan_account_id,
                  created_at, updated_at
             FROM income_streams ORDER BY id"#
    );
    table!(
        "income_stream_steps",
        IncomeStreamStepRow,
        r#"SELECT id AS "id!", income_stream_id, effective_on, annual_amount_minor, label,
                  created_at
             FROM income_stream_steps ORDER BY id"#
    );
    table!(
        "forecast_events",
        ForecastEventRow,
        r#"SELECT id AS "id!", label, kind, person_id, expected_on, timing_spread_months,
                  probability_bps, notes, created_at, updated_at
             FROM forecast_events ORDER BY id"#
    );
    table!(
        "forecast_event_effects",
        ForecastEventEffectRow,
        r#"SELECT id AS "id!", event_id, kind, sort_order, income_stream_id, person_id,
                  category_id, account_id, amount_minor, rate_bps, delay_months, ramp_months,
                  duration_months, created_at
             FROM forecast_event_effects ORDER BY id"#
    );
    table!(
        "forecast_event_relations",
        ForecastEventRelationRow,
        r#"SELECT id AS "id!", event_id, depends_on_event_id, kind, min_gap_months, created_at
             FROM forecast_event_relations ORDER BY id"#
    );

    map.end().map_err(ser_failed)?;
    Ok(out)
}

/// The snapshot as a [`Snapshot`], holding every table at once.
///
/// This is the *reference definition* of the export format — [`export_bytes`] is what the
/// endpoint actually serves, and the round-trip test asserts the two agree field for field. Kept
/// because `Snapshot` is the type [`import`] deserialises into, so having one place that says
/// "these are the tables, in this order, under these names" is what makes a format change
/// impossible to make in only half the code.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn export(db: &Db) -> AppResult<Snapshot> {
    let base_currency_code =
        sqlx::query_scalar!("SELECT base_currency_code FROM settings WHERE id=1")
            .fetch_one(db)
            .await?;

    Ok(Snapshot {
        version: SNAPSHOT_VERSION,
        base_currency_code,
        currencies: sqlx::query_as!(
            CurrencyRow,
            r#"SELECT code, name, symbol, decimal_places, created_at
                 FROM currencies ORDER BY code"#
        )
        .fetch_all(db)
        .await?,
        exchange_rates: sqlx::query_as!(
            ExchangeRateRow,
            r#"SELECT base_code, quote_code, as_of, rate
                 FROM exchange_rates"#
        )
        .fetch_all(db)
        .await?,
        categories: sqlx::query_as!(
            CategoryRow,
            r#"SELECT id AS "id!", name, parent_id, kind, color, icon, sort_order, created_at
                 FROM categories ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        merchants: sqlx::query_as!(
            MerchantRow,
            r#"SELECT id AS "id!", name, category_id, note, created_at, updated_at
                 FROM merchants ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        people: sqlx::query_as!(
            PersonRow,
            r#"SELECT id AS "id!", name, color, sort_order, placeholder AS "placeholder!: bool",
                      created_at, updated_at
                 FROM people ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        accounts: sqlx::query_as!(
            AccountRow,
            r#"SELECT id AS "id!", name, kind, currency_code, institution, metadata,
                      archived AS "archived!: bool",
                      excluded_from_net_worth AS "excluded_from_net_worth!: bool",
                      sort_order, secured_by_account_id,
                      ownership AS "ownership?", person_id, created_at, updated_at
                 FROM accounts ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        transactions: sqlx::query_as!(
            TransactionRow,
            r#"SELECT id AS "id!", account_id, posted_at, amount_minor, currency_code,
                      description, merchant, notes, category_id,
                      is_one_off AS "is_one_off!: bool", linked_transaction_id, provider,
                      external_id, categorized_by_rule_id, merchant_id, ownership, person_id,
                      created_at, updated_at
                 FROM transactions ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        valuations: sqlx::query_as!(
            ValuationRow,
            r#"SELECT id AS "id!", account_id, as_of, value_minor, currency_code, source, note,
                      created_at
                 FROM valuations ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        rules: sqlx::query_as!(
            RuleRow,
            r#"SELECT id AS "id!", name, description, expression, set_category_id,
                      set_one_off AS "set_one_off: bool",
                      overwrite_manual AS "overwrite_manual!: bool",
                      stop_on_match AS "stop_on_match!: bool", priority,
                      enabled AS "enabled!: bool", set_merchant_id, created_at, updated_at
                 FROM rules ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        crons: sqlx::query_as!(
            CronRow,
            r#"SELECT id AS "id!", name, account_id, kind, rate_bps, amount_minor, category_id,
                      frequency, day_of_month, start_date, last_run_on,
                      enabled AS "enabled!: bool", created_at, updated_at
                 FROM crons ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        providers: sqlx::query_as!(
            ProviderRow,
            r#"SELECT id AS "id!", name, kind, account_id, config, enabled AS "enabled!: bool",
                      last_synced_at, created_at, updated_at
                 FROM providers ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        equity_grants: sqlx::query_as!(
            GrantRow,
            r#"SELECT id AS "id!", account_id, company, grant_date, quantity, strike_minor,
                      currency_code, vest_months, cliff_months, unit_value_minor, note,
                      created_at, updated_at
                 FROM equity_grants ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        equity_exercises: sqlx::query_as!(
            ExerciseRow,
            r#"SELECT id AS "id!", grant_id, exercise_date, quantity, price_minor, note,
                      created_at
                 FROM equity_exercises ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        holdings: sqlx::query_as!(
            HoldingRow,
            r#"SELECT id AS "id!", account_id, ticker, exchange, name, currency_code, trade_date,
                      quantity, unit_price, fee_minor, kind, external_id, provider, created_at
                 FROM holdings ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        dividends: sqlx::query_as!(
            DividendRow,
            r#"SELECT id AS "id!", account_id, ticker, exchange, record_date, paid_date,
                      shares_held, gross_amount_minor, net_amount_minor, currency_code,
                      external_id, provider, created_at
                 FROM dividends ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        dividend_withholdings: sqlx::query_as!(
            DividendWithholdingRow,
            r#"SELECT id AS "id!", dividend_id, owed_to, tax_amount_minor, tax_credit_minor,
                      currency_code
                 FROM dividend_withholdings ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        income_streams: sqlx::query_as!(
            IncomeStreamRow,
            r#"SELECT id AS "id!", person_id, label, employer, currency_code,
                      annual_amount_minor, basis, pay_frequency, first_payment_on, starts_on,
                      ends_on, annual_increase_bps, kiwisaver_bps,
                      student_loan AS "student_loan!: bool", take_home_bps, linked_category_id,
                      enabled AS "enabled!: bool", sort_order, notes, employer_kiwisaver_bps,
                      kiwisaver_account_id, student_loan_account_id, created_at, updated_at
                 FROM income_streams ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        income_stream_steps: sqlx::query_as!(
            IncomeStreamStepRow,
            r#"SELECT id AS "id!", income_stream_id, effective_on, annual_amount_minor, label,
                      created_at
                 FROM income_stream_steps ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        forecast_assumptions: sqlx::query_as!(
            ForecastAssumptionRow,
            r#"SELECT id AS "id!", target_type, target_id, annual_growth_bps,
                      annual_volatility_bps, dividend_yield_bps, long_run_growth_bps,
                      annual_fee_bps, annual_fixed_fee_minor, notes, created_at, updated_at
                 FROM forecast_assumptions ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        forecast_events: sqlx::query_as!(
            ForecastEventRow,
            r#"SELECT id AS "id!", label, kind, person_id, expected_on, timing_spread_months,
                      probability_bps, notes, created_at, updated_at
                 FROM forecast_events ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        forecast_event_effects: sqlx::query_as!(
            ForecastEventEffectRow,
            r#"SELECT id AS "id!", event_id, kind, sort_order, income_stream_id, person_id,
                      category_id, account_id, amount_minor, rate_bps, delay_months, ramp_months,
                      duration_months, created_at
                 FROM forecast_event_effects ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
        forecast_event_relations: sqlx::query_as!(
            ForecastEventRelationRow,
            r#"SELECT id AS "id!", event_id, depends_on_event_id, kind, min_gap_months, created_at
                 FROM forecast_event_relations ORDER BY id"#
        )
        .fetch_all(db)
        .await?,
    })
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn import(db: &Db, snap: Snapshot) -> AppResult<Value> {
    let mut txn = db.begin().await?;
    // Defer FK checks so rows can be cleared and re-inserted in any order.
    // A PRAGMA, not a query over the schema — there is nothing for the compile-time checker to
    // verify, and `sqlx::query!` would try to describe it.
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *txn)
        .await?;

    /// One checked `DELETE` per table, in the order given. A `for` loop over table *names*
    /// would need `format!` to build each statement, which the compile-time checker cannot see
    /// — so a table that goes away would be a runtime error on import instead of a build
    /// failure. Expanding one statement per literal keeps both the ordering and the checking.
    macro_rules! wipe {
        ($($sql:literal),* $(,)?) => {{
            $( sqlx::query!($sql).execute(&mut *txn).await?; )*
        }};
    }
    wipe!(
        "DELETE FROM rule_applications",
        "DELETE FROM rule_runs",
        "DELETE FROM cron_runs",
        "DELETE FROM provider_syncs",
        // Audit, like the four above: a log of actions taken against *this* database, so it is
        // cleared and not restored. Re-inserting another database's log would be a false history.
        "DELETE FROM imports",
        // `forecast_event_effects` and `forecast_event_relations` are not listed: both are
        // `ON DELETE CASCADE` on `event_id`, so clearing the events clears them too. They are
        // re-inserted below with the events, not dropped.
        "DELETE FROM forecast_events",
        "DELETE FROM forecast_assumptions",
        "DELETE FROM income_stream_steps",
        "DELETE FROM income_streams",
        "DELETE FROM dividend_withholdings",
        "DELETE FROM dividends",
        "DELETE FROM holdings",
        "DELETE FROM equity_exercises",
        "DELETE FROM equity_grants",
        "DELETE FROM valuations",
        "DELETE FROM transactions",
        "DELETE FROM providers",
        "DELETE FROM crons",
        "DELETE FROM rules",
        "DELETE FROM merchants",
        // After `accounts`, which references it.
        "DELETE FROM accounts",
        "DELETE FROM people",
        "DELETE FROM categories",
        "DELETE FROM exchange_rates",
        "DELETE FROM currencies",
    );

    // `scheduled_task_runs` is *not* wiped (it is process state, not user data, and re-running
    // every task on import would be worse), so the scheduler still believes every poll ran
    // recently — against data that has just been replaced wholesale underneath it. Two tasks
    // cannot wait that out, so their last run is forgotten and the next tick re-polls:
    //
    // * the FX poll, because the wipe clears `exchange_rates` and a snapshot taken before the
    //   poller existed (or from a database that never polled) restores none — leaving every
    //   foreign-currency figure at parity for up to the 24h interval.
    // * the property-estimate poll, because the wipe clears `valuations` while the restored
    //   accounts keep their `house_pricer` subscriptions — so a subscribed property would show
    //   whatever estimate the snapshot happened to carry, for up to a *month*.
    let rate_poll = sure_app::tasks::exchange_rates::TASK_NAME;
    let estimate_poll = sure_app::tasks::property_estimates::TASK_NAME;
    sqlx::query!(
        "DELETE FROM scheduled_task_runs WHERE task_name IN (?1, ?2)",
        rate_poll,
        estimate_poll
    )
    .execute(&mut *txn)
    .await?;

    for c in &snap.currencies {
        sqlx::query!(
            "INSERT INTO currencies
                (code, name, symbol, decimal_places, created_at)
             VALUES (?1,?2,?3,?4,?5)",
            c.code,
            c.name,
            c.symbol,
            c.decimal_places,
            c.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    sqlx::query!(
        "UPDATE settings SET base_currency_code=?1,
            updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id=1",
        snap.base_currency_code
    )
    .execute(&mut *txn)
    .await?;

    for c in &snap.categories {
        sqlx::query!(
            "INSERT INTO categories
                (id, name, parent_id, kind, color, icon, sort_order, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            c.id,
            c.name,
            c.parent_id,
            c.kind,
            c.color,
            c.icon,
            c.sort_order,
            c.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for m in &snap.merchants {
        sqlx::query!(
            "INSERT INTO merchants
                (id, name, category_id, note, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            m.id,
            m.name,
            m.category_id,
            m.note,
            m.created_at,
            m.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for p in &snap.people {
        sqlx::query!(
            "INSERT INTO people
                (id, name, color, sort_order, placeholder, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            p.id,
            p.name,
            p.color,
            p.sort_order,
            p.placeholder,
            p.created_at,
            p.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    // A snapshot taken before accounts had owners restores accounts that name nobody. Rather
    // than refuse the import (the backup is still perfectly good data) or invent an owner,
    // do exactly what the household-required migration does: stand a placeholder person up
    // and hand it the orphans, so the invariant holds and the question stays visible.
    let placeholder_id = if snap.accounts.iter().any(|a| a.needs_placeholder_owner()) {
        Some(
            sqlx::query_scalar!(
                r#"INSERT INTO people (name, sort_order, placeholder)
                   VALUES ('Unassigned', 0, 1)
                   RETURNING id AS "id!""#
            )
            .fetch_one(&mut *txn)
            .await?,
        )
    } else {
        None
    };
    for a in &snap.accounts {
        let (ownership, person_id) = a.ownership_columns(placeholder_id);
        sqlx::query!(
            "INSERT INTO accounts
                (id, name, kind, currency_code, institution, metadata, archived,
                 excluded_from_net_worth, sort_order,
                 secured_by_account_id, ownership, person_id, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            a.id,
            a.name,
            a.kind,
            a.currency_code,
            a.institution,
            a.metadata,
            a.archived,
            a.excluded_from_net_worth,
            a.sort_order,
            a.secured_by_account_id,
            ownership,
            person_id,
            a.created_at,
            a.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for t in &snap.transactions {
        sqlx::query!(
            "INSERT INTO transactions
                (id, account_id, posted_at, amount_minor, currency_code, description, merchant,
                 notes, category_id, is_one_off, linked_transaction_id, provider, external_id,
                 categorized_by_rule_id, merchant_id, ownership, person_id, created_at,
                 updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
            t.id,
            t.account_id,
            t.posted_at,
            t.amount_minor,
            t.currency_code,
            t.description,
            t.merchant,
            t.notes,
            t.category_id,
            t.is_one_off,
            t.linked_transaction_id,
            t.provider,
            t.external_id,
            t.categorized_by_rule_id,
            t.merchant_id,
            t.ownership,
            t.person_id,
            t.created_at,
            t.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for v in &snap.valuations {
        sqlx::query!(
            "INSERT INTO valuations
                (id, account_id, as_of, value_minor, currency_code, source, note, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            v.id,
            v.account_id,
            v.as_of,
            v.value_minor,
            v.currency_code,
            v.source,
            v.note,
            v.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for r in &snap.rules {
        sqlx::query!(
            "INSERT INTO rules
                (id, name, description, expression, set_category_id, set_one_off,
                 overwrite_manual, stop_on_match, priority, enabled, set_merchant_id, created_at,
                 updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            r.id,
            r.name,
            r.description,
            r.expression,
            r.set_category_id,
            r.set_one_off,
            r.overwrite_manual,
            r.stop_on_match,
            r.priority,
            r.enabled,
            r.set_merchant_id,
            r.created_at,
            r.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for c in &snap.crons {
        sqlx::query!(
            "INSERT INTO crons
                (id, name, account_id, kind, rate_bps, amount_minor, category_id, frequency,
                 day_of_month, start_date, last_run_on, enabled, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            c.id,
            c.name,
            c.account_id,
            c.kind,
            c.rate_bps,
            c.amount_minor,
            c.category_id,
            c.frequency,
            c.day_of_month,
            c.start_date,
            c.last_run_on,
            c.enabled,
            c.created_at,
            c.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for p in &snap.providers {
        sqlx::query!(
            "INSERT INTO providers
                (id, name, kind, account_id, config, enabled, last_synced_at, created_at,
                 updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            p.id,
            p.name,
            p.kind,
            p.account_id,
            p.config,
            p.enabled,
            p.last_synced_at,
            p.created_at,
            p.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for g in &snap.equity_grants {
        sqlx::query!(
            "INSERT INTO equity_grants
                (id, account_id, company, grant_date, quantity, strike_minor, currency_code,
                 vest_months, cliff_months, unit_value_minor, note, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            g.id,
            g.account_id,
            g.company,
            g.grant_date,
            g.quantity,
            g.strike_minor,
            g.currency_code,
            g.vest_months,
            g.cliff_months,
            g.unit_value_minor,
            g.note,
            g.created_at,
            g.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for e in &snap.equity_exercises {
        sqlx::query!(
            "INSERT INTO equity_exercises
                (id, grant_id, exercise_date, quantity, price_minor, note, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            e.id,
            e.grant_id,
            e.exercise_date,
            e.quantity,
            e.price_minor,
            e.note,
            e.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for h in &snap.holdings {
        sqlx::query!(
            "INSERT INTO holdings
                (id, account_id, ticker, exchange, name, currency_code, trade_date, quantity,
                 unit_price, fee_minor, kind, external_id, provider, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            h.id,
            h.account_id,
            h.ticker,
            h.exchange,
            h.name,
            h.currency_code,
            h.trade_date,
            h.quantity,
            h.unit_price,
            h.fee_minor,
            h.kind,
            h.external_id,
            h.provider,
            h.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for d in &snap.dividends {
        sqlx::query!(
            "INSERT INTO dividends
                (id, account_id, ticker, exchange, record_date, paid_date, shares_held,
                 gross_amount_minor, net_amount_minor, currency_code, external_id, provider,
                 created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            d.id,
            d.account_id,
            d.ticker,
            d.exchange,
            d.record_date,
            d.paid_date,
            d.shares_held,
            d.gross_amount_minor,
            d.net_amount_minor,
            d.currency_code,
            d.external_id,
            d.provider,
            d.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for w in &snap.dividend_withholdings {
        sqlx::query!(
            "INSERT INTO dividend_withholdings
                (id, dividend_id, owed_to, tax_amount_minor, tax_credit_minor, currency_code)
             VALUES (?1,?2,?3,?4,?5,?6)",
            w.id,
            w.dividend_id,
            w.owed_to,
            w.tax_amount_minor,
            w.tax_credit_minor,
            w.currency_code
        )
        .execute(&mut *txn)
        .await?;
    }
    for s in &snap.income_streams {
        sqlx::query!(
            "INSERT INTO income_streams
                (id, person_id, label, employer, currency_code, annual_amount_minor, basis,
                 pay_frequency, first_payment_on, starts_on, ends_on, annual_increase_bps,
                 kiwisaver_bps, student_loan, take_home_bps, linked_category_id, enabled,
                 sort_order, notes, created_at, updated_at, employer_kiwisaver_bps,
                 kiwisaver_account_id, student_loan_account_id)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,
             ?22,?23,?24)",
            s.id,
            s.person_id,
            s.label,
            s.employer,
            s.currency_code,
            s.annual_amount_minor,
            s.basis,
            s.pay_frequency,
            s.first_payment_on,
            s.starts_on,
            s.ends_on,
            s.annual_increase_bps,
            s.kiwisaver_bps,
            s.student_loan,
            s.take_home_bps,
            s.linked_category_id,
            s.enabled,
            s.sort_order,
            s.notes,
            s.created_at,
            s.updated_at,
            s.employer_kiwisaver_bps,
            s.kiwisaver_account_id,
            s.student_loan_account_id
        )
        .execute(&mut *txn)
        .await?;
    }
    for s in &snap.income_stream_steps {
        sqlx::query!(
            "INSERT INTO income_stream_steps
                (id, income_stream_id, effective_on, annual_amount_minor, label, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            s.id,
            s.income_stream_id,
            s.effective_on,
            s.annual_amount_minor,
            s.label,
            s.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for f in &snap.forecast_assumptions {
        sqlx::query!(
            "INSERT INTO forecast_assumptions
                (id, target_type, target_id, annual_growth_bps, annual_volatility_bps,
                 dividend_yield_bps, long_run_growth_bps, notes, created_at, updated_at,
                 annual_fee_bps, annual_fixed_fee_minor)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            f.id,
            f.target_type,
            f.target_id,
            f.annual_growth_bps,
            f.annual_volatility_bps,
            f.dividend_yield_bps,
            f.long_run_growth_bps,
            f.notes,
            f.created_at,
            f.updated_at,
            f.annual_fee_bps,
            f.annual_fixed_fee_minor
        )
        .execute(&mut *txn)
        .await?;
    }
    for e in &snap.forecast_events {
        sqlx::query!(
            "INSERT INTO forecast_events
                (id, label, kind, person_id, expected_on, timing_spread_months, probability_bps,
                 notes, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            e.id,
            e.label,
            e.kind,
            e.person_id,
            e.expected_on,
            e.timing_spread_months,
            e.probability_bps,
            e.notes,
            e.created_at,
            e.updated_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for f in &snap.forecast_event_effects {
        sqlx::query!(
            "INSERT INTO forecast_event_effects
                (id, event_id, kind, sort_order, income_stream_id, person_id, category_id,
                 account_id, amount_minor, rate_bps, delay_months, ramp_months, duration_months,
                 created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            f.id,
            f.event_id,
            f.kind,
            f.sort_order,
            f.income_stream_id,
            f.person_id,
            f.category_id,
            f.account_id,
            f.amount_minor,
            f.rate_bps,
            f.delay_months,
            f.ramp_months,
            f.duration_months,
            f.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for r in &snap.forecast_event_relations {
        sqlx::query!(
            "INSERT INTO forecast_event_relations
                (id, event_id, depends_on_event_id, kind, min_gap_months, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            r.id,
            r.event_id,
            r.depends_on_event_id,
            r.kind,
            r.min_gap_months,
            r.created_at
        )
        .execute(&mut *txn)
        .await?;
    }
    for r in &snap.exchange_rates {
        sqlx::query!(
            "INSERT INTO exchange_rates (base_code, quote_code, as_of, rate) VALUES (?1,?2,?3,?4)",
            r.base_code,
            r.quote_code,
            r.as_of,
            r.rate
        )
        .execute(&mut *txn)
        .await?;
    }

    txn.commit().await?;

    Ok(json!({
        "ok": true,
        "counts": {
            "currencies": snap.currencies.len(),
            "categories": snap.categories.len(),
            "merchants": snap.merchants.len(),
            "people": snap.people.len(),
            "accounts": snap.accounts.len(),
            "transactions": snap.transactions.len(),
            "valuations": snap.valuations.len(),
            "rules": snap.rules.len(),
            "crons": snap.crons.len(),
            "providers": snap.providers.len(),
            "equity_grants": snap.equity_grants.len(),
            "equity_exercises": snap.equity_exercises.len(),
            "holdings": snap.holdings.len(),
            "dividends": snap.dividends.len(),
            "dividend_withholdings": snap.dividend_withholdings.len(),
            "forecast_assumptions": snap.forecast_assumptions.len(),
            "income_streams": snap.income_streams.len(),
            "income_stream_steps": snap.income_stream_steps.len(),
            "forecast_events": snap.forecast_events.len(),
            "forecast_event_effects": snap.forecast_event_effects.len(),
            "forecast_event_relations": snap.forecast_event_relations.len(),
        }
    }))
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    async fn empty_db() -> Db {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::migrate(&db).await.unwrap();
        db
    }

    /// A database with one of most things — the *shape* a real backup has (a person owning an
    /// account, transactions, a valuation, a category, a rate), never anybody's real data.
    ///
    /// Every id here is the one the database hands back rather than a literal. That is not tidiness:
    /// these rows used to claim `id = 1`, which was free until a migration started seeding default
    /// categories and then collided with the first of them. A fixture that picks its own ids is
    /// asserting something about the rest of the schema that it has no way to keep true.
    async fn populated_db() -> Db {
        let db = empty_db().await;
        let person = sqlx::query_scalar!(
            r#"INSERT INTO people (name, sort_order) VALUES ('A', 0) RETURNING id AS "id!""#
        )
        .fetch_one(&db)
        .await
        .unwrap();
        let category = sqlx::query_scalar!(
            r#"INSERT INTO categories (name, kind, sort_order)
               VALUES ('Groceries (fixture)', 'expense', 0)
               RETURNING id AS "id!""#
        )
        .fetch_one(&db)
        .await
        .unwrap();
        let account = sqlx::query_scalar!(
            r#"INSERT INTO accounts (name, kind, currency_code, metadata, ownership, person_id)
               VALUES ('Everyday', 'bank', 'NZD', '{}', 'person', ?1)
               RETURNING id AS "id!""#,
            person
        )
        .fetch_one(&db)
        .await
        .unwrap();
        // Two forecast events, so the round trip covers `forecast_events` and both its child
        // tables. Without any event at all, every export read an empty table and never decoded a
        // row — which is how the export came to reference four columns migration 0022 had
        // removed and still passed its tests. Written through `create_event` rather than raw
        // SQL so the effect and the relation are built the way the app builds them.
        let child = crate::forecast::create_event(
            &db,
            sure_core::SaveForecastEvent {
                label: "Child".into(),
                kind: sure_core::LifeEventKind::Child,
                person_id: Some(person),
                expected_on: sure_core::IsoDate::parse("2027-04-01").unwrap(),
                timing_spread_months: 6,
                probability_bps: 5000,
                notes: Some("daycare from six months".into()),
                effects: vec![sure_core::LifeEffectSpec::RecurringDelta {
                    category_id: category,
                    amount_minor: -450_00,
                    delay_months: 6,
                    ramp_months: 3,
                    duration_months: Some(60),
                }],
                relations: Vec::new(),
            },
        )
        .await
        .unwrap();
        crate::forecast::create_event(
            &db,
            sure_core::SaveForecastEvent {
                label: "Somewhere bigger".into(),
                kind: sure_core::LifeEventKind::Custom,
                person_id: None,
                expected_on: sure_core::IsoDate::parse("2028-01-01").unwrap(),
                timing_spread_months: 0,
                probability_bps: 10_000,
                notes: None,
                effects: Vec::new(),
                // Only on the paths where the child happens — the relation kind whose loss
                // changes what the projection means rather than merely reordering it.
                relations: vec![sure_core::SaveForecastEventRelation {
                    depends_on_event_id: child.id,
                    kind: sure_core::RelationKind::OnlyIf,
                    min_gap_months: 0,
                }],
            },
        )
        .await
        .unwrap();
        for (posted_at, amount) in [("2026-01-05", 5_000_00i64), ("2026-01-20", -1_200_00)] {
            sqlx::query!(
                "INSERT INTO transactions (account_id, posted_at, amount_minor, currency_code,
                                           description, category_id)
                 VALUES (?1, ?2, ?3, 'NZD', 'x', ?4)",
                account,
                posted_at,
                amount,
                category
            )
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query!(
            "INSERT INTO valuations (account_id, as_of, value_minor, currency_code)
             VALUES (?1, '2026-02-01', 3_800_00, 'NZD')",
            account
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query!(
            "INSERT INTO exchange_rates (base_code, quote_code, as_of, rate)
             VALUES ('NZD', 'USD', '2026-02-01', '0.6')"
        )
        .execute(&db)
        .await
        .unwrap();
        db
    }

    /// The format is a user-facing backup contract, so [`export_bytes`] — which writes each
    /// table straight out and drops it, instead of building a `serde_json::Value` copy of the
    /// whole database — must produce exactly what serialising the [`Snapshot`] struct produced.
    /// This is the test that keeps the two from drifting apart.
    #[tokio::test]
    async fn export_bytes_matches_the_snapshot_struct() {
        let db = populated_db().await;

        let streamed: Value = serde_json::from_slice(&export_bytes(&db).await.unwrap()).unwrap();
        let structured = serde_json::to_value(export(&db).await.unwrap()).unwrap();

        assert_eq!(streamed, structured);
        // And it really did carry the data, rather than agreeing on an empty object.
        assert_eq!(streamed["transactions"].as_array().unwrap().len(), 2);
        assert_eq!(streamed["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(streamed["version"], SNAPSHOT_VERSION);
    }

    /// The round trip an actual backup is: export the bytes, import them into a fresh database,
    /// export that one, and get the same blob. Nothing here depends on how the bytes were built
    /// — which is the point, since that is what changed.
    #[tokio::test]
    async fn a_streamed_export_still_imports() {
        let source = populated_db().await;
        let bytes = export_bytes(&source).await.unwrap();

        let restored = empty_db().await;
        let snap: Snapshot = serde_json::from_slice(&bytes).unwrap();
        let summary = import(&restored, snap).await.unwrap();
        assert_eq!(summary["counts"]["transactions"], 2);
        assert_eq!(summary["counts"]["accounts"], 1);
        // The table whose export was broken for its whole life, and the two child tables that
        // were never in the snapshot at all: restored, not silently dropped.
        assert_eq!(summary["counts"]["forecast_events"], 2);
        assert_eq!(summary["counts"]["forecast_event_effects"], 1);
        assert_eq!(summary["counts"]["forecast_event_relations"], 1);

        let round_tripped = export_bytes(&restored).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&round_tripped).unwrap(),
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            "a snapshot must restore to a database that exports the same snapshot"
        );
    }

    /// The regression test for the gap this snapshot had until the tables above were added: an
    /// event used to come back from a restore as a bare row, because `forecast_event_effects` and
    /// `forecast_event_relations` were in neither half of the format.
    ///
    /// It asserts through `forecast::list_events` — the read path the projection itself uses —
    /// rather than on row counts, because counts were never the problem. `import` reported the
    /// events restored and they were; what was missing is the only part of an event the
    /// simulation actually reads. `forecast_events.kind` is presentation and a form template
    /// (see `0022_forecast_events_unified.sql`), so an event whose effects did not survive is a
    /// row the UI draws and every projection ignores — a restore that looks clean and is not.
    ///
    /// Restoring *into a populated database* rather than an empty one is deliberate: that is the
    /// path where the wipe has to clear existing events whose relations point at each other
    /// before the incoming ones land.
    #[tokio::test]
    async fn a_restored_forecast_event_keeps_the_effects_the_simulation_reads() {
        let source = populated_db().await;
        let before = crate::forecast::list_events(&source).await.unwrap();

        let bytes = export_bytes(&source).await.unwrap();
        let restored = populated_db().await;
        import(&restored, serde_json::from_slice(&bytes).unwrap())
            .await
            .unwrap();

        let after = crate::forecast::list_events(&restored).await.unwrap();
        assert_eq!(after.len(), before.len(), "one event per event, not more");

        let named = |set: &[sure_core::ForecastEvent], label: &str| {
            set.iter()
                .find(|e| e.label == label)
                .unwrap_or_else(|| panic!("{label} came back"))
                .clone()
        };
        let (was, child) = (named(&before, "Child"), named(&after, "Child"));
        assert_eq!(
            child.effects.len(),
            1,
            "the effect is what the projection reads — an event without it is inert"
        );
        // Against what the source actually held rather than a restated literal: the category id
        // is whatever the fixture was handed, and an assertion that rebuilds the expected value
        // out of the actual one proves nothing.
        assert_eq!(
            child.effects[0].spec, was.effects[0].spec,
            "and it came back with the same numbers, not merely present"
        );

        let bigger = after
            .iter()
            .find(|e| e.label == "Somewhere bigger")
            .expect("the second event came back");
        assert_eq!(bigger.relations.len(), 1, "its dependency survived");
        assert_eq!(bigger.relations[0].kind, sure_core::RelationKind::OnlyIf);
        assert_eq!(
            bigger.relations[0].depends_on_event_id, child.id,
            "and still points at the event it is conditional on"
        );
    }

    /// An empty database still exports every key, so a client (and the importer) can rely on the
    /// shape rather than on which tables happened to have rows.
    #[tokio::test]
    async fn an_empty_database_exports_the_whole_shape() {
        let db = empty_db().await;
        let streamed: Value = serde_json::from_slice(&export_bytes(&db).await.unwrap()).unwrap();
        let structured = serde_json::to_value(export(&db).await.unwrap()).unwrap();
        assert_eq!(streamed, structured);
        assert!(streamed["holdings"].as_array().unwrap().is_empty());
        // Deserialising it back is what `POST /api/config/import` does first.
        serde_json::from_value::<Snapshot>(streamed).expect("an empty export is importable");
    }
}
