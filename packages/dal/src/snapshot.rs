//! Full config/data snapshot export & import. Export serialises every domain table
//! (ids preserved); import wipes the database and restores in one transaction with
//! `PRAGMA defer_foreign_keys=ON`. Pure audit/run tables are cleared but not restored.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use sure_core::AppResult;

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
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct CurrencyRow {
    pub code: String,
    pub name: String,
    pub symbol: String,
    pub decimal_places: i64,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ExchangeRateRow {
    pub base_code: String,
    pub quote_code: String,
    pub as_of: String,
    pub rate: String,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct MerchantRow {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub currency_code: String,
    pub institution: Option<String>,
    pub metadata: String,
    pub archived: bool,
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ExerciseRow {
    pub id: i64,
    pub grant_id: i64,
    pub exercise_date: String,
    pub quantity: i64,
    pub price_minor: i64,
    pub note: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct DividendWithholdingRow {
    pub id: i64,
    pub dividend_id: i64,
    pub owed_to: String,
    pub tax_amount_minor: i64,
    pub tax_credit_minor: Option<i64>,
    pub currency_code: String,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ForecastAssumptionRow {
    pub id: i64,
    pub target_type: String,
    pub target_id: i64,
    pub annual_growth_bps: Option<i64>,
    pub annual_volatility_bps: Option<i64>,
    pub dividend_yield_bps: Option<i64>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ForecastEventRow {
    pub id: i64,
    pub target_type: String,
    pub target_id: i64,
    pub kind: String,
    pub effective_date: String,
    pub amount_minor: i64,
    pub label: String,
    pub created_at: String,
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn export(db: &Db) -> AppResult<Snapshot> {
    let base_currency_code =
        sqlx::query_scalar::<_, String>("SELECT base_currency_code FROM settings WHERE id=1")
            .fetch_one(db)
            .await?;

    Ok(Snapshot {
        version: SNAPSHOT_VERSION,
        base_currency_code,
        currencies: sqlx::query_as("SELECT * FROM currencies ORDER BY code")
            .fetch_all(db)
            .await?,
        exchange_rates: sqlx::query_as("SELECT * FROM exchange_rates")
            .fetch_all(db)
            .await?,
        categories: sqlx::query_as("SELECT * FROM categories ORDER BY id")
            .fetch_all(db)
            .await?,
        merchants: sqlx::query_as("SELECT * FROM merchants ORDER BY id")
            .fetch_all(db)
            .await?,
        people: sqlx::query_as("SELECT * FROM people ORDER BY id")
            .fetch_all(db)
            .await?,
        accounts: sqlx::query_as("SELECT * FROM accounts ORDER BY id")
            .fetch_all(db)
            .await?,
        transactions: sqlx::query_as("SELECT * FROM transactions ORDER BY id")
            .fetch_all(db)
            .await?,
        valuations: sqlx::query_as("SELECT * FROM valuations ORDER BY id")
            .fetch_all(db)
            .await?,
        rules: sqlx::query_as("SELECT * FROM rules ORDER BY id")
            .fetch_all(db)
            .await?,
        crons: sqlx::query_as("SELECT * FROM crons ORDER BY id")
            .fetch_all(db)
            .await?,
        providers: sqlx::query_as("SELECT * FROM providers ORDER BY id")
            .fetch_all(db)
            .await?,
        equity_grants: sqlx::query_as("SELECT * FROM equity_grants ORDER BY id")
            .fetch_all(db)
            .await?,
        equity_exercises: sqlx::query_as("SELECT * FROM equity_exercises ORDER BY id")
            .fetch_all(db)
            .await?,
        holdings: sqlx::query_as("SELECT * FROM holdings ORDER BY id")
            .fetch_all(db)
            .await?,
        dividends: sqlx::query_as("SELECT * FROM dividends ORDER BY id")
            .fetch_all(db)
            .await?,
        dividend_withholdings: sqlx::query_as("SELECT * FROM dividend_withholdings ORDER BY id")
            .fetch_all(db)
            .await?,
        forecast_assumptions: sqlx::query_as("SELECT * FROM forecast_assumptions ORDER BY id")
            .fetch_all(db)
            .await?,
        forecast_events: sqlx::query_as("SELECT * FROM forecast_events ORDER BY id")
            .fetch_all(db)
            .await?,
    })
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn import(db: &Db, snap: Snapshot) -> AppResult<Value> {
    let mut txn = db.begin().await?;
    // Defer FK checks so rows can be cleared and re-inserted in any order.
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(&mut *txn)
        .await?;

    for table in [
        "rule_applications",
        "rule_runs",
        "cron_runs",
        "provider_syncs",
        "forecast_events",
        "forecast_assumptions",
        "dividend_withholdings",
        "dividends",
        "holdings",
        "equity_exercises",
        "equity_grants",
        "valuations",
        "transactions",
        "providers",
        "crons",
        "rules",
        "merchants",
        // After `accounts`, which references it.
        "accounts",
        "people",
        "categories",
        "exchange_rates",
        "currencies",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&mut *txn)
            .await?;
    }

    for c in &snap.currencies {
        sqlx::query("INSERT INTO currencies (code, name, symbol, decimal_places, created_at) VALUES (?1,?2,?3,?4,?5)")
            .bind(&c.code).bind(&c.name).bind(&c.symbol).bind(c.decimal_places).bind(&c.created_at)
            .execute(&mut *txn).await?;
    }
    sqlx::query("UPDATE settings SET base_currency_code=?1, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id=1")
        .bind(&snap.base_currency_code).execute(&mut *txn).await?;

    for c in &snap.categories {
        sqlx::query("INSERT INTO categories (id, name, parent_id, kind, color, icon, sort_order, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")
            .bind(c.id).bind(&c.name).bind(c.parent_id).bind(&c.kind).bind(&c.color).bind(&c.icon).bind(c.sort_order).bind(&c.created_at)
            .execute(&mut *txn).await?;
    }
    for m in &snap.merchants {
        sqlx::query("INSERT INTO merchants (id, name, category_id, note, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6)")
            .bind(m.id).bind(&m.name).bind(m.category_id).bind(&m.note).bind(&m.created_at).bind(&m.updated_at)
            .execute(&mut *txn).await?;
    }
    for p in &snap.people {
        sqlx::query("INSERT INTO people (id, name, color, sort_order, placeholder, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .bind(p.id).bind(&p.name).bind(&p.color).bind(p.sort_order).bind(p.placeholder).bind(&p.created_at).bind(&p.updated_at)
            .execute(&mut *txn).await?;
    }
    // A snapshot taken before accounts had owners restores accounts that name nobody. Rather
    // than refuse the import (the backup is still perfectly good data) or invent an owner,
    // do exactly what the household-required migration does: stand a placeholder person up
    // and hand it the orphans, so the invariant holds and the question stays visible.
    let placeholder_id = if snap.accounts.iter().any(|a| a.needs_placeholder_owner()) {
        Some(
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO people (name, sort_order, placeholder) VALUES ('Unassigned', 0, 1)
                 RETURNING id",
            )
            .fetch_one(&mut *txn)
            .await?,
        )
    } else {
        None
    };
    for a in &snap.accounts {
        let (ownership, person_id) = a.ownership_columns(placeholder_id);
        sqlx::query("INSERT INTO accounts (id, name, kind, currency_code, institution, metadata, archived, sort_order, secured_by_account_id, ownership, person_id, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)")
            .bind(a.id).bind(&a.name).bind(&a.kind).bind(&a.currency_code).bind(&a.institution).bind(&a.metadata).bind(a.archived).bind(a.sort_order).bind(a.secured_by_account_id).bind(ownership).bind(person_id).bind(&a.created_at).bind(&a.updated_at)
            .execute(&mut *txn).await?;
    }
    for t in &snap.transactions {
        sqlx::query("INSERT INTO transactions (id, account_id, posted_at, amount_minor, currency_code, description, merchant, notes, category_id, is_one_off, linked_transaction_id, provider, external_id, categorized_by_rule_id, merchant_id, ownership, person_id, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)")
            .bind(t.id).bind(t.account_id).bind(&t.posted_at).bind(t.amount_minor).bind(&t.currency_code).bind(&t.description).bind(&t.merchant).bind(&t.notes).bind(t.category_id).bind(t.is_one_off).bind(t.linked_transaction_id).bind(&t.provider).bind(&t.external_id).bind(t.categorized_by_rule_id).bind(t.merchant_id).bind(&t.ownership).bind(t.person_id).bind(&t.created_at).bind(&t.updated_at)
            .execute(&mut *txn).await?;
    }
    for v in &snap.valuations {
        sqlx::query("INSERT INTO valuations (id, account_id, as_of, value_minor, currency_code, source, note, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")
            .bind(v.id).bind(v.account_id).bind(&v.as_of).bind(v.value_minor).bind(&v.currency_code).bind(&v.source).bind(&v.note).bind(&v.created_at)
            .execute(&mut *txn).await?;
    }
    for r in &snap.rules {
        sqlx::query("INSERT INTO rules (id, name, description, expression, set_category_id, set_one_off, overwrite_manual, stop_on_match, priority, enabled, set_merchant_id, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)")
            .bind(r.id).bind(&r.name).bind(&r.description).bind(&r.expression).bind(r.set_category_id).bind(r.set_one_off).bind(r.overwrite_manual).bind(r.stop_on_match).bind(r.priority).bind(r.enabled).bind(r.set_merchant_id).bind(&r.created_at).bind(&r.updated_at)
            .execute(&mut *txn).await?;
    }
    for c in &snap.crons {
        sqlx::query("INSERT INTO crons (id, name, account_id, kind, rate_bps, amount_minor, category_id, frequency, day_of_month, start_date, last_run_on, enabled, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)")
            .bind(c.id).bind(&c.name).bind(c.account_id).bind(&c.kind).bind(c.rate_bps).bind(c.amount_minor).bind(c.category_id).bind(&c.frequency).bind(c.day_of_month).bind(&c.start_date).bind(&c.last_run_on).bind(c.enabled).bind(&c.created_at).bind(&c.updated_at)
            .execute(&mut *txn).await?;
    }
    for p in &snap.providers {
        sqlx::query("INSERT INTO providers (id, name, kind, account_id, config, enabled, last_synced_at, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)")
            .bind(p.id).bind(&p.name).bind(&p.kind).bind(p.account_id).bind(&p.config).bind(p.enabled).bind(&p.last_synced_at).bind(&p.created_at).bind(&p.updated_at)
            .execute(&mut *txn).await?;
    }
    for g in &snap.equity_grants {
        sqlx::query("INSERT INTO equity_grants (id, account_id, company, grant_date, quantity, strike_minor, currency_code, vest_months, cliff_months, unit_value_minor, note, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)")
            .bind(g.id).bind(g.account_id).bind(&g.company).bind(&g.grant_date).bind(g.quantity).bind(g.strike_minor).bind(&g.currency_code).bind(g.vest_months).bind(g.cliff_months).bind(g.unit_value_minor).bind(&g.note).bind(&g.created_at).bind(&g.updated_at)
            .execute(&mut *txn).await?;
    }
    for e in &snap.equity_exercises {
        sqlx::query("INSERT INTO equity_exercises (id, grant_id, exercise_date, quantity, price_minor, note, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .bind(e.id).bind(e.grant_id).bind(&e.exercise_date).bind(e.quantity).bind(e.price_minor).bind(&e.note).bind(&e.created_at)
            .execute(&mut *txn).await?;
    }
    for h in &snap.holdings {
        sqlx::query("INSERT INTO holdings (id, account_id, ticker, exchange, name, currency_code, trade_date, quantity, unit_price, fee_minor, kind, external_id, provider, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)")
            .bind(h.id).bind(h.account_id).bind(&h.ticker).bind(&h.exchange).bind(&h.name).bind(&h.currency_code).bind(&h.trade_date).bind(h.quantity).bind(h.unit_price).bind(h.fee_minor).bind(&h.kind).bind(&h.external_id).bind(&h.provider).bind(&h.created_at)
            .execute(&mut *txn).await?;
    }
    for d in &snap.dividends {
        sqlx::query("INSERT INTO dividends (id, account_id, ticker, exchange, record_date, paid_date, shares_held, gross_amount_minor, net_amount_minor, currency_code, external_id, provider, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)")
            .bind(d.id).bind(d.account_id).bind(&d.ticker).bind(&d.exchange).bind(&d.record_date).bind(&d.paid_date).bind(d.shares_held).bind(d.gross_amount_minor).bind(d.net_amount_minor).bind(&d.currency_code).bind(&d.external_id).bind(&d.provider).bind(&d.created_at)
            .execute(&mut *txn).await?;
    }
    for w in &snap.dividend_withholdings {
        sqlx::query("INSERT INTO dividend_withholdings (id, dividend_id, owed_to, tax_amount_minor, tax_credit_minor, currency_code) VALUES (?1,?2,?3,?4,?5,?6)")
            .bind(w.id).bind(w.dividend_id).bind(&w.owed_to).bind(w.tax_amount_minor).bind(w.tax_credit_minor).bind(&w.currency_code)
            .execute(&mut *txn).await?;
    }
    for f in &snap.forecast_assumptions {
        sqlx::query("INSERT INTO forecast_assumptions (id, target_type, target_id, annual_growth_bps, annual_volatility_bps, dividend_yield_bps, notes, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)")
            .bind(f.id).bind(&f.target_type).bind(f.target_id).bind(f.annual_growth_bps).bind(f.annual_volatility_bps).bind(f.dividend_yield_bps).bind(&f.notes).bind(&f.created_at).bind(&f.updated_at)
            .execute(&mut *txn).await?;
    }
    for e in &snap.forecast_events {
        sqlx::query("INSERT INTO forecast_events (id, target_type, target_id, kind, effective_date, amount_minor, label, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)")
            .bind(e.id).bind(&e.target_type).bind(e.target_id).bind(&e.kind).bind(&e.effective_date).bind(e.amount_minor).bind(&e.label).bind(&e.created_at)
            .execute(&mut *txn).await?;
    }
    for r in &snap.exchange_rates {
        sqlx::query(
            "INSERT INTO exchange_rates (base_code, quote_code, as_of, rate) VALUES (?1,?2,?3,?4)",
        )
        .bind(&r.base_code)
        .bind(&r.quote_code)
        .bind(&r.as_of)
        .bind(&r.rate)
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
            "forecast_events": snap.forecast_events.len(),
        }
    }))
}
