# Architecture

Sure is a flat Cargo + pnpm workspace. The backend follows a **ports-and-adapters
(hexagonal)** shape: an application core of use-case services depends only on trait
*ports*, and the web framework and the database are *adapters* wired in at the edges. The
frontend is a single Svelte SPA that talks to the backend only through a generated,
type-safe client.

> The move to this shape was staged; see [`architecture-refactor.md`](./architecture-refactor.md)
> for the plan and its status. Phases 1–2 (extract `sure-app`, introduce repo ports) and
> Phase 3a (purify `sure-core`'s domain types of `sqlx`) have landed; `sure-core`'s shared
> types still double as wire DTOs (Phase 3b is deliberately selective about splitting
> those), and some thin-CRUD aggregates still go straight from `sure-api` to `sure-dal`
> (Phase 3c, optional).

## Crate map

```
sure-core  ──►  (nothing)          domain vocabulary: AppError, AccountKind/Class, and the
                                     shared request/response types (no persistence deps)
sure-scheduler ─► (nothing)        generic recurring-task scheduler: ScheduledTask +
                                     TaskStateStore ports, storage-agnostic
sure-providers ─► sure-core        TransactionProvider / StockPriceProvider /
                                     ExchangeRateProvider traits, registry, CSV importer,
                                     Akahu (NZ banking), Yahoo Finance, Frankfurter clients
sure-app   ──►  core, providers,   the application core: use-case services (brokerage,
                scheduler            reports, rules, sync, stock prices) + the background
                                     tasks, the compute engines (rule eval, report
                                     aggregation), and the repository PORTS + Clock the
                                     services depend on. No SQL, no HTTP.
sure-dal   ──►  app, core,         SQLite pool + migrations + every SQL query (per-entity
                scheduler            repository modules) + `SqliteStore`, which implements
                                     sure-app's repo ports, and the scheduler's
                                     SQLite-backed TaskStateStore
sure-api   ──►  app, core          thin Axum HTTP layer: handlers, request/response DTOs,
              (+ dal, providers      OpenAPI. No SQL, no compute. `main`/`serve` is the
               in the binary)        composition root: it builds `SqliteStore`, injects it
                                     into the services, and registers the scheduler tasks
@sure/client ◄── (OpenAPI spec)    generated openapi-typescript + openapi-fetch client
@sure/web  ──►  @sure/client       Svelte SPA
@sure/api-tests ─► @sure/client    TS+Playwright e2e: spawns the sure-api binary, drives it
                                     through the client (validates client + API together)
```

Arrows are "depends on". The graph runs `core ← providers ← app ← dal`, with `sure-api`
depending on `sure-app` for the services it calls and on `sure-dal`/`sure-providers` only
in its composition root. The key inversion is **`sure-dal` depends on `sure-app`** — the
adapter depends on the core to see the port traits it implements, and the core never names
the adapter. So the web framework enters only at the top (`sure-api`) and SQL lives only at
the bottom (`sure-dal`); the application core in the middle knows about neither.

### `sure-core` — the shared vocabulary
Pure domain types every layer speaks: the workspace `AppError`/`AppResult`, the JSON
error envelope, and `AccountKind`/`AccountClass`. **No web framework and no persistence
library, ever, for the domain types themselves.** `sure-core` still has an `sqlx` Cargo
feature, but as of Phase 3a it gates *only* `AppError`'s own `From<sqlx::Error>`
conversion — no domain struct or enum derives any `sqlx` trait, with or without the
feature on:

```toml
# sure-core
[features]
sqlx = ["dep:sqlx"]                  # `AppError: From<sqlx::Error>` only
axum = ["dep:axum", "dep:tracing"]   # `impl IntoResponse for AppError`
# sure-dal enables `sqlx`; sure-api enables `axum`; app and providers enable neither.
```

`sqlx` is gated because only the DAL touches the database: the feature supplies the
`From<sqlx::Error>` conversion that lets DAL functions use `?`. Built on their own,
`sure-core`, `sure-app`, and `sure-providers` pull in neither sqlx nor axum, with the
`sqlx` feature on or off — `cargo check -p sure-core --all-features` and `--no-default-features`
both compile the exact same domain types. (Because the API binary depends on the DAL,
Cargo's feature unification still links sqlx into that binary — but the API crate itself
neither declares nor names it; persistence stays the DAL's job.)

Every `sure-dal` per-entity module maps its own `#[derive(sqlx::FromRow)]` row struct
(`TransactionRow`, `RuleRow`, `CronRow`, …) into the `sure-core` domain type via a `From`
(or, where a column can fail to parse — `AccountRow.kind` — a fallible `TryFrom`) impl;
`AccountKind` binds/reads as plain `TEXT` through a hand-written `as_str()`/`FromStr`
pair rather than `sqlx::Type`. A column rename or type change touches only the row struct
and its conversion, never the domain type or a handler.

> **Phase 3b (wire DTOs) is still pending, deliberately.** `sure-core`'s shared types
> still derive `serde`/`utoipa::ToSchema` and double as the JSON wire shape. That's a
> conscious, selective choice (see the refactor doc) — a DTO twin is only worth it where
> the wire shape genuinely diverges from the domain shape (as reports' response types
> already do, from Phase 1).

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
retried on the next check rather than waiting out the full interval. The concrete tasks
now live in `sure-app` (`sure_app::tasks::*`, `sure_app::stock_prices::StockPriceTask`);
`sure-dal` is the only implementor of `TaskStateStore` (`SqliteTaskStateStore`, backed by
the `scheduled_task_runs` table) — deliberately separate from `crons`/`cron_runs`, which
is a user-facing recurring-*adjustment* ledger (appreciation, interest, …), not a
background-job scheduler. This crate is the template the rest of the architecture now
follows: a port defined with the mechanism, an adapter implementing it elsewhere.

### `sure-providers` — the integration interface
The generic extension point for external data sources:

```rust
#[async_trait]
pub trait TransactionProvider: Send + Sync {
    fn kind(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn accepts_payload(&self) -> bool { false }
    fn supports_account_discovery(&self) -> bool { false }
    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>>;
    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> { Err(...) }
    async fn current_balance(&self, ctx: SyncContext<'_>) -> anyhow::Result<Option<ProviderBalance>> { Ok(None) }
}
```

A `Registry` holds the available implementations; `sure-app`'s sync service handles
persistence, dedupe (on `(provider, external_id)`), and audit generically. The bundled
`CsvProvider` is a credential-free reference implementation. To add a bank/broker
integration you implement the trait, add it to `Registry::new()`, and touch nothing else.

`list_accounts` is the account-discovery half: providers whose credentials can surface
many upstream accounts (e.g. `AkahuProvider`, reading `AKAHU_APP_TOKEN`/`AKAHU_USER_TOKEN`
from the environment) implement it to enumerate accounts not yet linked to a local one.
`POST /api/providers/link` then creates (or attaches to) a local account and the
`providers` row in one step, storing the upstream identifier in `config`
(`{"external_account_id": "..."}`) — no schema change needed, since `providers.config` is
already a free-form JSON column. `sure-app`'s `ProviderPollTask` auto-syncs every enabled
provider whose kind doesn't `accepts_payload()` (i.e. needs no human-supplied data) on a
fixed interval, sharing the same fetch/dedupe/audit path (`SyncService`) as the manual
sync route — new discovery-and-poll-capable provider kinds get both for free. Linking also
triggers one immediate best-effort sync, so a freshly-linked account isn't sitting empty
until the next scheduled poll.

`current_balance` addresses a gap transactions alone can't: a provider's transaction
history often doesn't reach back to when the account was opened (a mortgage's full term,
say), so summing only the available transactions drifts from the real balance. The sync
service upserts a same-day, `source = 'provider'` valuation from whatever this returns
after every successful sync — `account_value_at` (the balance/net-worth logic in
`sure_app::reports`) already prefers the latest valuation over summed transactions for any
account kind, so this anchors the displayed balance to the upstream's live figure going
forward, while transactions remain the detailed ledger for the rules engine and history.
A partial unique index (`valuations(account_id, as_of) WHERE source = 'provider'`, added
in `0010_provider_valuations.sql`) makes repeated same-day syncs update that day's snapshot
in place rather than accumulating rows; manual/cron valuations are unaffected. The same
balance fetch also carries an optional credit `limit_minor` (Akahu's `balance.limit`),
patched into a `credit_card`/`revolving_credit` account's `DepositoryMeta.credit_limit_minor`
(`accounts::set_credit_limit`, a no-op for any other kind) — the web UI computes "remaining
borrowing" from that plus the current balance.

`ProviderTransaction.category` (a `ProviderCategory { name, group }`) is the other piece
of enrichment a source can carry — Akahu's NZFCC classification, for instance.
`import_transactions` (`sure-dal`) resolves it via `categories::find_or_create` /
`merchants::find_or_create` (a group becomes the parent of its named category; a newly-
seen merchant is seeded with that category as its default) so imported transactions are
categorized instead of landing uncategorized, without duplicating a category/merchant
already reused by name across a sync's many rows. An already-known merchant's own default
category is never overwritten by a later import.

The crate carries two more ports alongside `TransactionProvider`, both consumed by
`sure-app`'s scheduled tasks: `StockPriceProvider` (`fetch_daily_prices` → daily closes,
implemented by `YahooFinanceProvider`, driven by `StockPriceTask`) and
`ExchangeRateProvider` (`fetch_rates(base)` → FX quotes, implemented by the free/keyless
`FrankfurterProvider`, driven by `ExchangeRateTask`). Neither shares a `Registry` — there's
exactly one implementation of each, instantiated directly in the composition root.

### `sure-app` — the application core
The hexagon's interior: the use-case services that hold **all the business logic**, the
compute engines, the background tasks, and — the point of the crate — the **ports** those
services depend on. No `sqlx`, no `axum`, no SQL, no HTTP.

Services are structs constructed from their dependencies as trait objects, so each can be
unit-tested against in-memory fakes:

- `BrokerageService` — price each open position from the historical-price cache, convert
  via FX, total into the account currency, and backfill a daily valuation series.
- `ReportService` — running balances, currency normalisation, category roll-ups, flow
  graphs (the ~800-line aggregation that was previously in the web crate).
- `RuleService` — the `zen-expression` evaluation loop that turns transaction contexts
  into decided category/merchant/one-off changes.
- `SyncService` — the fetch → dedupe → persist → audit → revalue flow shared by the manual
  sync route and `ProviderPollTask`.
- `StockPriceTask` and `sure_app::tasks::{exchange_rates, provider_poll, transfer_link}` —
  the `ScheduledTask` implementations.

The ports live in `sure_app::ports`:

```rust
// The wall clock, abstracted so day-by-day logic is deterministic in tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    fn today(&self) -> NaiveDate { self.now().date_naive() }
}

#[async_trait]
pub trait AccountRepo: Send + Sync {
    async fn get(&self, id: i64) -> AppResult<Account>;
}
// …plus BrokerageRepo, StockPriceCacheRepo, ValuationRepo, FxRatesRepo, RuleRepo,
//    ReportRepo, ProviderRepo, TransferRepo, ExchangeRateRepo.
```

Because `sure-dal` must depend on `sure-app` to implement these traits, `sure-app` cannot
depend back on `sure-dal` (Cargo forbids the cycle). So every row shape a port returns is
a **plain type owned by `sure_app::ports`** (`HoldingRow`, `WalletRow`, `TxCtx`, …), not
one of `sure-dal`'s internal `FromRow` structs — the adapter maps between the two. Where a
shape is already shared domain vocabulary (`Account`, `Valuation`, `Provider`, …) the
ports reuse `sure_core` directly. This already decouples the *port surface* from the table
shape; Phase 3 extends the same decoupling to the remaining `sure-core` types.

`SystemClock` is the real clock (used by the composition root); a `FixedClock` test seam
lets a service's unit tests freeze "today".

### `sure-dal` — the data-access adapter
Owns everything SQLite-specific: pool creation and pragmas (WAL, foreign keys), the
embedded migration set (`packages/dal/migrations`), `MIGRATOR`, and — the point of the
crate — **every SQL query in the app**. Exposes a `Db` type alias (today just
`SqlitePool`) that the composition root passes around. A `build.rs` emits
`rerun-if-changed=migrations` so a newly-added migration always forces a recompile (the
classic `sqlx::migrate!` staleness trap).

Queries live in per-entity repository modules (`sure_dal::{accounts, transactions,
categories, merchants, rules, reports, crons, equity, providers, snapshot, valuations,
currencies, settings, exchange_rate_cache, scheduled_tasks}`). Each module owns its
row/request/response types (they derive `FromRow`) and its functions, which take `&Db` and
return `AppResult<T>` — so no `sqlx` type ever crosses the crate boundary. Conventions are
uniform: `list → Vec<T>`, `create → T`, `get`/`update → T` (`NotFound` if absent),
`delete → ()`; foreign-key and unique-constraint failures are mapped to
`AppError::validation`/`Conflict` here rather than leaking a database error upward. Whole
multi-statement operations that must be atomic (a rules run and its audit trail, a config
import, the crons engine) run inside a transaction owned by the DAL.

The adapter to the application core is `sure_dal::store::SqliteStore` — **one struct
wrapping the pool that implements every `sure_app::ports` repo trait** by delegating to the
per-entity functions above and mapping each row into the port's plain type. Because one
struct implements every port, the composition root can hand out `store.clone()` for each
of a service's dependencies. `SqliteStore` is also where the row-shape → port-shape mapping
lives, keeping the SQL untouched by the port introduction.

### `sure-api` — the HTTP boundary and composition root
A thin driving adapter: the Axum handlers, the request/response DTOs, the OpenAPI
document, request telemetry, and the wiring that assembles everything at startup. **No SQL
and no business logic** — a handler extracts, calls one `sure_app` service (or, for the
thin CRUD routes, a `sure_dal` function directly), and serialises the result. It re-exports
the lower crates under their historical module paths so older handler code reads unchanged:

```rust
pub use sure_core::error;             // crate::error::AppError, ...
pub use sure_dal as db;               // crate::db::{connect, migrate, Db}
pub use sure_providers as providers;  // crate::providers::{Registry, TransactionProvider, ...}
```

`AppState` is the injection point: it holds the `Db` handle (for the thin CRUD routes) plus
the `Arc`-wrapped services. `AppState::new` and `serve()` are the **composition roots** —
they build a `SqliteStore` and a `SystemClock`, inject them into the services and the
scheduler tasks, and register the tasks with the `Scheduler`. This is the only place
concrete adapters are named:

```rust
let store = Arc::new(SqliteStore::new(db.clone()));
let clock = Arc::new(SystemClock);
let brokerage = Arc::new(BrokerageService::new(store.clone(), /* …ports… */, clock.clone()));
// reports, rules, sync built the same way; scheduler tasks likewise in `serve()`.
```

The e2e harness calls `build_app` with a fresh `AppState`, so it drives the real
composition over HTTP — there is no separate "test app" that could drift from production.

## Why this shape, and how it grows

The goal is smaller, independently-testable units with a one-way dependency graph and the
volatile technologies (web framework, database) pinned to the edges:

- **The error type is the seam between layers.** Every crate returns `AppResult<T>`; only
  `sure-api` (with `sure-core/axum`) knows how that becomes an HTTP status, and only
  `sure-dal` (with `sure-core/sqlx`) knows how a `sqlx::Error` becomes an `AppError`. Each
  direction is a feature on the shared error type, so web and persistence stay out of the
  layers that don't need them.
- **SQL lives in exactly one crate.** `sure-app` and `sure-api` name no `sqlx` type and
  issue no query; they ask a repo port for data and hand it changes to persist. The
  concrete store sits behind `SqliteStore`, so a different backend is a new adapter, not a
  rewrite of the services.
- **Ports where the seam earns its keep; functions where it doesn't.** The logic-heavy
  services (brokerage, reports, rules, sync) depend on repo-port *traits* and the `Clock`
  port, because that's what makes their branching logic unit-testable against fakes and
  keeps them innocent of SQL. The thin CRUD routes (accounts, categories, merchants,
  currencies, settings, …) still call `sure_dal` functions directly through the `Db`
  handle — wrapping a one-line list/create/delete in a trait a handler calls once would be
  ceremony without payoff. (This supersedes an earlier decision, recorded before the
  refactor, that the DAL should expose *only* functions and no traits at all: the trait now
  exists exactly where polymorphism-for-testing is real.)

**How compute and persistence divide.** A feature that is part pure logic and part SQL
splits across `sure-app` and `sure-dal`: the logic (and the ports it needs) lives in
`sure-app`, the SQL in `sure-dal` behind `SqliteStore`. Rules are the clearest case: the
DAL loads evaluation contexts and, given a list of decided changes, writes the transaction
updates and audit rows in one transaction; `RuleService` owns the `zen-expression` loop
that turns contexts into those changes. Reports follow the same shape — DAL loaders return
rows, `ReportService` crunches them. Operations that are *all* SQL and must be atomic (the
crons engine, config import/export, run undo) live wholly in the DAL and are exposed to the
services as a single port method.

## Type safety across the boundary

`cargo run --bin gen-openapi` serialises the `utoipa` document to
`packages/client/openapi.json`; `openapi-typescript` turns it into `paths`/`components`
types; the SPA calls the API through `openapi-fetch`. A backend change that alters a
request or response therefore surfaces as a **TypeScript compile error** in the web app.
(`packages/client/strip-operation-ids.mjs` drops duplicate `operationId`s so the generator
keys types by path — see its comment for the why.)
