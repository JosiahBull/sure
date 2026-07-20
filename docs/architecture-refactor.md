# Architecture refactor: toward a hexagonal core

A staged plan to move Sure from its current layered split to a **hexagonal (ports &
adapters)** architecture, so business logic is isolated from both the web framework and
the database and becomes trivial to modify, test, and re-back.

Read [`ARCHITECTURE.md`](./ARCHITECTURE.md) first — it describes the codebase as it
stands today. This document describes where we're taking it and how, in three phases that
each ship independently.

> **This plan consciously revises one decision recorded in `ARCHITECTURE.md`.** That
> document argues the DAL should expose *plain repository functions* rather than a `Store`
> trait, "because there is exactly one backing store; introducing a trait there would be
> ceremony without payoff." Phase 3 reverses that. The payoff we're now buying is not a
> second database — it's **isolating and unit-testing the logic-heavy services** (rules,
> brokerage, reports) against in-memory fakes, and being able to change a table's shape
> without touching the domain type or any handler. When these phases land,
> `ARCHITECTURE.md`'s "traits where polymorphism is real, functions where it isn't"
> section must be updated to match.

## Status

| Phase | State | Landed |
|---|---|---|
| **1 — Extract `sure-app`** | ✅ Done | 2026-07-20, `4ca476c` |
| **2 — Repo ports for logic-heavy services** | ✅ Done | 2026-07-20, `95eccde` |
| **3a — Row/domain split** | ✅ Done | 2026-07-20 |
| **3b/3c/3d — DTO audit, full port coverage, `sure-server` split** | ⏳ Pending | — |

Phases 1–2 shipped close to the plan below, with two notable as-built choices: the
services became **structs** (`BrokerageService`, `ReportService`, `RuleService`,
`SyncService`) constructed from `Arc<dyn Port>` dependencies rather than free functions;
and the ports own **their own plain row types** (`HoldingRow`, `WalletRow`, …) that the
adapter maps into, because `sure-app` can't depend back on `sure-dal`. See the per-phase
"as built" notes. `docs/ARCHITECTURE.md` has been updated to describe the landed state.

---

## Where we are

Five backend crates with a strict one-way dependency graph:

```
sure-core       domain vocabulary: AppError, AccountKind/Class, shared DTO types
sure-scheduler  generic recurring-task scheduler — ScheduledTask + TaskStateStore ports
sure-dal        SQLite pool + migrations + every SQL query (per-entity modules)
sure-providers  TransactionProvider / StockPriceProvider / ExchangeRateProvider + adapters
sure-api        Axum HTTP layer + MOST OF THE BUSINESS LOGIC
```

Two structural problems block "trivial to modify":

1. **The application core lives inside the web crate.** `sure-api` holds not just routing
   but the rule engine (`routes/rules.rs`), report aggregation (`routes/reports.rs`, 800
   LOC), brokerage valuation (`brokerage.rs`), FX (`fx.rs`), the stock-price service
   (`stock_prices.rs`), and the scheduled-task bodies (`exchange_rates.rs`,
   `provider_poll.rs`, `transfer_link.rs`). To change a business rule you edit the HTTP
   crate; to test one you stand up Axum and SQLite.

2. **The core depends on the concrete adapter, not on a port.** Every service calls
   `sure_dal::transactions::list(&db, q)` — free functions on a concrete `SqlitePool`.
   The inside of the hexagon points straight at the SQLite outside, so logic can't be
   exercised without a real database, and the persisted row shape *is* the domain type
   (`sure-core` types derive `sqlx::FromRow` behind a feature).

What's already right, and stays: `sure-scheduler` is a clean port/adapter pair
(`TaskStateStore` defined with the mechanism, implemented by the DAL); `sure-providers`
already defines port traits with swappable adapters and a `Registry`; `AppError`
feature-gates its web and db concerns. These are the templates for everything below.

## Where we're going

```
        DRIVING ADAPTERS              APPLICATION CORE                DRIVEN ADAPTERS
   ┌────────────────────────┐    ┌──────────────────────┐      ┌────────────────────────┐
   │ sure-api               │    │ sure-app             │      │ sure-dal (SQLite)      │
   │  routes, extractors,   │───▶│  use-cases /services │◀─────│  implements repo ports │
   │  OpenAPI, telemetry    │    │  + PORTS (traits):   │      ├────────────────────────┤
   │ scheduler task wiring  │    │   repos, Clock       │◀─────│ sure-providers         │
   │ main.rs = composition  │    │        │             │      │  implement provider    │
   │            root        │    │        ▼             │      │  ports                 │
   └────────────────────────┘    │ sure-core (domain)   │      ├────────────────────────┤
                                  │  pure types + logic  │◀─────│ system Clock, etc.     │
                                  └──────────────────────┘      └────────────────────────┘
```

Target dependency arrows ("depends on"):

```
sure-core       ─► (nothing)                       pure domain types + pure domain logic
sure-app        ─► sure-core, sure-scheduler       use-cases + all ports (traits)
sure-dal        ─► sure-app, sure-core, scheduler  implements repo ports + TaskStateStore
sure-providers  ─► sure-app, sure-core             implements provider ports
sure-api        ─► sure-app, sure-core             thin HTTP; (dal + providers only in main)
```

The one arrow that flips is **`api → dal` becomes `dal → app`**: the adapter now depends
on the core to see the port traits it implements, and the core never names the adapter.
`sure-api`'s binary still depends on `sure-dal`/`sure-providers`, but *only in the
composition root* (`main.rs`/`serve()`) where concrete adapters are constructed and
injected — handlers reference only `sure-app`. `serve()` already does exactly this for the
scheduler; we extend the pattern to everything.

---

## Phase 1 — Extract `sure-app` (highest value, lowest risk)

> **✅ As built (2026-07-20, `4ca476c`).** `packages/app` created (~2,900 LOC) with the
> modules below plus `ports.rs`; `zen-expression` moved to `sure-app`. `sure-api`'s
> `lib.rs` no longer declares the compute modules — they're gone from the web crate. The
> `brokerage.rs` unit tests moved with the code. Everything below is the plan as executed.

Physically move the business logic out of the web crate into a new `sure-app` crate. **No
trait inversion yet** — the services keep calling `sure_dal::*` free functions directly.
This alone splits the big crate and establishes the layer; it's a large but mechanical
move that touches no logic.

### New crate

```
packages/app/
  Cargo.toml        # deps: sure-core, sure-dal, sure-providers, sure-scheduler,
                    #       chrono, rust_decimal, serde_json, async-trait, tracing, anyhow
  src/lib.rs
```

Add `"packages/app"` to the workspace members and `sure-app = { path = "packages/app" }`
to `[workspace.dependencies]`.

### What moves

| From `sure-api` | To `sure-app` | Notes |
|---|---|---|
| `brokerage.rs` | `app/brokerage.rs` | valuation + FX orchestration |
| `fx.rs` | `app/fx.rs` | make `Fx` and its methods `pub` (was `pub(crate)`) |
| `stock_prices.rs` | `app/stock_prices.rs` | `price_at` service + `StockPriceTask` |
| `exchange_rates.rs` | `app/tasks/exchange_rates.rs` | `ExchangeRateTask` |
| `provider_poll.rs` | `app/tasks/provider_poll.rs` | `ProviderPollTask` |
| `transfer_link.rs` | `app/tasks/transfer_link.rs` | `TransferLinkTask` |
| rule engine from `routes/rules.rs` | `app/rules.rs` | the `zen-expression` loop, `Current`, `validate_rule`, apply/preview orchestration; move `zen-expression` dep to `sure-app` |
| aggregation from `routes/reports.rs` | `app/reports.rs` | net-worth / category / flow compute; the `ReportQuery`/response DTOs stay in `sure-api` for now (they're `ToSchema`) |
| provider sync orchestration from `routes/providers.rs` | `app/sync.rs` | `sync_provider` and the fetch→dedupe→persist flow the poll task shares |

### What stays in `sure-api`

`routes/` (now thin), `openapi.rs`, `state.rs`, `config.rs`, `telemetry.rs`, `main.rs`,
`lib.rs`, and `AppError`'s `IntoResponse`. A handler becomes: extract → call one
`sure_app` function → serialize.

```rust
// routes/brokerage.rs — after
pub async fn snapshot(State(st): State<AppState>, Path(id): Path<i64>, Query(q): Query<AsOf>)
    -> AppResult<Json<BrokerageSnapshot>>
{
    let provider = YahooFinanceProvider::new();
    Ok(Json(sure_app::brokerage::snapshot(&st.db, Some(&provider), id, q.as_of()).await?))
}
```

Scheduler wiring in `serve()` changes only its import paths
(`crate::exchange_rates::ExchangeRateTask` → `sure_app::tasks::exchange_rates::ExchangeRateTask`).

### Acceptance

- `sure-api` no longer contains `zen-expression` or report/brokerage math.
- `cargo build -p sure-app` compiles without `axum`.
- `cargo test --workspace` green; the e2e suite (`@sure/api-tests`) unchanged and passing.
- No behaviour change — this is a move, verified by the existing tests (`brokerage.rs`'s
  unit tests move with it).

---

## Phase 2 — Introduce ports where testability pays

> **✅ As built (2026-07-20, `95eccde`).** Ports live in `sure_app::ports`: `Clock` /
> `SystemClock` plus `AccountRepo`, `BrokerageRepo`, `StockPriceCacheRepo`, `ValuationRepo`,
> `FxRatesRepo`, `RuleRepo`, `ReportRepo`, `ProviderRepo`, `TransferRepo`,
> `ExchangeRateRepo`. `sure-dal` now depends on `sure-app` and provides
> `sure_dal::store::SqliteStore`, one struct implementing every port by delegating to the
> existing per-entity modules and mapping row shapes. Services are structs
> (`BrokerageService` etc.) holding `Arc<dyn Port>`s; `AppState` holds `db` + the four
> services + a `stock_prices` port, wired in `AppState::new` (HTTP composition root) and
> again in `serve()` for the scheduler tasks. A `FixedClock` test seam lives in
> `sure-app`. **Divergence from the plan:** the ports own plain row types rather than
> returning `sure-core`/`sure-dal` structs, since `sure-app` can't depend on `sure-dal`
> (the inversion) — this already decouples the *port surface* from the table shape, which
> is a down payment on Phase 3a for those shapes.

Invert the dependency for the logic-heavy services so they can be unit-tested against
in-memory fakes, and abstract the clock. **Scope this deliberately**: add ports for the
services that contain real branching logic (rules, brokerage/FX, reports, sync); leave
thin CRUD routes calling `sure_dal` directly — a `TransactionRepo::list` that a handler
calls once and forwards is ceremony (the exact judgement `ARCHITECTURE.md` already makes).

### Ports live in `sure-app`

```rust
// sure-app/src/ports.rs
use sure_core::{Account, AppResult, StockPrice, Transaction /* … */};

#[async_trait::async_trait]
pub trait AccountRepo: Send + Sync {
    async fn get(&self, id: i64) -> AppResult<Account>;
}

#[async_trait::async_trait]
pub trait BrokerageRepo: Send + Sync {
    async fn positions_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<HoldingRow>>;
    async fn wallet_balances_at(&self, account_id: i64, as_of: &str) -> AppResult<Vec<WalletRow>>;
    async fn account_tickers(&self, account_id: i64) -> AppResult<Vec<(String, String)>>;
    async fn earliest_activity_date(&self, account_id: i64) -> AppResult<Option<String>>;
}

#[async_trait::async_trait]
pub trait StockPriceCacheRepo: Send + Sync {
    async fn get_at(&self, ticker: &str, exchange: &str, as_of: &str) -> AppResult<Option<StockPrice>>;
    async fn upsert(&self, ticker: &str, exchange: &str, as_of: &str, close: &str, ccy: &str) -> AppResult<()>;
}

// Abstracts the wall clock so day-by-day logic (brokerage backfill, scheduler,
// stock-price polling) is deterministic in tests instead of reading Utc::now() directly.
pub trait Clock: Send + Sync {
    fn now(&self) -> chrono::DateTime<chrono::Utc>;
    fn today(&self) -> chrono::NaiveDate { self.now().date_naive() }
}
```

Also introduced here: `ValuationRepo`, `FxRatesRepo` (the `currency_decimals` +
`exchange_rates` loaders `Fx::load` uses), `RuleRepo`, and `ReportRepo`. The
existing provider port traits (`StockPriceProvider`, `TransactionProvider`,
`ExchangeRateProvider`) stay in `sure-providers` and are consumed by `sure-app` as-is
(Phase 3 optionally relocates them for a single source of truth).

### Adapter in `sure-dal`

One struct wrapping the pool implements every repo port by delegating to the existing free
functions — the functions don't change, so the SQL is untouched:

```rust
// sure-dal/src/store.rs
use sure_app::ports::{AccountRepo, StockPriceCacheRepo /* … */};

#[derive(Clone)]
pub struct SqliteStore { pub db: crate::Db }

#[async_trait::async_trait]
impl AccountRepo for SqliteStore {
    async fn get(&self, id: i64) -> AppResult<Account> { crate::accounts::get(&self.db, id).await }
}

#[async_trait::async_trait]
impl StockPriceCacheRepo for SqliteStore {
    async fn get_at(&self, t: &str, e: &str, a: &str) -> AppResult<Option<StockPrice>> {
        crate::stock_prices::get_at(&self.db, t, e, a).await
    }
    async fn upsert(&self, t: &str, e: &str, a: &str, c: &str, ccy: &str) -> AppResult<()> {
        crate::stock_prices::upsert(&self.db, t, e, a, c, ccy).await
    }
}
```

`sure-dal` gains `sure-app` as a dependency (to see the traits). No cycle:
`dal → app → core`, and `dal → core` directly.

### Services depend on ports, not on `Db`

```rust
// sure-app/src/brokerage.rs — after
use std::sync::Arc;

pub struct BrokerageService {
    accounts: Arc<dyn AccountRepo>,
    brokerage: Arc<dyn BrokerageRepo>,
    prices: Arc<dyn StockPriceCacheRepo>,
    valuations: Arc<dyn ValuationRepo>,
    fx: Arc<dyn FxRatesRepo>,
    clock: Arc<dyn Clock>,
}

impl BrokerageService {
    pub async fn snapshot(
        &self,
        price_provider: Option<&dyn StockPriceProvider>,
        account_id: i64,
        as_of: NaiveDate,
    ) -> AppResult<BrokerageSnapshot> {
        let account = self.accounts.get(account_id).await?;   // ← port, fakeable
        // … same logic, `self.prices` / `self.fx` instead of sure_dal::*, self.clock.today()
    }
}
```

Now a unit test constructs `BrokerageService` from hand-written in-memory fakes and a
frozen `Clock` — no SQLite, no Axum. This is the whole point of the exercise.

### Composition root wires concrete adapters

```rust
// sure-api/src/main.rs (serve) / state.rs
let store = Arc::new(sure_dal::SqliteStore { db: pool.clone() });
let clock = Arc::new(sure_app::SystemClock);

let brokerage = Arc::new(BrokerageService::new(
    store.clone(), store.clone(), store.clone(), store.clone(), store.clone(), clock.clone(),
));
let reports = Arc::new(ReportService::new(store.clone(), clock.clone()));
let rules   = Arc::new(RuleService::new(store.clone()));

let state = AppState { db: pool, brokerage, reports, rules /* thin CRUD still uses db */ };
```

`AppState` holds `Arc<Service>`s; handlers call `st.brokerage.snapshot(...)` and never
name `sure_dal`. `SqliteStore` implementing many traits means one `store.clone()` satisfies
every port a service needs.

### Acceptance

- `sure-app`'s logic-heavy modules have unit tests that use in-memory fakes (no `sure-dal`
  in `[dev-dependencies]` for those tests).
- No `Utc::now()` / `chrono::Utc::now()` inside `sure-app` services — all via `Clock`.
- Handlers for rules/brokerage/reports/sync reference only `sure_app` types.
- `cargo test --workspace` + e2e green.

---

## Phase 3 — Purify the domain and complete port coverage

> **3a ✅ done; 3b/3c/3d ⏳ pending.** Starting point after Phase 2: `sure-core` still
> carried the `sqlx` and `axum` features and its shared types still derived
> `sqlx::FromRow` / `sqlx::Type`, so for the *shared vocabulary* (`Transaction`,
> `Account`, …) the domain type was still also the row type and the wire type. The ports'
> own row shapes (`HoldingRow`, …) were already decoupled from Phase 2.

Finish the hexagon: make `sure-core` a *pure* domain crate and extend port coverage to the
remaining aggregates, so persistence and transport shapes are fully decoupled from the
domain model. Order the work by payoff — the row/domain split is the valuable part; the
wire-DTO split is optional and only where shapes actually diverge.

### 3a. Split the persistence row shape from the domain type (valuable)

> **✅ As built (2026-07-20).** Every `sure_core` type that derived `sqlx::FromRow`
> directly (18 structs across 12 files: `HoldingLot`, `Dividend`, `DividendWithholding`,
> `Category`, `Currency`, `Merchant`, `Settings`, `Cron`, `CronRun`, `EquityGrant`,
> `EquityExercise`, `Valuation`, `ProviderSync`, `StockPrice`, `Rule`, `RuleRun`,
> `RuleApplicationDetail`, `Transaction`) got a `*Row` struct + `From`/`TryFrom` impl in
> its `sure-dal` module, following the `AccountRow`/`ProviderRow` pattern that already
> existed. `AccountKind` also lost its `sqlx::Type` derive — it gained a hand-written
> `as_str()`/`FromStr` pair instead, and `AccountRow.kind`/binds go through that; since a
> stored value that fails to parse is now a real (if never-expected) failure mode rather
> than a `sqlx::Type` decode error, `From<AccountRow> for Account` became a fallible
> `TryFrom`. `sure-core`'s `sqlx` feature now gates *only* `AppError`'s
> `From<sqlx::Error>` conversion (not a "domain type" concern) — no struct or enum in
> `sure-core` derives any `sqlx` trait any more, satisfying the acceptance criterion
> literally. `Account` also gained `#[derive(Clone)]` (needed by Phase 2's test fakes;
> every field type already derived `Clone`, so this was a trivial, safe addition — noted
> here since it landed slightly out of order, alongside Phase 2 work).

Today `sure_core::Transaction` derives `sqlx::FromRow`, so the table shape *is* the domain
type — a column rename ripples into every handler. Separate them:

```rust
// sure-core (domain) — pure, no sqlx/serde/utoipa derives
#[derive(Clone, Debug)]
pub struct Transaction { /* domain fields */ }

// sure-dal — the row, mapped to the domain type
#[derive(sqlx::FromRow)]
struct TransactionRow { /* columns */ }
impl From<TransactionRow> for sure_core::Transaction { /* … */ }
```

Now a migration that reshapes a table changes only `TransactionRow` and its `From` impl.
Do this for every aggregate. Drop `sure-core`'s `sqlx` feature entirely once complete.

### 3b. Split the wire DTO from the domain type (only where it pays)

`sure-core` types also derive `serde` + `utoipa::ToSchema` — transport concerns. The
purist move is a DTO twin in `sure-api` with `From<Domain>`:

```rust
// sure-api/src/dto.rs
#[derive(Serialize, ToSchema)]
pub struct TransactionDto { /* … */ }
impl From<sure_core::Transaction> for TransactionDto { /* … */ }
```

**Be selective.** A `serde` derive on a domain type is harmless to modifiability; the cost
of a DTO twin + `From` impl for every type is real. Introduce a DTO **only where the wire
shape genuinely diverges from the domain shape** (e.g. computed/flattened report
responses, or when an API-compat concern pins the JSON while the domain evolves). Where
they're identical, keeping the derives on the domain type is the pragmatic call. Track
which types get a twin so the choice is deliberate, not accidental.

### 3c. Complete the ports; relocate provider ports (optional)

Extend repo-port coverage to the aggregates left calling `sure_dal` directly in Phase 2,
so `sure-api` need not depend on `sure-dal` at all except in the composition root. For a
single source of truth, optionally move the provider port *traits*
(`TransactionProvider`, `StockPriceProvider`, `ExchangeRateProvider`) into
`sure-app::ports`, leaving `sure-providers` as pure adapters implementing them — mirroring
the repo-port arrangement exactly.

### 3d. (Optional) extract the composition root

If we want `sure-api` itself to depend only on `sure-app`, split the binary out: a thin
`sure-server` crate owns `main.rs`/`serve()` and depends on everything to wire it, while
`sure-api` becomes a library of routes/handlers depending only on `sure-app`. This is the
last 5% — worth it only if we start shipping the API as a reusable library.

### Acceptance

- `sure-core` builds with **no** `sqlx`, `axum`, `serde`, or `utoipa` feature required by
  the domain types (serde/utoipa may remain on consciously-chosen shared DTOs).
- Changing a table's columns touches only `sure-dal`; changing the JSON contract touches
  only `sure-api`.
- `sure-api`'s non-`main` code names neither `sqlx` nor `sure_dal` query functions.
- `ARCHITECTURE.md` updated to describe the ports-and-adapters model and to supersede the
  "plain functions, not traits" rationale.

---

## Sequencing, risk, and testing

- **Order:** Phase 1 → 2 → 3, each a separate PR (or a small series). Phase 1 is a pure
  move and should land and bake before 2 begins. Within Phase 2, port one service at a
  time (brokerage, then reports, then rules, then sync) — each is independently
  shippable. Phase 3a (rows) before 3b (DTOs).
- **Safety net:** the `@sure/api-tests` Playwright e2e suite drives the real binary through
  the generated client end-to-end — it's the regression gate for every phase, since none
  of this should change external behaviour. Keep it green at each step; treat a client
  TypeScript compile error (from a changed OpenAPI shape) as a signal a DTO changed
  unintentionally.
- **Merge gates unchanged:** `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets --all-features -D warnings`, `cargo test --workspace`, web/api-tests
  typecheck (see [`CI.md`](./CI.md)). New crate must satisfy all four.
- **Rollback:** Phase 1 is trivially revertible (it's a move). Phases 2–3 are per-service,
  so a problematic service can be reverted to direct `sure_dal` calls without unwinding the
  others.

## Definition of done

The rule engine, report aggregation, and brokerage valuation compile and unit-test with
**no web framework and no database** in scope; a new persistence backend or a table
reshape is a change confined to `sure-dal`; a JSON contract change is confined to
`sure-api`; and `sure-core` is a dependency-free description of the domain. At that point
"trivial to modify" is structural, not aspirational.
