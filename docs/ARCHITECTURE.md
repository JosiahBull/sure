# Architecture

Sure is a flat Cargo + pnpm workspace. The backend follows a **ports-and-adapters
(hexagonal)** shape: an application core of use-case services depends only on trait
*ports*, and the web framework and the database are *adapters* wired in at the edges. The
frontend is a single Svelte SPA that talks to the backend only through a generated,
type-safe client.

> The move to this shape was staged; see [`architecture-refactor.md`](./architecture-refactor.md)
> for the plan and its status. All of it has landed: Phases 1–2 (extract `sure-app`,
> introduce repo ports), Phase 3a (purify `sure-core`'s domain types of `sqlx`), Phase 3b
> (a deliberately selective wire-DTO audit — reports keep their existing DTO twins;
> nothing else needed one), Phase 3c (repo-port coverage extended to every aggregate, so
> no handler calls `sure-dal` directly any more), and Phase 3d (the composition root
> extracted into its own `sure-server` crate, so `sure-api` is a pure routes/handlers
> library with no `main` and no dependency on `sure-dal` or `sqlx`).

## Crate map

```
sure-core  ──►  (nothing)          domain vocabulary: AppError, AccountKind/Class, and the
                                     shared request/response types (no persistence deps)
sure-scheduler ─► (nothing)        generic recurring-task scheduler: ScheduledTask +
                                     TaskStateStore ports, storage-agnostic
sure-providers ─► app, sure-core   concrete adapters implementing sure-app's provider
                                     ports: CSV + Akahu (NZ banking) transaction sources,
                                     Yahoo Finance prices, Frankfurter FX, and the upload
                                     parsers (ASB, myIR, Sharesies, CSV) behind
                                     `ImportAdapter`; two registries implement the two
                                     lookup ports (`ProviderRegistry`, `ImportRegistry`)
sure-app   ──►  core, scheduler    the application core: use-case services (brokerage,
                                     reports, rules, sync, stock prices, forecast) + the
                                     background tasks, the compute engines (rule eval,
                                     report aggregation, Monte Carlo projection), and every
                                     PORT the services depend on — repos, Clock, and the
                                     provider ports. No SQL, no HTTP.
sure-dal   ──►  app, core,         SQLite pool + migrations + every SQL query (per-entity
                scheduler            repository modules) + `SqliteStore`, which implements
                                     every one of sure-app's repo ports, and the
                                     scheduler's SQLite-backed TaskStateStore
sure-api   ──►  app, core,         thin Axum HTTP layer: handlers, request/response DTOs,
                providers            OpenAPI. No SQL, no compute, no `main`. Every handler
                                     goes through a `sure_app` service or repo port; the
                                     crate never names `sure_dal` or `sqlx`.
sure-server ──► app, dal,          the composition root: the only crate that names every
                providers,           adapter. Builds the one `SqliteStore` + `SystemClock`,
                scheduler, api       injects them into `sure-app`'s services and `sure-api`'s
                                     `AppState`, registers the scheduler tasks, and owns
                                     `main`/`serve`. Produces the `sure-api` binary.
@sure/client ◄── (OpenAPI spec)    generated openapi-typescript + openapi-fetch client
@sure/web  ──►  @sure/client       Svelte SPA
@sure/api-tests ─► @sure/client    TS+Playwright e2e: spawns the sure-api binary, drives it
                                     through the client (validates client + API together)
```

Arrows are "depends on". The graph runs `core ← app ← {dal, providers}`, with `sure-api`
depending on `sure-app` for every handler and on neither `sure-dal` nor `sure-providers`.
That last part was only true of `sure-dal` until the import unification: `sure-api` named
three parsers directly (`routes::{asb, student_loan, brokerage}`), which is what putting
them behind the `ImportAdapter` port closed — see [IMPORT.md](IMPORT.md).
The key inversion is that **both `sure-dal` and `sure-providers` depend on `sure-app`** —
the adapters depend on the core to see the port traits they implement (`SqliteStore` the
repo ports; the provider clients the `TransactionProvider` / `StockPriceProvider` /
`ExchangeRateProvider` ports; `Registry` the `ProviderRegistry` port), and the core never
names an adapter. The concrete provider adapters (a `Registry`, a `StockPriceProvider`) are
built by the composition root and injected into `AppState`, so a handler selects a provider
through a port rather than naming a concrete one. `sure-server` sits above all of it as the
composition root, the one place every concrete adapter is named and wired together; the web
framework's routing lives in `sure-api`, but the binary — and the only crate that touches
both `sqlx` and a live `TcpListener` — is `sure-server`.

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
`sure-core`, `sure-app`, `sure-providers`, and (since Phase 3d) `sure-api` pull in neither
sqlx nor axum's server bits, with the `sqlx` feature on or off — `cargo check -p sure-core
--all-features` and `--no-default-features` both compile the exact same domain types.
`sure-server`, the composition root, is the only crate that depends on `sure-dal` and so
the only one Cargo's feature unification actually links `sqlx` into — the `sure-api`
binary it produces is built entirely from crates that never declare or name it.

Every `sure-dal` per-entity module maps its own `#[derive(sqlx::FromRow)]` row struct
(`TransactionRow`, `RuleRow`, `CronRow`, …) into the `sure-core` domain type via a `From`
(or, where a column can fail to parse — `AccountRow.kind` — a fallible `TryFrom`) impl;
`AccountKind` binds/reads as plain `TEXT` through a hand-written `as_str()`/`FromStr`
pair rather than `sqlx::Type`. A column rename or type change touches only the row struct
and its conversion, never the domain type or a handler.

> **Phase 3b (wire DTOs) landed as a deliberately selective audit.** `sure-core`'s shared
> types still derive `serde`/`utoipa::ToSchema` and double as the JSON wire shape for
> every aggregate except reports — that's a conscious choice, not an oversight (see the
> refactor doc): a DTO twin is only worth it where the wire shape genuinely diverges from
> the domain shape, which the Phase 3c audit confirmed is true only of reports' response
> types (already split, from Phase 1).

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

### `sure-providers` — the external adapters
Concrete implementations of the provider ports — this crate holds adapters, **not** port
definitions. The ports live in `sure_app::ports` (the hexagon owns its ports); this crate
depends on `sure-app` to see them, so `sure-app` never depends back on it. The
transaction-source port it implements is:

```rust
#[async_trait]
pub trait TransactionProvider: Send + Sync {   // defined in sure_app::ports
    fn kind(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn accepts_payload(&self) -> bool { false }
    fn supports_account_discovery(&self) -> bool { false }
    async fn fetch(&self, ctx: SyncContext<'_>) -> anyhow::Result<Vec<ProviderTransaction>>;
    async fn list_accounts(&self) -> anyhow::Result<Vec<ProviderAccount>> { Err(...) }
    async fn current_balance(&self, ctx: SyncContext<'_>) -> anyhow::Result<Option<ProviderBalance>> { Ok(None) }
}
```

`Registry` (this crate) implements `sure_app::ports::ProviderRegistry`, holding the
available implementations; the composition root builds it and injects it, and `sure-app`'s
`SyncService` handles persistence, dedupe (on `(provider, external_id)`), and audit
generically. The bundled `CsvProvider` is a credential-free reference implementation. To
add a bank/broker integration you implement the trait and add it to `Registry::new()` —
`sure-app` and `sure-api` never change.

Nothing in `sure-providers` reads configuration. An adapter that talks to the network is
constructed with an `Endpoint` (its base URL, already checked to be `https://` or a proxy on
this machine — see `packages/providers/src/http.rs`), which is why `Registry::new` takes the
built `AkahuProvider` rather than building one: only `sure-server` knows where it points and
whether there are credentials. That is also what lets a test aim an adapter at the local
record/replay proxy (`packages/testproxy`) instead of the live API.

`list_accounts` is the account-discovery half: providers whose credentials can surface
many upstream accounts (e.g. `AkahuProvider`, holding the `AKAHU_APP_TOKEN`/`AKAHU_USER_TOKEN`
pair `serve` read for it — or the error saying which one is unset, so an unconfigured install
still boots and fails with a variable name when someone asks for a sync) implement it to
enumerate accounts not yet linked to a local one.
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

This crate implements two more ports (also defined in `sure_app::ports`), consumed by
`sure-app`'s scheduled tasks: `StockPriceProvider` (`fetch_daily_prices` → daily closes,
implemented by `YahooFinanceProvider`, driven by `StockPriceTask` and injected into
`AppState` for the on-demand price lookups the brokerage/stock-price routes make) and
`ExchangeRateProvider` (`fetch_rates(base)` → FX quotes, implemented by the free/keyless
`FrankfurterProvider`, driven by `ExchangeRateTask`). Neither uses the `Registry` — there's
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
- `ForecastService` — resolves each asset/investment/liability account's and each
  top-level income/expense category's growth/volatility/dividend-yield assumption
  (override → an existing enabled cron's rate → derived from history, or a deterministic
  amortisation schedule for a mortgage/loan with complete terms), then runs a Monte Carlo
  projection (thousands of independent monthly paths) into P10/P25/median/mean/P75/P90
  net-worth bands. Every `forecast_events` step-change/one-off applies identically across
  every path — a user-asserted certainty, not something the simulation adds noise to.
  Never writes to the real ledger; unlike `crons`, nothing here is ever applied for real.
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
//    ReportRepo, ProviderRepo, TransferRepo, ExchangeRateRepo, ForecastRepo — and the
//    provider ports (TransactionProvider, StockPriceProvider, ExchangeRateProvider,
//    ProviderRegistry), all implemented by sure-providers.
```

Both `sure-dal` and `sure-providers` implement ports from this crate, so both depend on it
and it depends on neither — one rule for every outbound dependency: the port is defined
here, the adapter lives outside and implements it, and the composition root injects the
concrete one.

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
currencies, settings, exchange_rates, scheduled_tasks, forecast}`). Each module owns its
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

### `sure-api` — the HTTP boundary
A thin driving adapter: the Axum handlers, the request/response DTOs, the OpenAPI
document, and request telemetry. **No SQL, no business logic, no `main`** — every handler
either calls one `sure_app` service or reaches into `AppState`'s repo port directly for a
thin CRUD aggregate; nothing calls `sure_dal`. It re-exports the lower crates it does
depend on under their historical module paths so handler code reads unchanged:

```rust
pub use sure_core::error;             // crate::error::AppError, ...
pub use sure_providers as providers;  // crate::providers::{Registry, TransactionProvider, ...}
```

`AppState` (defined in `state.rs`) is the injection point: every field is either an
`Arc<Service>` (the five logic-heavy services) or `Arc<dyn Port>` (a `sure_app::ports`
trait object for a thin-CRUD aggregate) — both types `sure-app` defines, so the struct
itself never names `sure_dal`. `build_app(state, web_dir, &ApiConfig)` assembles the
router, the OpenAPI JSON endpoint, and the middleware stack around whatever `AppState`
it's handed; it's called both by `sure-server`'s `serve()` and by the e2e harness (with a
fresh `AppState` built the same way production does), so there's no separate "test app"
that could drift from production.

Five sibling modules make up that stack, each a thin layer over the routes:
`cache` (the route→cache-policy/deadline table), `etag` (weak validators and `304`s),
`limits` (per-client rate limiting, the in-flight ceiling, the shared error envelope),
`security` (security headers and the CORS allowlist), and `telemetry` (the request span
and error normalisation). `config` holds their tunables as plain data with `Default` —
**it parses no environment**, because reading the environment is a concern of *running*
the server. See [HTTP.md](HTTP.md) for what each layer does and why it sits where it does.

### `sure-server` — the composition root
A thin crate with exactly one job: own `main`, and be the only place a concrete adapter
is named and wired together. It's the sole dependent of `sure-dal` left in the binary's
own source (as opposed to `sure-api`, which depends on `sure-app` and `sure-providers`
only) — splitting it out of `sure-api` is what makes "`sure-api` depends only on
`sure-app` [+ `sure-providers`, for the couple of routes that need a concrete provider
adapter directly]" literally true, not just true of its business logic.

```rust
// sure-server/src/lib.rs
fn build_state(db: Db) -> sure_api::State {
    let store = Arc::new(SqliteStore::new(db));
    let clock = Arc::new(SystemClock);
    let brokerage = Arc::new(BrokerageService::new(store.clone(), /* …ports… */, clock.clone()));
    // reports, rules, sync built the same way, then handed out as `sure_api::State { .. }`
    // — a plain struct literal, since every `AppState` field is `pub`.
    sure_api::State { brokerage, /* … */ stock_prices: store.clone(), /* … */ providers: store }
}

pub async fn serve(config: Config) -> anyhow::Result<()> {
    let pool = sure_dal::connect(&config.database_url).await?;
    sure_dal::migrate(&pool).await?;
    // …build a second SqliteStore + SystemClock, register the scheduler tasks
    // (skipped entirely when BACKGROUND_TASKS=off, as the e2e suite sets)…
    let app = sure_api::build_app(build_state(pool.clone()), config.web_dir.as_deref(), &config.api);
    http::serve(listener, app, config.http).await?;   // drains before returning
    pool.close().await;
    Ok(())
}
```

`Config` (all environment parsing — `DATABASE_URL`/`BIND_ADDR`/`WEB_DIR`/`BACKGROUND_TASKS`
plus the HTTP tunables it hands to `sure-api` as an `ApiConfig`) lives here too, for the same reason:
it's a concern of *running* the server, not of the routes themselves. The crate's only
binary, `sure-api`, is what `Dockerfile`/`package.json`/CI actually build and run — the
name predates the split and was kept unchanged so nothing downstream (the Docker
`ENTRYPOINT`, `packages/api-tests`' spawned-binary path) needed to change.

The `http` module owns the TCP accept loop instead of calling `axum::serve`, because the
connection-level guards — a slowloris timeout, a connection ceiling, HTTP/2 stream limits,
and a graceful drain — are all settings on hyper's connection builder, which `axum::serve`
constructs internally and never exposes. Draining before `pool.close()` is what stops a
container restart from cutting a SQLite write short. See [HTTP.md](HTTP.md).

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
- **Every aggregate sits behind a port, but for two different reasons.** The logic-heavy
  services (brokerage, reports, rules, sync, forecast) depend on repo-port *traits* and the `Clock`
  port because that's what makes their branching logic unit-testable against in-memory
  fakes — `SqliteStore` is one implementation among what could be several. The thin CRUD
  aggregates (accounts, categories, merchants, currencies, settings, …) also go through a
  port, but `SqliteStore` is their only implementation ever expected to exist; the trait
  there isn't for test substitutability, it's what makes "`sure-api` cannot depend on
  `sure-dal`" a compiler-enforced fact rather than a convention. Once that was the goal
  (Phase 3c), a thin CRUD handler had no other way to reach its data — it can only see
  what `AppState` hands it, and `AppState`'s fields are `sure_app` types. (This supersedes
  two earlier decisions in turn: first that the DAL should expose *only* functions and no
  traits, then — mid-refactor — that a port was only worth it where polymorphism-for-
  testing was real. Both were reasonable calls under a narrower goal than the one that
  ultimately won: an `sure-api` that depends on `sure-dal` in zero call sites, not just in
  the ones that happen to need a test double.)

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
