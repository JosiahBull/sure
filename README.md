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

**Agent access (MCP)**
- An [MCP](https://modelcontextprotocol.io) server on the same port, so Claude (or any MCP
  client) can answer "what did grocery spend do after we switched supermarkets" against the
  real ledger — and, behind a second opt-in, file transactions and write rules. Aggregation
  happens server-side so nothing pulls four thousand rows to add them up, and the one tool
  that can change many rows at once refuses to write until it has told you the count and been
  told it back. **Off by default**, and gated twice: `SURE_MCP` sets a ceiling on the host,
  and the working mode is a setting in the app (Settings → Preferences) that can only choose
  within it — changes apply with no restart. Turning it on sends transaction memos to a model
  provider, which is a real departure from the rest of this app. See
  [docs/MCP.md](docs/MCP.md).

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
- **One import for every file**, for sources with no usable API: an ASB transaction
  export, myIR student-loan workbooks, a Sharesies export zip, or a plain CSV. Drop the
  files in one place and Sure works out what each one is — no picking an importer, no
  hunting for the right button. Everything is previewed before it writes, routed to the
  account it belongs to (from a previous import, a stored account number, or by hand),
  reported account by account, idempotent on re-upload, and reversible afterwards. See
  [docs/IMPORT.md](docs/IMPORT.md).
- **History past the bank feed**: a bank's own export reaches about seven years where
  open banking serves two, so a cash/card account's history can be extended behind its
  feed — and rows the feed already covers are held back automatically, so nothing is
  counted twice. Select every account's export at once; each is routed and reported
  separately.
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
- Runtime sqlx queries (not the compile-time macros) — no database needed to build.
- Svelte over React to hit the bundle-size / old-hardware target. Charts are hand-rolled
  SVG (pie, line) fed by backend data; only `d3-sankey` is pulled in for the flow layout.

## Prerequisites

- Rust (nightly is used here; stable ≥ 1.94 with edition 2021 also works — that floor is
  `sqlx` 0.9's MSRV, not ours)
- Node ≥ 22 and `pnpm` (`corepack enable`)
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
| `pnpm lint:rust` / `pnpm fmt:rust` | clippy / rustfmt |
| `pnpm sqlx:prepare` | Regenerate `.sqlx/`, the compile-time query metadata — after any query or migration change |
| `pnpm sqlx:check` | Fail if `.sqlx/` is stale (what pre-commit and CI run) |

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
| `col AS "col?"` | force nullable — the outer side of a `LEFT JOIN` |
| `col AS "col: T"` | decode as `T` — needed for `bool` (a SQLite `INTEGER` is otherwise `i64`), and for an aggregate like `SUM(x)` that SQLite describes as having no type |

A few queries are shaped at runtime rather than fixed — the transaction list's optional
filters, the bulk update/delete id lists, the chunked provider import — and use
`sqlx::QueryBuilder` instead. Each says so where it is written; they are the only SQL in the
codebase the compiler does not check.

## Testing

Three tiers — which one a test belongs in, how to add a fixture, and where the traps are —
in [docs/TESTING.md](docs/TESTING.md). The property they share: **no test reaches a
third-party host.** Every outbound provider request goes to `sure-testproxy`
(`packages/testproxy`), a local reverse-proxy cluster standing in for Frankfurter, Yahoo
Finance and Akahu, in a mode that cannot dial an upstream at all — so a call nobody stubbed
comes back `503` with the method and URI logged, rather than depending on someone else's
uptime.

- **Providers** (`packages/providers/tests`, `packages/testproxy/tests`) — in-process Rust
  fixtures that stand up a proxy cluster and aim one adapter at it. This is the half of an
  adapter no unit test can reach: the URL it builds, the headers it sends, the date window
  it asks for, whether its pagination loop follows the cursor, and how the transport copes
  with a server misbehaving to order (a redirect, a body over the byte ceiling, malformed
  JSON, silence). Milliseconds each, and where a provider bug is cheapest to find.
- **API** (`packages/api-tests`) — TypeScript + Playwright. Each test spawns the *real*
  compiled `sure-api` binary on an ephemeral port against its own temp-file SQLite
  database and drives it **through the generated `@sure/client`** — so a failure means
  the API *or* the client is wrong, and `pnpm test:api:check` (`tsc`) validates the
  request/response types against the client at compile time. Third-party HTTP is the only
  thing stubbed; everything else is real, fully parallel and isolated. Covers CRUD,
  filtering, transfers, the rules engine (classify / audit / undo / manual protection +
  merchant actions), crons, reports (incl. multi-currency via snapshot import), equity
  vesting, config import/export, and the provider surface end to end — CSV sync, an Akahu
  sync's re-sync window, stock-price backfill and its cache, the exchange-rate poll.
- **Frontend** (`packages/web/tests`): Playwright drives a mobile-Chromium context
  against the built SPA + a freshly-seeded backend, with **screenshot snapshots** of
  every page (baselines committed under `tests/*-snapshots/`, pinned to the dark theme).
  Regenerate with `pnpm --filter @sure/web exec playwright test --update-snapshots`.
  It stubs nothing: the proxy is there because the browser is a second caller — a click on
  "Sync now" or "Revalue" makes the backend dial a third party on demand. Claims a
  screenshot cannot pin get their own DOM assertion, since a baseline's 3% tolerance
  passes a two-line notice naming the wrong currency.

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
firing on startup can't race a test), `SURE_MCP` (`off`/`read`/`write` — the *ceiling* on the
MCP endpoint at `/mcp`, off unless set, with the working mode chosen in the app; see
[docs/MCP.md](docs/MCP.md)).

The HTTP layer — cache directives, compression, h2c, and the abuse guards — is described
in [docs/HTTP.md](docs/HTTP.md), along with every env var that tunes it. The defaults are
the intended settings; the most likely one to change is `CORS_ALLOWED_ORIGINS` if you serve
the app from a different hostname than `sure.bullfamilies.com`.

The same thing as a container — one image serving the API and the SPA — is
`pnpm docker:build` (or `./scripts/build.sh`), which builds exactly what the Release workflow
publishes on a tag, so a broken image build is something you find before pushing a tag rather
than after:

```bash
pnpm docker:build
docker run --rm -p 8080:8080 -v sure-data:/data sure:latest
```

The image is ~28 MB and contains the statically-linked binary, the built SPA, and a CA
bundle — and nothing else. No shell, no package manager, no libc, no `curl`, no coreutils:
`gcr.io/distroless/static` plus a musl build, so the only executable in it is `sure-api`
itself. That is most of the point. A container whose only program is the one you meant to
run has nowhere to go if the process is ever compromised, and there is no distro package
stream underneath it to track CVEs against.

Two things that used to be installed for are gone with it. `curl` was there for the
`HEALTHCHECK`, which is now `sure-api --health-check` — the server asking itself, through the
HTTP client the provider adapters already link in, so it costs the image nothing. `tini` was
there for PID-1 signal handling and zombie reaping, and this process needs neither: it
installs its own `SIGTERM`/`SIGINT` handlers, and the Landlock policy forbids `execve`, so it
has no children to reap.

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

[AGPL-3.0-only](LICENSE).

Not a permissive licence by preference but by inheritance: parts of the web layer —
notably the account-subtype table, the palette and the design tokens in
`packages/web/src/lib/accountSubtypes.ts`, `accountMeta.ts` and `app.css` — were
transcribed from [we-promise/sure](https://github.com/we-promise/sure), which is
AGPL-3.0. That makes this a derivative work, so it cannot be relicensed more
permissively. Comments naming "the reference" throughout the web layer mark the passages
concerned. This project is an independent Rust/Svelte rewrite and is not affiliated with,
endorsed by, or supported by that project or its authors.

`-only` rather than `-or-later`: upstream ships the bare licence text with no "or later"
grant, so none is passed on.

### Bundled third-party assets

- **Geist** and **Geist Mono** (`packages/web/public/fonts/`) — © 2024 The Geist Project
  Authors, under the [SIL Open Font License 1.1](packages/web/public/fonts/OFL.txt). The
  fonts are *not* covered by the AGPL above; OFL clause 2 requires that licence to travel
  with them, which is why the text is vendored beside the `.woff2` files.
- **Lucide** icons (`packages/web/src/lib/icons.ts`) — ISC.
