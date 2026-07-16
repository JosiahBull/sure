//! Full config/data snapshot export & import. Export serialises every domain table
//! (ids preserved); import wipes the database and restores in one transaction with
//! `PRAGMA defer_foreign_keys=ON`. Pure audit/run tables are cleared but not restored.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use sure_core::AppResult;

use crate::Db;

pub const SNAPSHOT_VERSION: i64 = 1;

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub version: i64,
    pub base_currency_code: String,
    pub currencies: Vec<CurrencyRow>,
    pub exchange_rates: Vec<ExchangeRateRow>,
    pub categories: Vec<CategoryRow>,
    pub merchants: Vec<MerchantRow>,
    pub accounts: Vec<AccountRow>,
    pub transactions: Vec<TransactionRow>,
    pub valuations: Vec<ValuationRow>,
    pub rules: Vec<RuleRow>,
    pub crons: Vec<CronRow>,
    pub providers: Vec<ProviderRow>,
    pub equity_grants: Vec<GrantRow>,
    pub equity_exercises: Vec<ExerciseRow>,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct CurrencyRow {
    pub code: String,
    pub name: String,
    pub symbol: String,
    pub decimal_places: i64,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, FromRow)]
pub struct ExchangeRateRow {
    pub base_code: String,
    pub quote_code: String,
    pub as_of: String,
    pub rate: String,
}

#[derive(Serialize, Deserialize, FromRow)]
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

#[derive(Serialize, Deserialize, FromRow)]
pub struct MerchantRow {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    pub note: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, FromRow)]
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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, FromRow)]
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
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, FromRow)]
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

#[derive(Serialize, Deserialize, FromRow)]
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

#[derive(Serialize, Deserialize, FromRow)]
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

#[derive(Serialize, Deserialize, FromRow)]
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

#[derive(Serialize, Deserialize, FromRow)]
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

#[derive(Serialize, Deserialize, FromRow)]
pub struct ExerciseRow {
    pub id: i64,
    pub grant_id: i64,
    pub exercise_date: String,
    pub quantity: i64,
    pub price_minor: i64,
    pub note: Option<String>,
    pub created_at: String,
}

pub async fn export(db: &Db) -> AppResult<Snapshot> {
    let base_currency_code =
        sqlx::query_scalar::<_, String>("SELECT base_currency_code FROM settings WHERE id=1")
            .fetch_one(db)
            .await?;

    Ok(Snapshot {
        version: SNAPSHOT_VERSION,
        base_currency_code,
        currencies: sqlx::query_as("SELECT * FROM currencies ORDER BY code").fetch_all(db).await?,
        exchange_rates: sqlx::query_as("SELECT * FROM exchange_rates").fetch_all(db).await?,
        categories: sqlx::query_as("SELECT * FROM categories ORDER BY id").fetch_all(db).await?,
        merchants: sqlx::query_as("SELECT * FROM merchants ORDER BY id").fetch_all(db).await?,
        accounts: sqlx::query_as("SELECT * FROM accounts ORDER BY id").fetch_all(db).await?,
        transactions: sqlx::query_as("SELECT * FROM transactions ORDER BY id").fetch_all(db).await?,
        valuations: sqlx::query_as("SELECT * FROM valuations ORDER BY id").fetch_all(db).await?,
        rules: sqlx::query_as("SELECT * FROM rules ORDER BY id").fetch_all(db).await?,
        crons: sqlx::query_as("SELECT * FROM crons ORDER BY id").fetch_all(db).await?,
        providers: sqlx::query_as("SELECT * FROM providers ORDER BY id").fetch_all(db).await?,
        equity_grants: sqlx::query_as("SELECT * FROM equity_grants ORDER BY id").fetch_all(db).await?,
        equity_exercises: sqlx::query_as("SELECT * FROM equity_exercises ORDER BY id").fetch_all(db).await?,
    })
}

pub async fn import(db: &Db, snap: Snapshot) -> AppResult<Value> {
    let mut txn = db.begin().await?;
    // Defer FK checks so rows can be cleared and re-inserted in any order.
    sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *txn).await?;

    for table in [
        "rule_applications", "rule_runs", "cron_runs", "provider_syncs",
        "equity_exercises", "equity_grants", "valuations", "transactions",
        "providers", "crons", "rules", "merchants", "accounts", "categories",
        "exchange_rates", "currencies",
    ] {
        sqlx::query(&format!("DELETE FROM {table}")).execute(&mut *txn).await?;
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
    for a in &snap.accounts {
        sqlx::query("INSERT INTO accounts (id, name, kind, currency_code, institution, metadata, archived, sort_order, secured_by_account_id, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)")
            .bind(a.id).bind(&a.name).bind(&a.kind).bind(&a.currency_code).bind(&a.institution).bind(&a.metadata).bind(a.archived).bind(a.sort_order).bind(a.secured_by_account_id).bind(&a.created_at).bind(&a.updated_at)
            .execute(&mut *txn).await?;
    }
    for t in &snap.transactions {
        sqlx::query("INSERT INTO transactions (id, account_id, posted_at, amount_minor, currency_code, description, merchant, notes, category_id, is_one_off, linked_transaction_id, provider, external_id, categorized_by_rule_id, merchant_id, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)")
            .bind(t.id).bind(t.account_id).bind(&t.posted_at).bind(t.amount_minor).bind(&t.currency_code).bind(&t.description).bind(&t.merchant).bind(&t.notes).bind(t.category_id).bind(t.is_one_off).bind(t.linked_transaction_id).bind(&t.provider).bind(&t.external_id).bind(t.categorized_by_rule_id).bind(t.merchant_id).bind(&t.created_at).bind(&t.updated_at)
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
    for r in &snap.exchange_rates {
        sqlx::query("INSERT INTO exchange_rates (base_code, quote_code, as_of, rate) VALUES (?1,?2,?3,?4)")
            .bind(&r.base_code).bind(&r.quote_code).bind(&r.as_of).bind(&r.rate)
            .execute(&mut *txn).await?;
    }

    txn.commit().await?;

    Ok(json!({
        "ok": true,
        "counts": {
            "currencies": snap.currencies.len(),
            "categories": snap.categories.len(),
            "merchants": snap.merchants.len(),
            "accounts": snap.accounts.len(),
            "transactions": snap.transactions.len(),
            "valuations": snap.valuations.len(),
            "rules": snap.rules.len(),
            "crons": snap.crons.len(),
            "providers": snap.providers.len(),
            "equity_grants": snap.equity_grants.len(),
            "equity_exercises": snap.equity_exercises.len(),
        }
    }))
}
