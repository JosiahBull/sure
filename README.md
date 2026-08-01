# Sure

A fast, local, single-family financial tracker. Rust + Axum + SQLite backend, a tiny
Svelte SPA you can install to your iPhone home screen, and a fully type-safe client
generated from the backend's OpenAPI spec.

Designed to run behind a firewall on your own hardware — **no logins, no cloud, no
multi-tenant scale**. Heavy work (aggregation, currency normalisation, rules, vesting
math) happens on the backend so the frontend stays a ~34 KB gzipped bundle that flies
on old devices.

| Overview (net worth, spend, money-flow) | Accounts + share vesting | Property paid-off % |
| --- | --- | --- |
| ![Overview](docs/screenshots/overview.png) | ![Vesting](docs/screenshots/equity.png) | ![Property](docs/screenshots/property.png) |

## Features

**Money model**
- Transactions with categories, **merchants**, notes, a one-off flag, and transfer linking.
- Nested categories (income / expense / transfer) with a real tree.
- First-class custom **merchants** (payees) with an optional default category; assignable
  inline or automatically via rules.
- Accounts of every kind: bank, savings, credit card, revolving credit, mortgage,
  student loan, vehicle, real estate, shares (NZ / US / **private with vesting**).
- Multi-currency, normalised into a configurable base currency for reports.
- Point-in-time valuations for assets/liabilities → net-worth history.
- **Property equity**: link a home's mortgage, revolving credit, and other loans (e.g. a
  green-loan program) as its secured debt, and see total debt, equity, and **paid-off %**.

**Automation**
- **Rules** with a nested-logic [Zen expression](https://gorules.io) engine
  (`is_expense and contains(lower(description), 'countdown')`). A rule can set a category,
  a **merchant**, and/or the one-off flag. Preview before saving, run / re-run, and
  **undo** any run — every change is recorded in an audit log.
- **Config backup**: export the whole configuration + data as a JSON snapshot and
  re-import it (ids preserved, destructive) — for rapid iteration while developing.
- **Scheduled adjustments** ("crons"): e.g. *the house appreciates 1%/yr, applied
  monthly*, or a recurring subscription. Idempotent, and each applied period is undoable.
- **Equity vesting**: multiple grants across multiple companies, cliff + linear-monthly
  vesting, exercises, and intrinsic value that rolls into net worth.
- **Provider trait**: a generic Rust interface to pull transactions from external
  sources, with a credential-free CSV importer as the reference implementation
  (dedupes on re-sync). Providers that expose credentialed APIs can also discover
  upstream accounts and link them to a new or existing local account — see the Akahu
  (NZ open banking) implementation, which additionally auto-syncs on a schedule.
- **Bulk file imports** for sources with no usable API: a Sharesies export zip
  (holdings, dividends, wallet ledger) and myIR student-loan exports (reconciling
  several overlapping download windows into one ledger). Both are idempotent, so
  re-uploading costs nothing — see [docs/STUDENT-LOAN.md](docs/STUDENT-LOAN.md).
- **Balance-only accounts get a ledger anyway**: where an upstream reports a balance
  but no transactions (an IR student loan), a daily task differences the balance
  series into transactions, so week-to-week movement is visible.

**Reports & UI**
- Net-worth line over time, income/expense donut per category, and a **Sankey**
  money-flow diagram — all computed server-side.
- Global time-range filter (last month / 90 days / YTD / 12 months / all time) and a
  one-off toggle.
- Installable PWA (iPhone "Add to Home Screen").

## Architecture

Flat pnpm + Cargo workspace under `packages/`. The backend follows a ports-and-adapters
(hexagonal) shape — an application core depending only on trait ports, with the web
framework and the database wired in as adapters at the edges — see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the reasoning and the interface choices,
and [docs/HTTP.md](docs/HTTP.md) for what the HTTP boundary does around them (caching,
compression, HTTP/2, rate limiting). On Linux the server also sandboxes itself with
Landlock before it does any work — see [docs/SANDBOX.md](docs/SANDBOX.md).

```
packages/
  core/         Rust: domain types + AppError, no persistence/web deps (sure-core).
  scheduler/    Rust: generic recurring-task scheduler, storage-agnostic (sure-scheduler).
  dal/          Rust: SQLite pool, migrations, every SQL query (sure-dal).
  providers/    Rust: TransactionProvider trait + registry + CSV/Akahu/Yahoo/FX clients (sure-providers).
  app/          Rust: the application core — use-case services + repo ports, no SQL/HTTP (sure-app).
  api/          Rust: Axum routes/handlers + OpenAPI, depends only on sure-app; `gen-openapi` bin (sure-api).
  server/       Rust: the composition root — wires sure-dal/sure-providers into sure-app/sure-api; owns `main` (sure-server).
  api-tests/    TypeScript: Playwright e2e — spawns the real binary per test, driven through @sure/client.
  client/       Generated TypeScript client (openapi-typescript + openapi-fetch).
  web/          Svelte 5 SPA (Vite, vite-plugin-pwa) + Playwright tests.
scripts/        seed.mjs (demo data), generate-icons.mjs (PWA icons).
```

**Type safety end to end.** `gen-openapi` dumps the OpenAPI spec to JSON; a codegen
step turns it into typed `paths`/`components`, and the SPA calls the API through
`openapi-fetch` — so a backend change that breaks a request surfaces as a TypeScript
error. (See `packages/client/strip-operation-ids.mjs` for why the spec is post-processed.)

**Key decisions**
- Money is stored as signed integer **minor units** (cents); decimals (fx rates, share
  prices) as text, parsed to `Decimal` in Rust. `STRICT` SQLite tables.
- Runtime sqlx queries (not the compile-time macros) — no database needed to build.
- Svelte over React to hit the bundle-size / old-hardware target. Charts are hand-rolled
  SVG (pie, line) fed by backend data; only `d3-sankey` is pulled in for the flow layout.

## Prerequisites

- Rust (nightly is used here; stable ≥ 1.85 with edition 2021 also works)
- Node ≥ 22 and `pnpm` (`corepack enable`)

## Quick start

```bash
pnpm install
pnpm gen:client          # build the OpenAPI spec + generate the typed client
pnpm dev                 # api on :8080, web dev server on :5173 (proxies /api)
```

Vite doesn't start until the backend answers `/api/health`, so on a cold build the web
pane sits quiet while cargo works rather than serving a SPA whose every request dies in
the proxy with `ECONNREFUSED`. `pnpm dev:web` on its own skips the wait, for when a
backend is already running.

The backend reloads on save, like the SPA does: `pnpm dev:api` watches every crate for
changes to `.rs`, `Cargo.toml`, `Cargo.lock`, a migration, or `.env`, then rebuilds and
restarts the API ([`scripts/dev-api.mjs`](scripts/dev-api.mjs)). It builds *before* it
stops the old process and keeps that process running if the build fails, so a compile
error leaves the last working backend serving the SPA instead of dropping it — fix the
error and the next save picks up where it left off. `pnpm dev:api:once` is the plain
`cargo run`, for when you want a single build and no watcher.

Open http://localhost:5173. To load demo data into the dev database:

```bash
pnpm seed                # posts realistic data to http://127.0.0.1:8080
```

### Configuration

Everything has a working default, so none of this is needed to start. For what you do set,
copy [`.env.example`](.env.example) to `.env` (gitignored) — the backend loads it on
startup, searching the working directory and its parents, so `pnpm dev`, `cargo run`, and
the release binary all find the repo-root file wherever they're run from. Real environment
variables win over the file; `SURE_ENV_FILE` points at a different path, or set it empty to
load nothing (the test suites do this, so a developer's tokens can't reach them). The vars
themselves are listed under [Production](#production-single-binary) and in
[docs/HTTP.md](docs/HTTP.md).

### Finding blocking code

Everything the backend does on a request runs on a tokio worker thread, and a poll that
takes milliseconds instead of microseconds has stalled every other request queued behind
it. The usual culprits are ordinary-looking synchronous calls in an `async fn`: a
`std::fs` read, a `std::thread::sleep`, parsing a large upload, a hot loop over a report.

[`tokio-blocked`](https://docs.rs/tokio-blocked) can be compiled in to catch them. It
times every task poll and logs a WARN when one runs long:

```bash
pnpm dev:api:blocked     # the usual watch-and-reload backend, with the detector on
pnpm test:api:blocked    # the API e2e suite against a detector build (serial, see below)
```

```
WARN tokio_blocked::task_poll_blocked: poll_duration_ns=2227292 callsite.name="runtime.spawn"
  callsite.target="tokio::task" callsite.file="packages/server/src/http.rs" callsite.line=140
```

Both are wrappers around [`scripts/blocked.mjs`](scripts/blocked.mjs), which builds with
`--features blocking-detector` *and* `RUSTFLAGS="--cfg tokio_unstable"` — tokio only emits
the per-task spans the detector measures when both are set. That RUSTFLAGS change is a
different build fingerprint, so these builds go to `target/blocked/` rather than
recompiling the workspace every time you switch back and forth; the first one is cold and
takes a few minutes. Neither the feature nor the flag is ever on in a normal build: the
detector makes the runtime carry span bookkeeping that production should not pay for.

Two knobs, both read at startup and echoed on the "blocking detector active" line:
`SURE_BLOCKED_POLL_US` (default `1000`) warns above that many microseconds in a *single*
poll, and `SURE_BLOCKED_TOTAL_MS` (default off) warns when a task's *total* busy time
reaches that many milliseconds — the "this task is quietly expensive" view, for once the
per-poll warnings are dealt with. Either takes `off` to disable it.

1ms is well above tokio's own 10–100µs guidance on purpose: this is an unoptimised build,
where six requests against a fresh database produce 29 warnings at 150µs and 4 at 1ms, and
a threshold that fires on every request is one you learn to scroll past. Lower it when
you're hunting a specific stall.

Caveats worth knowing before chasing a number. Tokio can only report the `spawn` callsite,
not the blocking line itself, so for a wide future it tells you *that* something blocked,
not where: every request is one task spawned in `packages/server/src/http.rs`, and all of
startup — including migrations — is the root future in `main.rs`, so expect a burst before
the "listening" line (a test run spawns a server per test, so `| grep http.rs` is the way
to read one). Wrap a suspect in `tokio::spawn(...).await` to bisect. And a machine
under load produces false positives, which is why `pnpm test:api:blocked` runs the suite
with one worker; append `--workers=4` to trade signal for wall-clock once you know what
you're looking at.

## Commands

| Command | What it does |
| --- | --- |
| `pnpm dev` | Run backend + Vite dev server together (Vite waits for the backend; both reload on save) |
| `pnpm dev:api` | Backend only, rebuilding and restarting on every Rust/migration/`.env` change |
| `pnpm dev:api:once` | Backend only, built and run once (`cargo run`, no watcher) |
| `pnpm dev:api:blocked` | `pnpm dev:api` with the tokio blocking detector compiled in ([above](#finding-blocking-code)) |
| `pnpm gen:client` | Regenerate the OpenAPI spec and the typed client |
| `pnpm build` | Generate client, build the release backend, build the SPA |
| `pnpm seed` | Seed a running backend with demo data |
| `pnpm test` | API e2e tests, then the web Playwright suite |
| `pnpm test:api` | API e2e tests (TS + Playwright, through the client) |
| `pnpm test:api:blocked` | The same suite, serially, against a build that reports blocking code ([above](#finding-blocking-code)) |
| `pnpm test:api:check` | Type-check the API tests against the client contract (`tsc`) |
| `pnpm test:web:install` | One-time: install the Chromium used by the web suite |
| `pnpm test:web` | Web Playwright suite (builds + boots + seeds automatically) |
| `pnpm lint:rust` / `pnpm fmt:rust` | clippy / rustfmt |

## Testing

- **API** (`packages/api-tests`) — TypeScript + Playwright. Each test spawns the *real*
  compiled `sure-api` binary on an ephemeral port against its own temp-file SQLite
  database and drives it **through the generated `@sure/client`** — so a failure means
  the API *or* the client is wrong, and `pnpm test:api:check` (`tsc`) validates the
  request/response types against the client at compile time. No mocking; fully parallel
  and isolated. Covers CRUD, filtering, transfers, the rules engine (classify / audit /
  undo / manual protection + merchant actions), crons, reports (incl. multi-currency via
  snapshot import), equity vesting, CSV provider sync, and config import/export.
- **Frontend** (`packages/web/tests`): Playwright drives a mobile-Chromium context
  against the built SPA + a freshly-seeded backend, with **screenshot snapshots** of
  every page (baselines committed under `tests/*-snapshots/`, pinned to the dark theme).
  Regenerate with `pnpm --filter @sure/web exec playwright test --update-snapshots`.

## Production (single binary)

Build everything, then run the backend pointed at the built SPA — it serves the app and
the API from one origin:

```bash
pnpm build
WEB_DIR=packages/web/dist DATABASE_URL=sqlite:data/sure.db ./target/release/sure-api
```

Configuration via env — or via a `.env` beside the binary or above it, which the server
loads on startup unless `SURE_ENV_FILE` says otherwise (see
[Configuration](#configuration)): `DATABASE_URL` (default `sqlite:data/sure.db`), `BIND_ADDR`
(default `127.0.0.1:8080`), `WEB_DIR` (serve the SPA when set), `RUST_LOG`,
`BACKGROUND_TASKS` (set to `off` to stop the scheduler — exchange rates, provider polling,
stock prices, transfer linking — from running; the API e2e suite does this so a task
firing on startup can't race a test).

The HTTP layer — cache directives, compression, h2c, and the abuse guards — is described
in [docs/HTTP.md](docs/HTTP.md), along with every env var that tunes it. The defaults are
the intended settings; the most likely one to change is `CORS_ALLOWED_ORIGINS` if you serve
the app from a different hostname than `sure.bullfamilies.com`.

On Linux the process sandboxes itself with [Landlock](https://landlock.io) before it opens
the database or binds a socket: writable access to the data directory and nothing else,
read access to the SPA directory and the system config it needs, no `execve` anywhere, and
outbound TCP limited to 443 and 53. It needs no privileges and nothing on the host — set
`SURE_SANDBOX=enforce` to refuse to start if the kernel can't apply all of it. The policy,
its two deliberate compromises, and the rest of the `SURE_SANDBOX_*` vars are in
[docs/SANDBOX.md](docs/SANDBOX.md).

For the Akahu bank-feed provider (NZ accounts + transactions), set `AKAHU_APP_TOKEN` and
`AKAHU_USER_TOKEN` in the environment or in `.env` (from your Akahu personal-app
dashboard) — without these, "akahu" still
appears as a provider kind but discovery/sync fail with a clear error naming the missing
var. No OAuth redirect flow is implemented; these are the static tokens Akahu issues
directly for personal-app use.

## License

MIT.
