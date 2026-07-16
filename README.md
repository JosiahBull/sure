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
  (dedupes on re-sync).

**Reports & UI**
- Net-worth line over time, income/expense donut per category, and a **Sankey**
  money-flow diagram — all computed server-side.
- Global time-range filter (last month / 90 days / YTD / 12 months / all time) and a
  one-off toggle.
- Installable PWA (iPhone "Add to Home Screen").

## Architecture

Flat pnpm + Cargo workspace under `packages/`. The backend is split into layered crates
with a one-way dependency graph (`core ← dal, providers ← api`) — see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the reasoning and the interface choices.

```
packages/
  core/         Rust: shared domain types + AppError, feature-gated for HTTP (sure-core).
  dal/          Rust: SQLite pool, pragmas, and embedded migrations (sure-dal).
  providers/    Rust: TransactionProvider trait + registry + CSV importer (sure-providers).
  api/          Rust: Axum HTTP layer + report/rules engines; `sure-api` + `gen-openapi` bins.
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

Open http://localhost:5173. To load demo data into the dev database:

```bash
pnpm seed                # posts realistic data to http://127.0.0.1:8080
```

## Commands

| Command | What it does |
| --- | --- |
| `pnpm dev` | Run backend + Vite dev server together |
| `pnpm gen:client` | Regenerate the OpenAPI spec and the typed client |
| `pnpm build` | Generate client, build the release backend, build the SPA |
| `pnpm seed` | Seed a running backend with demo data |
| `pnpm test` | API e2e tests, then the web Playwright suite |
| `pnpm test:api` | API e2e tests (TS + Playwright, through the client) |
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

Configuration via env: `DATABASE_URL` (default `sqlite:data/sure.db`), `BIND_ADDR`
(default `127.0.0.1:8080`), `WEB_DIR` (serve the SPA when set), `RUST_LOG`.

## License

MIT.
