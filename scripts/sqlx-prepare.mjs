#!/usr/bin/env node
//
// Regenerate (or verify) the `.sqlx/` offline query metadata that `sqlx::query!` and friends
// are checked against at compile time.
//
//   node scripts/sqlx-prepare.mjs           # regenerate .sqlx/ (pnpm sqlx:prepare)
//   node scripts/sqlx-prepare.mjs --check   # fail if .sqlx/ is stale (pnpm sqlx:check)
//
// The point of the script — rather than calling `cargo sqlx prepare` directly — is the
// database it points at. `cargo sqlx prepare` needs a live schema to describe queries
// against, and the default `DATABASE_URL` is `sqlite:data/sure.db`, which is real financial
// data (CLAUDE.md: nothing in tooling may write to it, and merely opening a WAL database can
// recover and checkpoint it). So this builds a throwaway database under `target/`, applies
// the embedded migrations to it, and describes against that. The schema is therefore exactly
// what `packages/dal/migrations` produces, which is also what the app and the e2e suite run —
// not whatever a developer's working database has drifted into.
//
// SQLX_OFFLINE is forced to `false` for the child `cargo` so the macros describe against that
// database instead of the metadata they are in the middle of producing. It wins over
// `.cargo/config.toml`'s `[env] SQLX_OFFLINE = "true"`, because cargo's `[env]` defers to an
// already-set variable unless the entry says `force = true`.
//
// `--check` is what .githooks/pre-commit runs: it exits non-zero when a query was edited
// without regenerating, which would otherwise only surface as a confusing compile error on a
// machine that has no database.

import { spawnSync } from "node:child_process";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const repoRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();

const check = process.argv.includes("--check");

// Under target/ so `cargo clean` and .gitignore's `/target` both already cover it, and so it
// can never be confused with data/sure.db. Rebuilt from scratch every run: a stale schema here
// would silently verify queries against migrations that no longer exist.
const dbDir = path.join(repoRoot, "target", "sqlx-prepare");
const dbPath = path.join(dbDir, "schema.db");
const databaseUrl = `sqlite:${dbPath}`;

function run(cmd, args, extraEnv = {}) {
  const res = spawnSync(cmd, args, {
    cwd: repoRoot,
    stdio: "inherit",
    // --no-dotenv on the sqlx calls and an explicit DATABASE_URL here: neither may inherit a
    // developer's .env, which is exactly where a pointer at the live database would come from.
    env: { ...process.env, DATABASE_URL: databaseUrl, ...extraEnv },
  });
  if (res.error) throw res.error;
  return res.status ?? 1;
}

fs.rmSync(dbDir, { recursive: true, force: true });
fs.mkdirSync(dbDir, { recursive: true });

console.log(`▶ building throwaway schema database at ${path.relative(repoRoot, dbPath)}`);
let status = run("sqlx", ["database", "create", "--no-dotenv", "--database-url", databaseUrl]);
if (status !== 0) process.exit(status);

status = run("sqlx", [
  "migrate",
  "run",
  "--no-dotenv",
  "--database-url",
  databaseUrl,
  "--source",
  "packages/dal/migrations",
]);
if (status !== 0) process.exit(status);

// `--workspace` puts a single `.sqlx/` at the repo root rather than one per crate. The
// trailing cargo args mirror the clippy/test gates so feature-gated and test-only queries are
// described too — a `#[cfg(test)]` query missing from the metadata fails the build in CI.
console.log(`▶ cargo sqlx prepare${check ? " --check" : ""}`);
status = run(
  "cargo",
  [
    "sqlx",
    "prepare",
    "--workspace",
    "--no-dotenv",
    ...(check ? ["--check"] : []),
    "--",
    "--all-targets",
    "--all-features",
  ],
  { SQLX_OFFLINE: "false" },
);

if (status !== 0) {
  if (check) {
    console.error(
      "\n✗ .sqlx/ is out of date. Run `pnpm sqlx:prepare` and commit the result.\n" +
        "  (Every sqlx::query!/query_as!/query_scalar! invocation needs cached metadata, so a\n" +
        "   query or migration change means regenerating it.)",
    );
  }
  process.exit(status);
}

console.log(check ? "✓ .sqlx/ is up to date" : "✓ .sqlx/ regenerated");
