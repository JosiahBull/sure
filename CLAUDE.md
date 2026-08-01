# sure — agent/contributor rules

Sure is a local-first financial tracker: a Rust/Axum/SQLite backend and a Svelte SPA.
Rust workspace (`Cargo.toml`): `packages/core` (domain types, zero I/O) → `packages/dal`
(all `sqlx`/SQL, converts rows to core types) → `packages/app` (services/use-cases,
talks to the DAL only through `sure_app::ports` traits) → `packages/api` (Axum routes,
OpenAPI) → `packages/server` (binary: wires it together, HTTP transport concerns) —
plus `packages/providers` (bank/broker/price feed adapters) and `packages/scheduler`
(cron runner). pnpm workspace (`pnpm-workspace.yaml`): `packages/web` (the SPA),
`packages/client` (generated typed API client), `packages/api-tests` (Playwright
backend e2e suite).

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

## Conventions

- **Scripts** (`package.json`): `pnpm dev` runs API + web together; `pnpm build` runs
  `gen:client` then `cargo build --release` then the web build; `pnpm test` runs
  `test:api` (`@sure/api-tests`, Playwright-driven backend e2e) then `test:web`;
  `pnpm lint:rust` is `cargo clippy --all-targets -- -D warnings`; `pnpm fmt:rust` is
  `cargo fmt --all`.
- **Blocking-code detector** (development only): `pnpm dev:api:blocked` and
  `pnpm test:api:blocked` build with `sure-api`'s `blocking-detector` feature *and*
  `RUSTFLAGS="--cfg tokio_unstable"` — both are needed, the feature alone reports nothing —
  into `target/blocked/`, which adds `tokio-blocked`'s layer to the subscriber
  `telemetry::init_tracing` installs so a task that blocks its worker thread logs a WARN.
  It is never on in a normal or release build. Keep the `RUST_LOG` filter attached to the
  *output* layer rather than the registry: a registry-wide filter drops the TRACE-level
  `runtime.spawn` spans the detector reads, and the detector then silently sees nothing.
- **Pre-commit** (`.githooks/pre-commit`, wired by the `prepare` script): runs
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `pnpm test:api`, `pnpm --filter @sure/web check`. It deliberately skips the web
  *visual* Playwright suite (only deterministic in CI's pinned container). Bypass in
  an emergency with `git commit --no-verify`, not by weakening a lint.
- **Generated client**: `packages/client/src/schema.d.ts` is generated by
  `pnpm gen:client` (openapi-typescript over `packages/client/openapi.json`, itself
  generated by `pnpm gen:openapi` from `sure-api`'s utoipa schema). It is tracked in
  git and carries `// This file was auto-generated ... Do not make direct changes` —
  regenerate it, never hand-edit it.
- **Migrations** (`packages/dal/migrations/`, embedded via
  `sqlx::migrate!("./migrations")` in `packages/dal/src/lib.rs`): append-only. sqlx
  records each applied migration's checksum in `_sqlx_migrations`; editing a migration
  that has already run anywhere breaks that checksum check on next connect. A schema
  change is always a new numbered file.
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
# each member crate's Cargo.toml (packages/core, dal, app, api, server, providers, scheduler)
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
