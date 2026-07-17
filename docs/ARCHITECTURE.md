# Architecture

Sure is a flat Cargo + pnpm workspace. The backend is split into layered crates with a
strict dependency direction; the frontend is a single Svelte SPA that talks to the
backend only through a generated, type-safe client.

## Crate map

```
sure-core  ──►  (nothing)          domain vocabulary: AppError, AccountKind/Class
sure-scheduler ─► (nothing)        generic recurring-task scheduler: ScheduledTask +
                                     TaskStateStore ports, storage-agnostic
sure-dal   ──►  core, scheduler    SQLite pool + migrations + every SQL query (per-entity
                                     repository modules: accounts, transactions, rules, …)
                                     + the scheduler's SQLite-backed TaskStateStore
sure-providers ─► sure-core        TransactionProvider + ExchangeRateProvider traits,
                                     registry, CSV importer, Frankfurter FX client
sure-api   ──►  core, dal,         Axum HTTP layer: handlers, DTOs, OpenAPI, and the pure
                providers,           compute engines (rule eval, report aggregation). No SQL.
                scheduler            Also wires up the Scheduler with the app's background
                                     tasks (e.g. the daily exchange-rate poll) at startup.
@sure/client ◄── (OpenAPI spec)    generated openapi-typescript + openapi-fetch client
@sure/web  ──►  @sure/client       Svelte SPA
@sure/api-tests ─► @sure/client    TS+Playwright e2e: spawns the sure-api binary, drives it
                                     through the client (validates client + API together)
```

Arrows are "depends on". Nothing depends on `sure-api` except the tests, and nothing
below `sure-api` knows about HTTP — the web framework only enters at the top.

### `sure-core` — the shared vocabulary
Pure domain types every layer speaks: the workspace `AppError`/`AppResult`, the JSON
error envelope, and `AccountKind`/`AccountClass`. **No web framework and no persistence
library by default.** Its two cross-cutting integrations are each behind a Cargo feature,
so a layer only pays for what it uses:

```toml
# sure-core
[features]
sqlx = ["dep:sqlx"]                  # `AppError: From<sqlx::Error>` + `AccountKind: sqlx::Type`
axum = ["dep:axum", "dep:tracing"]   # `impl IntoResponse for AppError`
# sure-dal enables `sqlx`; sure-api enables `axum`; providers enables neither.
```

`sqlx` is gated because only the DAL touches the database: the feature supplies the
`From<sqlx::Error>` conversion that lets DAL functions use `?`, and the `sqlx::Type`
mapping so enum columns bind/decode directly. Built on its own, `sure-core` (and
`sure-providers`) pull in neither sqlx nor axum. (Because the API binary depends on the
DAL, Cargo's feature unification still links sqlx into that binary — but the API crate
itself neither declares nor names it; persistence stays the DAL's job.)

### `sure-scheduler` — generic recurring-task scheduling
A small, storage-agnostic crate for background jobs that need to survive a restart
without redoing work ahead of schedule — the motivating case is the exchange-rate poll,
but it's written to take on more scheduled tasks later without each one reinventing this.
Two traits define the seam:

```rust
#[async_trait]
pub trait ScheduledTask: Send + Sync {
    fn name(&self) -> &'static str;      // key in the state store, e.g. "exchange_rate_poll"
    fn interval(&self) -> Duration;      // how often it needs to run
    async fn run(&self) -> anyhow::Result<()>;
}

#[async_trait]
pub trait TaskStateStore: Send + Sync {
    async fn last_run_at(&self, task_name: &str) -> anyhow::Result<Option<DateTime<Utc>>>;
    async fn record_run(&self, task_name: &str, at: DateTime<Utc>) -> anyhow::Result<()>;
}
```

`Scheduler` wakes up every `check_interval` (independent of any task's own interval — it
just controls how promptly a newly-due task is noticed), asks the store whether each
registered task is due, and runs it. Only a *successful* run is recorded, so a failure is
retried on the next check rather than waiting out the full interval. `sure-dal` is the
only implementor of `TaskStateStore` today (`SqliteTaskStateStore`, backed by the
`scheduled_task_runs` table) — deliberately separate from `crons`/`cron_runs`, which is a
user-facing recurring-*adjustment* ledger (appreciation, interest, …), not a background-job
scheduler.

### `sure-dal` — the data-access boundary
Owns everything SQLite-specific: pool creation and pragmas (WAL, foreign keys), the
embedded migration set (`packages/dal/migrations`), `MIGRATOR`, and — the point of the
crate — **every SQL query in the app**. Exposes a `Db` type alias (today just
`SqlitePool`) that higher layers pass around. A `build.rs` emits
`rerun-if-changed=migrations` so a newly-added migration always forces a recompile (the
classic `sqlx::migrate!` staleness trap).

Queries live in per-entity repository modules (`sure_dal::{accounts, transactions,
categories, merchants, rules, reports, crons, equity, providers, snapshot, valuations,
currencies, settings, exchange_rate_cache, scheduled_tasks}`). Each module owns its
row/request/response types (they derive
`FromRow`) and its functions, which take `&Db` and return `AppResult<T>` — so no `sqlx`
type ever crosses the crate boundary. Conventions are uniform: `list → Vec<T>`,
`create → T`, `get`/`update → T` (`NotFound` if absent), `delete → ()`; foreign-key and
unique-constraint failures are mapped to `AppError::validation`/`Conflict` here rather
than leaking a database error upward. Whole multi-statement operations that must be
atomic (a rules run and its audit trail, a config import, the crons engine) run inside a
transaction owned by the DAL; the API crate re-exports these types
(`pub use sure_dal::rules::*`) so the OpenAPI paths and handler signatures are unchanged.

### `sure-providers` — the integration interface
The generic extension point:

```rust
#[async_trait]
pub trait TransactionProvider: Send + Sync {
    fn kind(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn accepts_payload(&self) -> bool { false }
    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>>;
}
```

A `Registry` holds the available implementations; the sync route handles persistence,
dedupe (on `(provider, external_id)`), and audit generically. The bundled `CsvProvider`
is a credential-free reference implementation. To add a bank/broker integration you
implement the trait, add it to `Registry::new()`, and touch nothing else.

The crate also carries a second, unrelated port — `ExchangeRateProvider` — for pulling
currency exchange rates from an upstream source (`fetch_rates(base) -> Vec<ExchangeRateQuote>`).
It doesn't share a `Registry`: today there's exactly one implementation
(`FrankfurterProvider`, a free/keyless ECB-rates client), instantiated directly and run
as a `sure-scheduler` task from `sure-api`.

### `sure-api` — the application
The HTTP surface (Axum handlers + request/response DTOs), the OpenAPI document, and the
pure compute that sits between the two: the Zen-expression rule evaluator and the report
aggregation (running balances, currency normalisation, category roll-ups, flow graphs).
These engines hold **no SQL** — they call `sure_dal` loaders for rows and hand decided
changes back to the DAL to persist. `sqlx` is not a dependency of this crate. It
re-exports the lower crates under their historical module paths, so handler code reads
unchanged after the split:

```rust
pub use sure_core::error;             // crate::error::AppError, ...
pub use sure_dal as db;               // crate::db::{connect, migrate, Db}
pub use sure_providers as providers;  // crate::providers::{Registry, TransactionProvider, ...}
```

## Why this split, and how it grows

The goal was smaller, testable units with a one-way dependency graph:

- **The error type is the seam between layers.** Every crate returns `AppResult<T>`; only
  `sure-api` (with `sure-core/axum`) knows how that becomes an HTTP status, and only
  `sure-dal` (with `sure-core/sqlx`) knows how a `sqlx::Error` becomes an `AppError`.
  Each direction is a feature on the shared error type, so web and persistence stay out of
  the layers that don't need them.
- **SQL lives in exactly one crate.** `sure-api` names no `sqlx` type and issues no query;
  it asks `sure-dal` for rows and hands it changes to persist. This keeps persistence
  swappable behind the `Db` alias and makes the compute independently testable, and it is
  what the e2e suite exercises end-to-end through the generated client.
- **Interfaces are traits where polymorphism is real** (`TransactionProvider` — there will
  be many implementations) and **plain functions where it isn't**. The DAL exposes
  repository functions rather than a `Store` trait, because there is exactly one backing
  store; introducing a trait there would be ceremony without payoff. If a second backend
  ever appeared, `Db` is the type to make generic.

**How compute and persistence divide.** Where a feature is part pure logic and part SQL,
the logic stays in `sure-api` and the SQL moves to `sure-dal`. Rules are the clearest
case: the DAL loads the evaluation contexts and, given a list of decided changes, writes
the transaction updates and audit rows in one transaction; the API crate owns the
`zen-expression` loop that turns contexts into those changes. Reports follow the same
shape — DAL loaders return rows, the API crate crunches them. Operations that are *all*
SQL and must be atomic (the crons engine, config import/export, run undo) live wholly in
the DAL.

## Type safety across the boundary

`cargo run --bin gen-openapi` serialises the `utoipa` document to
`packages/client/openapi.json`; `openapi-typescript` turns it into `paths`/`components`
types; the SPA calls the API through `openapi-fetch`. A backend change that alters a
request or response therefore surfaces as a **TypeScript compile error** in the web app.
(`packages/client/strip-operation-ids.mjs` drops duplicate `operationId`s so the generator
keys types by path — see its comment for the why.)
