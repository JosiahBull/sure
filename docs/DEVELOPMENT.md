# Development

Everything needed to build, run and test Sure locally. If you only want to *run* it, the
[README](../README.md) covers that in a couple of commands and you need none of this.

Contributor rules — the enum/exhaustive-match conventions, the personal-data rule for
fixtures, and what the git hooks enforce — are in [CLAUDE.md](../CLAUDE.md).

## Prerequisites

- Rust (nightly is used here; stable ≥ 1.94 also works — that floor is `sqlx` 0.9's MSRV,
  not ours. The workspace is on edition 2024, which needs only 1.85)
- Node ≥ 24 and `pnpm` (`corepack enable`)
- `sqlx-cli`, **only if you change a SQL query or add a migration** —
  `cargo install sqlx-cli@0.9.0 --no-default-features --features sqlite,rustls` — so you can run
  `pnpm sqlx:prepare`. Install it at the same version as the `sqlx` crate in `Cargo.toml` (the
  version CI pins, see `.github/workflows/checks.yml`): the CLI writes the `.sqlx/` metadata the
  macros read, and a mismatch surfaces only as "metadata is out of date". Building and running
  the app needs neither it nor a database: the compile-time query check reads the committed
  `.sqlx/` metadata (see [Compile-time checked SQL](#compile-time-checked-sql)).

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
restarts the API ([`scripts/dev-api.mjs`](../scripts/dev-api.mjs)). It builds *before* it
stops the old process and keeps that process running if the build fails, so a compile
error leaves the last working backend serving the SPA instead of dropping it — fix the
error and the next save picks up where it left off. `pnpm dev:api:once` is the plain
`cargo run`, for when you want a single build and no watcher.

Open http://localhost:5173. To load demo data into the dev database:

```bash
pnpm seed                # posts realistic data to http://127.0.0.1:8080
```

## Configuration

Everything has a working default, so none of this is needed to start. For what you do set,
copy [`.env.example`](../.env.example) to `.env` (gitignored) — the backend loads it on
startup, searching the working directory and its parents, so `pnpm dev`, `cargo run`, and
the release binary all find the repo-root file wherever they're run from. Real environment
variables win over the file; `SURE_ENV_FILE` points at a different path, or set it empty to
load nothing (the test suites do this, so a developer's tokens can't reach them). The vars
themselves are listed in the [README](../README.md#running-it) and in [HTTP.md](HTTP.md).

## Commands

| Command | What it does |
| --- | --- |
| `pnpm dev` | Run backend + Vite dev server together (Vite waits for the backend; both reload on save) |
| `pnpm dev:api` | Backend only, rebuilding and restarting on every Rust/migration/`.env` change |
| `pnpm dev:api:once` | Backend only, built and run once (`cargo run`, no watcher) |
| `pnpm gen:client` | Regenerate the OpenAPI spec and the typed client |
| `pnpm build` | Generate client, build the release backend, build the SPA |
| `pnpm docker:build` | Build the release container image locally (`./scripts/build.sh`) |
| `pnpm seed` | Seed a running backend with demo data |
| `pnpm test` | Rust tests, then the API e2e tests, then the web Playwright suite |
| `pnpm test:rust` | Rust unit, integration and doc tests (`cargo test --workspace --all-features`) |
| `pnpm test:api` | API e2e tests (TS + Playwright, through the client) |
| `pnpm test:api:check` | Type-check the API tests against the client contract (`tsc`) |
| `pnpm test:web:install` | One-time: install the Chromium used by the web suite |
| `pnpm test:web` | Web Playwright suite (builds + boots + seeds automatically) |
| `pnpm snapshots:verify` | Run that suite in CI's pinned container against the committed `-linux.png` baselines (needs Docker) |
| `pnpm snapshots:update` | Regenerate those baselines in the same container and copy them into the tree |
| `pnpm lint:rust` / `pnpm fmt:rust` | clippy / rustfmt |
| `pnpm sqlx:prepare` | Regenerate `.sqlx/`, the compile-time query metadata — after any query or migration change |
| `pnpm sqlx:check` | Fail if `.sqlx/` is stale (what pre-commit and CI run) |

## Workspace layout

Flat pnpm + Cargo workspace under `packages/`. The backend follows a ports-and-adapters
(hexagonal) shape — an application core depending only on trait ports, with the web
framework and the database wired in as adapters at the edges — see
[ARCHITECTURE.md](ARCHITECTURE.md) for the reasoning and the interface choices, and
[HTTP.md](HTTP.md) for what the HTTP boundary does around them (caching, compression,
HTTP/2, rate limiting). On Linux the server also sandboxes itself with Landlock before it
does any work — see [SANDBOX.md](SANDBOX.md).

```
packages/
  core/         Rust: domain types + AppError, no persistence/web deps (sure-core).
  scheduler/    Rust: generic recurring-task scheduler, storage-agnostic (sure-scheduler).
  dal/          Rust: SQLite pool, migrations, every SQL query (sure-dal).
  providers/    Rust: TransactionProvider trait + registry + CSV/Akahu/Yahoo/FX clients (sure-providers).
  app/          Rust: the application core — use-case services + repo ports, no SQL/HTTP (sure-app).
  api/          Rust: Axum routes/handlers + OpenAPI, depends only on sure-app; `gen-openapi` bin (sure-api).
  mcp/          Rust: MCP tools/resources/prompts over the same core, for an agent (sure-mcp).
  server/       Rust: the composition root — wires sure-dal/sure-providers into sure-app/sure-api; owns `main` (sure-server).
  testproxy/    Rust: the record/replay proxy cluster standing in for every third-party host (sure-testproxy).
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
- Every SQL query is checked at compile time against the committed `.sqlx/` metadata, so
  no database is needed to build (see below).
- Svelte over React to hit the bundle-size / old-hardware target. Charts are hand-rolled
  SVG (pie, line) fed by backend data; only `d3-sankey` is pulled in for the flow layout.

## Compile-time checked SQL

Every query in `packages/dal` uses `sqlx::query!` / `query_as!` / `query_scalar!`, so the SQL
is verified against the real schema when you compile: a column that doesn't exist, a bind count
that doesn't line up, or a row struct whose types disagree with the table is a build error
instead of a 500 on whichever request happens to hit it first.

The schema it checks against is whatever `packages/dal/migrations` produces, cached in the
committed `.sqlx/` directory. `.cargo/config.toml` sets `SQLX_OFFLINE=true`, so a build never
opens a database — a fresh clone, CI and a container all compile with no `DATABASE_URL` at all,
and nothing can wander into your real `data/sure.db`.

So the one rule: **change a query or add a migration → `pnpm sqlx:prepare`, and commit the
`.sqlx/` change with it.** `pnpm sqlx:check` (pre-commit, and a CI gate) fails when you forget.
The script builds a throwaway database under `target/`, applies the migrations to it, and
describes the queries against that.

Three annotations show up a lot, because SQLite's `describe` is conservative about nullability:

| Annotation | Means |
| --- | --- |
| `col AS "col!"` | force non-null — an `INTEGER PRIMARY KEY` (a rowid alias), or any column read back out of a subquery, CTE, `GROUP BY` or window function |
| `col AS "col?"` | force nullable — the outer side of a `LEFT JOIN`, and any nullable `INTEGER` in an `INSERT`/`UPDATE … RETURNING` clause, which sqlx 0.9 otherwise infers as non-null |
| `col AS "col: T"` | decode as `T` — needed for `bool` (a SQLite `INTEGER` is otherwise `i64`), and for an aggregate like `SUM(x)` that SQLite describes as having no type |

A missing `?` is the dangerous one: sqlx widens a non-null column into an `Option` field without
complaining, so it compiles clean and returns a wrong value at runtime rather than failing the
build.

A few queries are shaped at runtime rather than fixed — the transaction list's optional
filters, the bulk update/delete id lists, the chunked provider import — and use
`sqlx::QueryBuilder` instead. Each says so where it is written; they are the only SQL in the
codebase the compiler does not check.

## Testing

Three tiers — provider fixtures, API e2e, and the frontend visual suite — with which one a
test belongs in, how to add a fixture, and where the traps are, in
[TESTING.md](TESTING.md). The property they share: **no test reaches a third-party host.**
Every outbound provider request goes to `sure-testproxy` (`packages/testproxy`), a local
reverse-proxy cluster standing in for Frankfurter, Yahoo Finance and Akahu, in a mode that
cannot dial an upstream at all — so a call nobody stubbed comes back `503` with the method
and URI logged, rather than depending on someone else's uptime.

Run them with `pnpm test`, or one tier at a time (`pnpm test:rust`, `pnpm test:api`,
`pnpm test:web`) — see [Commands](#commands).

The visual suite's baselines are per-platform: `pnpm test:web` compares against `-darwin.png`
on a Mac, while CI compares against `-linux.png` rendered in a pinned container. A UI change
usually moves both, and `pnpm snapshots:update` regenerates the Linux half locally in that same
container so it lands in the same commit — see
[Regenerating the Linux baselines](TESTING.md#regenerating-the-linux-baselines).

## Further reading

| Doc | What it covers |
| --- | --- |
| [ARCHITECTURE.md](ARCHITECTURE.md) | The hexagonal refactor: where the seams are and why |
| [HTTP.md](HTTP.md) | Caching, compression, h2c, rate limiting, and every env var that tunes them |
| [TESTING.md](TESTING.md) | The three tiers, fixtures, and the record/replay proxy |
| [CI.md](CI.md) | The workflows, and how a release is cut |
| [SANDBOX.md](SANDBOX.md) | The Landlock policy and its two deliberate compromises |
| [MCP.md](MCP.md) | The MCP server: tools, resources, and the two opt-in gates |
| [IMPORT.md](IMPORT.md) | The one-drop file import pipeline |
| [FORECAST.md](FORECAST.md) | Where each forecast number comes from |
| [STUDENT-LOAN.md](STUDENT-LOAN.md) | Importing and tracking an IR student loan |
