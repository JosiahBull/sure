# sure — agent/contributor rules

Sure is a local-first financial tracker: a Rust/Axum/SQLite backend and a Svelte SPA.
Rust workspace (`Cargo.toml`): `packages/core` (domain types, zero I/O) → `packages/dal`
(all `sqlx`/SQL, converts rows to core types) → `packages/app` (services/use-cases,
talks to the DAL only through `sure_app::ports` traits) → `packages/api` (Axum routes,
OpenAPI) → `packages/server` (binary: wires it together, HTTP transport concerns) —
plus `packages/providers` (bank/broker/price feed adapters), `packages/scheduler`
(cron runner), `packages/appbase` (process lifecycle: signals, cancellation, and
draining what the process spawned — depends on nothing else in the workspace), and
`packages/testproxy` (test support: the record/replay proxy cluster standing in for every
third-party host, so no test reaches one — also depends on nothing else in the workspace,
because `sure-providers` dev-depends on *it*; see `docs/TESTING.md`).

**Upstream wire formats live in their own client crates**, outside that dependency chain and
un-prefixed because they are clients for somebody else's API rather than parts of Sure:
`akahu-client` (published, from crates.io) and, in-tree because they have no users outside it
yet, `packages/frankfurter-client`, `packages/house-pricer-client` and
`packages/yahoo-finance-client`. Each owns exactly one thing — what the upstream's JSON *is* —
and depends on no other workspace member, **not even on its siblings**, so it cannot name an
account, a valuation or a minor unit. The point is blast radius: these endpoints are
undocumented and rename fields without notice, and the fix for that should be one line in one
crate that recompiles no domain logic. `sure-providers` is the only member that may name one.
The division of labour, in the four that exist:

- **the client** owns the URL shape, the status codes, and anything that is only true of *this
  wire* — Yahoo's `User-Agent`, its two parallel `timestamp`/`close` arrays flattened into
  candles, the `Retry-After` on a refusal parsed into a `Duration`. Each has a
  `#[non_exhaustive]` error enum with a distinct variant for every outcome a caller must
  *branch* on rather than a stringly message: Yahoo's `404` (a delisted symbol, which is not a
  failure) is not its `NoChartData` (a `200` that said nothing, which is), and neither is
  `RateLimited`.
- **the adapter** keeps what is genuinely ours: which `reqwest::Client` policy applies
  (`Endpoint`, the shared body ceiling passed to every client's `with_max_response_bytes`), the
  `Throttle` and how long a refusal stands the process down, the caches, Sure's exchange
  vocabulary (`"NZX"` → `.NZ` is `symbol_for`, in the adapter), and every mapping into a domain
  type — `f64` → `Decimal`, dollars → minor units, an epoch second plus a `gmtoffset` → a
  trading day.

The duplication this buys is deliberate and small: the Frankfurter and Yahoo clients parse
`Retry-After` identically, in twenty lines each, because a shared crate for it would be the
first edge between two crates whose whole value is having none. Each copy is tested where it
lives. A new upstream gets the same split.
pnpm workspace (`pnpm-workspace.yaml`): `packages/web` (the SPA),
`packages/client` (generated typed API client), `packages/api-tests` (Playwright
backend e2e suite).

This file is the *rules*. The dev workflow they apply to — prerequisites, the reloading dev
server, the command reference, and the testing tiers — is `docs/DEVELOPMENT.md`, and
`README.md` is deliberately user-facing only (what Sure is, and how to run it).

## Rules

### 1. Domain values are enums, not strings

If a value has a closed, known set of legal values, it is a Rust enum — never a bare
`&str`/`String` — anywhere above the boundary that reads or writes it as text. The
enums already exist for accounts: `AccountKind`, `AccountClass`, `RateType`,
`AreaUnit`, `MileageUnit`, `TaxTreatment` (`packages/core/src/types.rs`). The **one**
legal place a domain value is text is the edge it's serialised at — a SQLite `TEXT`
column, an HTTP wire body, an external feed payload — and it must be parsed into the
enum immediately on the way in and rendered to text exactly once on the way out.
`packages/dal/src/accounts.rs`'s `TryFrom<AccountRow> for Account` is the model to
copy: it reads the `kind` column as `String` and immediately does
`let kind: AccountKind = r.kind.parse()?;` before the value goes anywhere else.

Before (what NOT to do — two independent, silently-driftable copies of the same
mapping, operating on strings a typo can slip through):

```rust
// packages/core/src/types.rs
pub fn class_of(kind: &str) -> &'static str {
    match kind {
        "cash" | "bank" | "savings" => "cash",
        "credit_card" | "revolving_credit" | "mortgage" | "student_loan" | "loan" | "liability" => "liability",
        "shares_nz" | "shares_us" | "shares_private" | "brokerage" | "crypto" => "investment",
        _ => "asset",
    }
}

// packages/app/src/reports.rs — a private, hand-copied duplicate of the same table
fn class_of(kind: &str) -> &'static str { /* ... identical match ... */ }
```

After — one mapping, on the enum, exhaustive:

```rust
// packages/core/src/types.rs — the only mapping; AccountKind::class already does this
impl AccountKind {
    pub fn class(self) -> AccountClass {
        match self {
            AccountKind::Cash | AccountKind::Bank | AccountKind::Savings => AccountClass::Cash,
            AccountKind::CreditCard | AccountKind::RevolvingCredit | AccountKind::Mortgage
            | AccountKind::StudentLoan | AccountKind::Loan | AccountKind::Liability => AccountClass::Liability,
            AccountKind::Vehicle | AccountKind::RealEstate | AccountKind::Asset => AccountClass::Asset,
            AccountKind::SharesNz | AccountKind::SharesUs | AccountKind::SharesPrivate
            | AccountKind::Brokerage | AccountKind::Crypto => AccountClass::Investment,
        }
    }
}
```

`packages/app/src/reports.rs` should call `AccountKind::class` (parsing the DAL's
`kind: String` once, at the report row boundary) instead of keeping its own
`class_of(&str)`. Adding `AccountKind::Timeshare` next year is then a compile error at
the one real mapping, not a value that quietly falls into `_ => "asset"` in one copy
and not the other.

### 2. Match exhaustively

No `match` (or `if`/`else if` chain standing in for one) over a domain enum — or over
a `&str`/`String` that is really one of these closed sets — may have a `_ =>` /
`variant | _ =>` arm, and no `unwrap_or`/`unwrap_or_default` may quietly turn an
unparseable domain value into a default. Adding a variant must fail the build at
every site that has to decide what it means, not silently take the fallback branch.

The escape hatch: a wildcard arm is legitimate only over a genuinely open string (see
the NOT-violations list below) or an external `#[non_exhaustive]` enum you don't
control (e.g. `sqlx::Error`, `http::StatusCode`) where new variants really do arrive
without warning and a catch-all is the only option. **Every such arm needs a one-line
comment saying which of those two reasons applies** — an un-commented `_ =>` over an
enum is treated as an oversight, not a decision.

Genuinely open (not violations): ISO-8601 date/datetime strings, currency codes (FK
into `currencies`), tickers, exchange/institution/lender/broker names, free text,
ids, SQL fragments, HTTP path templates, env var names, file paths, provider external
ids, JSON blobs.

### 3. Fixtures carry real data's *shape*, never its identifiers

A parser's best test input is a real export — for `sure_providers::asb` and
`sure_providers::myir` the real memo genuinely *is* the specification, so reaching for one
is the right instinct. What must not come with it is anything identifying. Both were built
that way and, until 2026-08-04, between them carried an IRD number, an ASB account number,
two third parties' account numbers, a family member's name, two payee names, an employer
with its payroll id, a salary figure, and three payment-card last-fours. Getting them back
out meant rewriting all 58 commits, so the cost of noticing late is high.

**Keep** — not personal, and in `asb.rs` load-bearing: merchant brands (`Countdown`,
`KMART`), suburbs, bank/branch prefixes, transaction-type codes, ordinary amounts. ASB
splits a memo at character twelve, so a 12-character particulars field, and a mid-word split
like `TWL 119 ALBA NY`, are precisely what the `rejoin_split_field` tests pin.

**Replace** — account numbers (yours *and* anyone else's), IRD numbers, card last-fours,
people's names, payee and employer names, payroll and payment references, salary figures.

**Replace length-for-length.** A twelve-character field that becomes fourteen silently stops
exercising the boundary its test exists for. Reuse the fakes already in the tree:
`12-3456-…` for a bank account (`scripts/seed.mjs`, `accounts.rs`), `012-345-678` for an IRD
number.

**Check provenance before committing a fixture — "it looks synthetic" is not evidence.**
`.githooks/pre-commit` now does this for you via `scripts/pii-scan.mjs` (see the enforcement
section for what it covers and what it misses), but do it by hand while writing the fixture
rather than discovering it at commit time. `data/sure.db` is the ground truth, and a byte grep
answers it without opening a sqlite handle, so it cannot write to the live DB (see the
`data/sure.db` convention below):

```sh
# Any hit means the value came from real data. Grep a known-fake control too, so a
# zero-hit result is evidence the grep works rather than that the file didn't match.
rg -a -F "$SUSPECT" data/sure.db          # the literal out of the fixture you are adding
rg -a -F '12-3456-0000123' data/sure.db   # control: an established fake, must be 0
```

Then grep the **whole tree**, not the file you are editing — one pasted value spreads
further than where it landed. The same loan amount had been independently copied into
`sure_app::tasks::balance_delta`; an account number reached the generated
`packages/client/src/schema.d.ts` through a doc example; and a card last-four in
`scripts/seed.mjs` was *rendered into* the committed Playwright baselines, which no text
substitution can reach (they had to be stripped from history and regenerated). Look for a
value's other spellings while you are there: an IRD number appears both as `nnn-nnn-nnn` and
undashed inside a direct-debit memo, and every amount is written twice — once as a decimal
string and again as minor units, with and without digit grouping (`400.00`, `400_00`,
`40000`).

## Conventions

- **Scripts** (`package.json`): `pnpm dev` runs API + web together; `pnpm build` runs
  `gen:client` then `cargo build --release` then the web build; `pnpm test` runs
  `test:rust` (`cargo test --workspace --all-features`) then `test:api`
  (`@sure/api-tests`, Playwright-driven backend e2e) then `test:web`;
  `pnpm lint:rust` is `cargo clippy --all-targets -- -D warnings`; `pnpm fmt:rust` is
  `cargo fmt --all`; `pnpm sqlx:prepare` regenerates the compile-time query metadata in
  `.sqlx/` and `pnpm sqlx:check` verifies it is current (see the query-checking convention
  below). `test:rust` carries `--all-features` but deliberately *not*
  `--all-targets`: for `cargo test` that flag excludes doctests rather than adding to
  them, which would silently drop `sure_appbase`'s usage example.
- **Spawning background tasks**: use `Shutdown::spawn` / `Shutdown::spawn_blocking`
  (`packages/appbase`), never a bare `tokio::spawn`, for anything that outlives a
  request. Only tracked tasks are cancelled and waited for at shutdown; an untracked one
  is dropped mid-flight when the process exits, and — being invisible to the drain — will
  not show up in the shutdown report or fail `specs/shutdown.spec.ts`. `spawn` is
  `#[track_caller]`, so a debug build names the spawning line when a task overruns the
  drain; a helper that wraps it needs `#[track_caller]` too or it becomes the reported
  site for everything it spawns. See `docs/HTTP.md` for the phases and their env vars.
- **Provider endpoints are injected; a test that needs an upstream stubs it.** Every adapter
  in `packages/providers` is constructed with an `Endpoint` (`src/http.rs`: `https://` anywhere,
  or plaintext only to loopback, for a test proxy) and `AkahuProvider` with its credentials as
  values — so nothing reads the environment on a request path and only `sure-server` decides
  where an adapter points. Never reach for a `DEFAULT_BASE_URL` at a call site. A test that
  needs an upstream registers a stub on the `sure-testproxy` cluster (`packages/testproxy`) it
  is pointed at: every backend both Playwright suites spawn is, in replay mode with no
  snapshots, so a call nobody stubbed is a `503 {}` that never leaves the machine rather than a
  dependency on someone else's uptime — and, since that 503 sends the adapter down an error path
  the test author did not mean to exercise, it now **fails the test that made it**
  (`failOnUnstubbedRequests` in `packages/api-tests/fixtures.ts`, and the run-level equivalent in
  `packages/web/tests/global-teardown.ts`). A test that means to provoke one declares it with
  `allowUnstubbed({ upstream, path_pattern, why })`. **Akahu and House Pricer traffic is never
  recorded into this repo** — `scripts/pii-scan.mjs`'s `PRIVATE_UPSTREAMS` refuses either by path
  *and* by content, and decodes base64 bodies so an `.ndjson` cannot smuggle an account number
  past it. Akahu's traffic is account numbers and transaction memos; House Pricer's is a street
  address, GPS centroid, title boundary polygon and land value, so one exchange is a dossier on
  where somebody lives — neither is market data about an instrument. Frankfurter and Yahoo are
  public market data and *are* recorded, in `packages/providers/tests/snapshots/`: those captures are what prove
  the adapters still parse the real document, rather than the subset of it a hand-written fixture
  would carry. Nothing re-checks them against the live API, so a capture is evidence about the day
  it was taken — `pnpm fixtures:record` and read the diff when a price or FX path misbehaves
  against the real app but not in the suite. Tiers, fixtures and the traps: `docs/TESTING.md`.
- **Every outbound call is paced, and every upstream answer is worth reusing.** A third party's
  rate limits are enforced on the *household*, not on this app, so a burst costs the one person
  whose data it is. Three guards, all injected as `Pacing`/`ProviderLimits` from `sure-server`
  alongside the `Endpoint` (nothing in `sure-providers` reads configuration): a 500ms floor on
  the gap between two requests to one host, applied inside the adapter — including between the
  pages of an Akahu sweep; a stand-down window that a `429` arms, recognised by the wire crate as
  its own error variant so a rate limit is never flattened into a generic 4xx and retried
  straight away; and a TTL on the answers that describe what exists (Akahu's account listing,
  Yahoo's `404` for a symbol), because the UI asks for those once per render and neither changes
  minute to minute. Pacing **sleeps**, a cooldown **refuses** — five minutes of `Retry-After` slept
  inside a request would blow the 30s route deadline. On top of that `sure_app::sync` will not
  start a second *successful* sync of one provider inside `PROVIDER_SYNC_COOLDOWN_SECS`; it
  replays the last run as `SyncReport { fresh: false }`, which is what stops the UI reporting an
  import that never happened. Adding an adapter means adding a `Throttle`, not just a client.
  The numbers and their env vars: `docs/HTTP.md`.
- **Every query is compile-time checked; `.sqlx/` is how.** All SQL in `sure-dal` goes through
  `sqlx::query!` / `query_as!` / `query_scalar!`, so a column that doesn't exist, a bind count
  that doesn't match, or a row struct whose types disagree with the table is a build error
  rather than a runtime failure on whichever request reaches it first. (It found a live one on
  the way in: `snapshot.rs` had been exporting `forecast_events` by four columns migration 0022
  removed, which 500'd `GET /api/config/export` for anyone with a forecast event and passed its
  tests only because the fixture never inserted one.) The macros check against the committed
  `.sqlx/` metadata, and `.cargo/config.toml` pins `SQLX_OFFLINE=true` so a build never opens a
  database to do it — which is also what keeps `cargo build` away from `data/sure.db` whatever
  a developer's `.env` says. **Edit a query or a migration and you must run `pnpm sqlx:prepare`
  and commit the result**; `pnpm sqlx:check` (pre-commit, and the `sqlx` job in `checks.yml`)
  fails if you didn't. `scripts/sqlx-prepare.mjs` applies the migrations to a throwaway database
  under `target/` and describes against that, so the checked schema is always the one the
  migrations produce.
  Three annotations recur because SQLite's `describe` is conservative — `col AS "col!"` forces
  non-null (a rowid alias, or anything read out of a subquery/CTE/`GROUP BY`), `col AS "col?"`
  forces nullable, and `col AS "col: T"` names the decode type (needed for `bool`, and for an
  aggregate like `SUM(x)` that describes as `NULL`). See the module docs in
  `packages/dal/src/lib.rs`. The handful of queries whose *shape* is decided at runtime — the
  transaction list's optional filters, `bulk_update`/`bulk_delete`, the chunked provider import
  — keep `QueryBuilder`/`format!` and each says so at the call site; they are the only unchecked
  SQL left.
- **Pre-commit** (`.githooks/pre-commit`, wired by the `prepare` script): runs
  `node scripts/pii-scan.mjs` (rule 3; first, because it is the cheapest gate and the only
  one guarding something a later gate cannot undo), then `node scripts/sqlx-prepare.mjs --check`
  (before the compilers, so stale query metadata reports itself instead of surfacing as a
  baffling query error out of clippy), then `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-features` (directly after clippy, which has already
  compiled the workspace, so the marginal cost is linking and running the test binaries;
  CI runs the same command as the `test` job in `checks.yml`),
  `pnpm test:api`, `pnpm --filter @sure/web check`. It deliberately skips the web
  *visual* Playwright suite (only deterministic in CI's pinned container). Bypass in
  an emergency with `git commit --no-verify`, not by weakening a lint.
- **commit-msg** (`.githooks/commit-msg`, same `prepare` script): runs
  `node scripts/pii-scan.mjs --message "$1"` — rule 3 over the commit *message*, which the
  pre-commit scan structurally cannot see because it reads staged file diffs. The gap was
  real: the commit introducing `pii-scan.mjs` quoted, in its own message, the third-party
  account number it had just found in an `asb.rs` doc comment, and it survived the
  2026-08-04 scrub because `--replace-text` rewrites blobs while a message needs
  `--replace-message`. Costs a file read plus the same provenance grep, so it stays in the
  milliseconds. The message is reduced to what git will actually store first — comment lines
  and everything below a `-v` scissors line are dropped — or the default template's
  `# Author: …` line would fail every commit on the `email` pattern. `--no-verify` skips
  this and pre-commit together.
- **Generated client**: `packages/client/src/schema.d.ts` is generated by
  `pnpm gen:client` (openapi-typescript over `packages/client/openapi.json`, itself
  generated by `pnpm gen:openapi` from `sure-api`'s utoipa schema). It is tracked in
  git and carries `// This file was auto-generated ... Do not make direct changes` —
  regenerate it, never hand-edit it.
- **Migrations** (`packages/dal/migrations/`, embedded via
  `sqlx::migrate!("./migrations")` in `packages/dal/src/lib.rs`): append-only. sqlx
  records each applied migration's checksum in `_sqlx_migrations`; editing a migration
  that has already run anywhere breaks that checksum check on next connect. A schema
  change is always a new numbered file — and a new file means running `pnpm sqlx:prepare`,
  because the queries are checked against the schema the migrations produce.
- **Money and rates**: amounts are signed integer minor units (e.g. `114_269_63` ==
  $114,269.63 — see the `clippy::inconsistent_digit_grouping` allow and its comment in
  `packages/dal/src/lib.rs`); rates are basis points (`packages/core/src/types.rs`,
  e.g. `MortgageMeta::interest_rate_bps`, `crons.rate_bps`).
- **`data/sure.db` is live developer data**, not a fixture — one WAL segment alone is
  hundreds of MB of real transaction history. Nothing in tooling, a test run, or an
  agent session may write to it or the default `DATABASE_URL`; the e2e suite and any
  script must point at their own throwaway database (see `test-e2e.db` /
  `global-setup.ts` in `packages/api-tests`) instead of touching the default.

## Enforcement plan

`clippy::wildcard_enum_match_arm` is the direct machine check for rule 2, and is now
applied — `cargo clippy --workspace --all-targets --all-features -- -D warnings` is
clean with it active.

```toml
# root Cargo.toml
[workspace.lints.rust]
# (no new rustc lints yet — clippy carries this rule)

[workspace.lints.clippy]
# Rule 2, direct enforcement: a `_` or `variant | _` arm over an enum is a hard error.
# Every legitimate use (an external #[non_exhaustive] enum) needs `#[allow(..)]` with
# a comment, per the CLAUDE.md rule — that's the intended friction.
wildcard_enum_match_arm = "deny"
# Complements the above: catches `Foo::A | _ =>` where naming the one remaining
# variant instead of `_` is just as short — zero violations, so it's free to carry
# and keeps a future two-variant enum honest from day one.
match_wildcard_for_single_variants = "deny"
```

```toml
# every member crate's Cargo.toml — no exceptions, test-support crates included
# (packages/core, dal, app, api, server, providers, scheduler, appbase, testproxy)
[lints]
workspace = true
```

Both lines above are live in every member crate's `Cargo.toml` today.

Lints considered and **not** included, with the measured reason:

| lint | measured | verdict |
|---|---|---|
| `clippy::wildcard_enum_match_arm` | included, 0 outstanding violations (was 9 raw warnings pre-fix; see breakdown below) | **include** |
| `clippy::match_wildcard_for_single_variants` | 0 warnings | **include** — free, guards the future |
| `clippy::string_to_string` | n/a — clippy reports it **removed** (`lint \`clippy::string_to_string\` has been removed: \`clippy::implicit_clone\` covers those cases`) | **excluded**: does not exist in this clippy version; asking for it would make `-D warnings` fail outright |
| `clippy::str_to_string` | 272 warning sites (`-W clippy::str_to_string`), e.g. every `"buy".to_string()` / `ccy.to_string()` in `packages/dal`, `packages/providers`, `packages/app` | **excluded**: this lint fires on *any* `&str.to_string()`, which is pervasive, ordinary, and orthogonal to stringly-typed domain values — turning it on is 272 unrelated call-site edits for no enforcement of either rule |
| `clippy::enum_glob_use` | 5 warnings, all `use AccountKind::*;` in `packages/core/src/types.rs` (the exhaustive matches in `class`, `as_str`, `FromStr`, `profile_for`, `default_for`) | **excluded**: these five call sites are exactly the idiom rule 2 wants (a short, exhaustive match over every variant) — the lint would fight the fix, not enforce it |

`wildcard_enum_match_arm`'s original 9 raw hits, and how each was resolved:

- `packages/app/src/forecast.rs`'s `amortization_terms` — `_ => return None` over
  `AccountMetadata` — rewritten to name every non-Mortgage/Loan variant explicitly.
- `packages/dal/src/accounts.rs`'s `set_original_amount` — `_ => return Ok(())` over
  `AccountMetadata` — same treatment, every non-Mortgage/Loan variant named.
- `packages/dal/src/accounts.rs`'s `metadata_from_stored` — a `match value { Value::Object(..) => .., _ => .. }`
  over `serde_json::Value` (a fixed, non-domain container type, not one of our own
  enums) — rewritten as `if let Value::Object(..) = value { .. } else { .. }` instead,
  which has no wildcard arm to justify at all.
- The 6 `other => AppError::from(other)` arms over `sqlx::Error`
  (`packages/dal/src/{brokerage,crons,currencies,merchants,providers,transactions}.rs`,
  each a small `map_fk`/`unique_or_fk` helper) — `sqlx::Error` is `#[non_exhaustive]`
  upstream, so these are the legitimate escape hatch. Each now carries
  `#[allow(clippy::wildcard_enum_match_arm)]` **on the enclosing function** (a match-arm-level
  `#[allow]` does not suppress this particular lint — confirmed by trial) plus the
  one-line justifying comment the rule demands.

Five more closed enums were typed and given the same exhaustive-match treatment as
part of landing the lints (so the codebase would already be clean the moment
`-D warnings` started enforcing it): `LotKind` (`holdings.kind`), `RuleRunKind`
(`rule_runs.kind`), `SyncOutcome` (`provider_syncs.status`; named to avoid shadowing
`Result::Ok`), `Interval` (a report's date-sampling granularity — parsed at the HTTP
edge in `sure-api`'s `routes::reports`, rejecting an unrecognised value with a 400
instead of silently defaulting to `month`), and `SankeyNodeKind` (income/center/
expense/savings, built directly rather than parsed, so no `FromStr` — see
`packages/core/src/{brokerage,rules,providers}.rs` and `packages/app/src/reports.rs`).

**Rule 3 can't be a clippy lint** — whether an account-number-shaped literal is real or
invented is not a property of the source, it is a question about `data/sure.db`, which no
compiler can consult. So it is enforced by `scripts/pii-scan.mjs`, which `.githooks/pre-commit`
runs first (it costs milliseconds, and guards the one thing a later gate cannot undo):

1. **Shape** — regexes for NZ bank account numbers, IRD numbers (dashed, and undashed beside
   an `SLS` marker), `FC…` payee accounts, `CARD nnnn` last-fours, UUIDs baked in as shell
   defaults, long literals assigned to secret-looking names, JWTs, and email addresses. Only
   *added* lines of staged files are scanned, so pre-existing content never blocks a commit;
   `--all` sweeps the whole tree, and `--message FILE` scans one commit message (the
   `.githooks/commit-msg` mode — a message is history the file modes never look at).
2. **Allowlist** — `ALLOWED` in that script, baselined from a tree verified clean, so the
   established fakes in `seed.mjs`/`accounts.rs`/the ASB fixtures stay quiet. Adding a new
   invented literal there is the intended workflow, and the entry is the audit trail.
3. **Provenance** — anything not allowlisted is byte-grepped against `data/sure.db` (and its
   WAL), read-only, no sqlite handle. A hit means real data and says so with a count; no hit
   is still reported, because it may be a third party's data that was never in this database.

It found a real third-party account number in an `asb.rs` doc comment within a minute of
being written — one the manual scrub had missed, because that scrub grepped for the specific
numbers it already knew rather than for the shape.

The same number then survived in its *own* commit message until 2026-08-11, because for its
first week the script only ever read files: `--all` printed a clean tree while the commit
describing the find still quoted it verbatim, and that green tick is what disguised it. Hence
the `commit-msg` hook. The lesson generalises past this script — when checking whether a
literal is gone, scan objects rather than paths:
`git cat-file --batch-all-objects --batch | rg -a -F <literal>` covers blobs, commit messages,
and unreachable objects in one pass.

What it still can't see: a value paraphrased by a digit, a shape nobody has added a pattern
for, and anything rendered into an image (the Playwright baselines had to be regenerated, not
rewritten). It narrows rule 3 to the cases a regex can carry; the question "where did this
string come from?" is still the check that matters.
