#!/usr/bin/env node
//
// Assert every dependency a workspace member declares is inherited from the root
// `[workspace.dependencies]` table — the version *and* the feature set.
//
// One table, one answer. Cargo unifies features across a build, so a feature enabled by one
// member is enabled for every other member that links the same crate: `features = [..]` written
// at a member is not a local choice, it is a workspace-wide one made somewhere nobody thinks to
// look. A version written at a member is worse, because it can differ — two requirements that
// resolve to different majors build two copies of the crate, and the types of one are not the
// types of the other. `sure-providers` carried exactly that until the workspace moved reqwest to
// 0.13: a second, renamed copy (`reqwest-akahu`) whose `ClientBuilder` was a different type, so
// `http.rs` had two byte-identical builders it could not merge. The root manifest is also where
// this tree keeps the *reasoning* — why `zip` is on 8 rather than 2, why `reqwest` names `rustls`
// with no defaults — and a requirement in a member manifest is a requirement with no comment
// next to the four paragraphs explaining the others.
//
// What this checks, over `packages/*/Cargo.toml` (every dependency table: `[dependencies]`,
// `[dev-dependencies]`, `[build-dependencies]`, and the `[target.'cfg(..)'.dependencies]` form):
//
//   1. every entry inherits — `foo = { workspace = true }`, never a version, `path` or `git`;
//   2. no entry carries `features` / `default-features`, which belong beside the requirement;
//   3. the name it inherits actually exists in the root table;
//   4. no key beyond `workspace`, `optional` and (for a workspace-internal crate) `features`.
//
// The exception in 4 is deliberate and narrow: `sure-core`'s `axum` and `sqlx` features are
// per-consumer gates — the DAL turns on the `sqlx` one, `sure-api` the `axum` one, and that is
// what keeps the web stack out of the crates that don't talk HTTP. A feature of *our* crate,
// selected by the member that needs it, is the mechanism working; a feature of somebody else's
// crate, selected in one member on behalf of all of them, is the thing this script exists to
// stop. Anything genuinely outside both cases takes a waiver comment on the line above:
//
//   # workspace-deps-allow: <why>
//
// which is the same friction CLAUDE.md rule 2 puts on a wildcard match arm — allowed, but written
// down and greppable. Honoured waivers are printed on every run, so a stale one is visible.
//
// The fixer is `cargo autoinherit` (crates.io, `cargo install cargo-autoinherit`): it hoists a
// member's requirement into the root table and rewrites the member to inherit it, leaving the
// surrounding comments alone. It does not hoist a *feature set* — that is judgement about which
// features the workspace wants, and it is the half of this rule a tool cannot make for you — so
// run it, then move the `features = [..]` up by hand with a note saying what they buy. This
// script is what CI runs, because it is pure text parsing: no cargo, no network, no lockfile,
// milliseconds, and no toolchain to install on the runner.
//
//   node scripts/check-workspace-deps.mjs
//
// Part of the `workspace-deps` job in .github/workflows/checks.yml and of .githooks/pre-commit.

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";

const repoRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();
process.chdir(repoRoot);

const ROOT_MANIFEST = "Cargo.toml";
const GHA = Boolean(process.env.GITHUB_ACTIONS);
const WAIVER = /^workspace-deps-allow:\s*(\S.*)$/;

// A key a member may legitimately set on an inherited dependency. `optional` has to be here:
// it is what makes a dependency a feature of *that* crate (`sure-core`'s gated `sqlx`/`axum`),
// and the root table has no way to express it.
const MEMBER_KEYS = new Set(["workspace", "optional"]);

if (process.argv.length > 2) {
  const arg = process.argv[2];
  if (arg === "-h" || arg === "--help") {
    // Print the header block above, uncommented.
    const self = readFileSync(new URL(import.meta.url), "utf8");
    const header = self.split("\n").slice(1);
    for (const line of header) {
      if (!line.startsWith("//")) break;
      console.log(line.replace(/^\/\/ ?/, ""));
    }
    process.exit(0);
  }
  fail(`unknown argument '${arg}' (try --help)`);
}

function err(msg) {
  // Workflow commands are read off stdout, so this is deliberately not stderr.
  console.log(GHA ? `::error::${msg}` : `error: ${msg}`);
}

function fail(msg) {
  err(msg);
  process.exit(1);
}

// ------------------------------------------------------------------------------------ parsing

// A dependency table in any of its spellings, including the `[dependencies.foo]` sub-table
// (group 1), where the dependency's name is the table's rather than a key's.
const DEP_TABLE = /(?:^|\.)(?:dev-|build-)?dependencies(?:\.([A-Za-z0-9_-]+))?$/;

/** Drop a trailing `# comment`, leaving anything inside a quoted string alone. */
function stripComment(line) {
  let quoted = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '"') quoted = !quoted;
    else if (c === "#" && !quoted) return line.slice(0, i);
  }
  return line;
}

/** True while an inline table or array is still open, so the entry continues on the next line. */
function unbalanced(text) {
  let depth = 0;
  for (const line of text.split("\n")) {
    for (const c of stripComment(line)) {
      if (c === "{" || c === "[") depth++;
      else if (c === "}" || c === "]") depth--;
    }
  }
  return depth > 0;
}

/**
 * Every dependency entry in one manifest.
 *
 * Returns `{ name, key, value, line, table, waiver }` per entry, where `key` is set only for
 * the dotted form (`foo.workspace = true`) and `waiver` is the reason from a
 * `# workspace-deps-allow:` comment in the block immediately above it.
 */
function dependencyEntries(file) {
  const lines = readFileSync(file, "utf8").split("\n");
  const found = [];
  let table = null;
  let subtable = null;
  let comments = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const s = line.trim();

    if (s === "") {
      comments = [];
      continue;
    }
    if (s.startsWith("#")) {
      comments.push(s.replace(/^#\s?/, ""));
      continue;
    }
    if (s.startsWith("[") && s.endsWith("]")) {
      const header = s.slice(1, -1).trim();
      const m = DEP_TABLE.exec(header);
      table = m ? header : null;
      subtable = m ? (m[1] ?? null) : null;
      // A `[dependencies.foo]` sub-table is one entry whose keys are the lines below it. Gather
      // them so the same checks apply, rather than seeing `version`/`features` as entries.
      if (subtable) {
        const body = [];
        let j = i + 1;
        for (; j < lines.length; j++) {
          const t = lines[j].trim();
          if (t.startsWith("[") && t.endsWith("]")) break;
          if (t !== "" && !t.startsWith("#")) body.push(t);
        }
        found.push({
          name: subtable,
          key: null,
          value: `{ ${body.join(", ")} }`,
          line: i + 1,
          table: header,
          waiver: waiverIn(comments),
        });
        comments = [];
        i = j - 1;
        table = null;
        subtable = null;
      }
      continue;
    }
    if (!table) {
      comments = [];
      continue;
    }

    const eq = stripComment(s).indexOf("=");
    if (eq === -1) {
      comments = [];
      continue;
    }
    let value = s.slice(eq + 1).trim();
    let j = i;
    while (unbalanced(value) && j + 1 < lines.length) {
      j++;
      value += `\n${lines[j]}`;
    }
    const lhs = s.slice(0, eq).trim();
    const dot = lhs.indexOf(".");
    found.push({
      name: dot === -1 ? lhs : lhs.slice(0, dot),
      key: dot === -1 ? null : lhs.slice(dot + 1),
      value,
      line: i + 1,
      table,
      waiver: waiverIn(comments),
    });
    if (dot === -1) comments = [];
    i = j;
  }
  return found;
}

function waiverIn(comments) {
  for (const c of comments) {
    const m = WAIVER.exec(c);
    if (m) return m[1];
  }
  return null;
}

/** The keys of an inline table, e.g. `{ workspace = true, features = ["x"] }`. */
function inlineKeys(value) {
  if (!value.startsWith("{")) return null;
  return [...value.matchAll(/([A-Za-z0-9_-]+)\s*=/g)].map((m) => m[1]);
}

// -------------------------------------------------------------------------------- the manifests

const members = readdirSync("packages", { withFileTypes: true })
  .filter((e) => e.isDirectory())
  .map((e) => path.join("packages", e.name, "Cargo.toml"))
  .filter((p) => {
    try {
      readFileSync(p);
      return true;
    } catch {
      return false; // a pnpm-only package (web, client, api-tests)
    }
  })
  .sort();

if (members.length === 0) {
  fail(`found no member manifests under packages/ — the glob is broken`);
}

// The root table, which is what a member inherits *from*. A `path = ` entry is a crate in this
// workspace, and those are the only ones a member may add features to (see the header).
const rootDeps = new Map();
for (const entry of dependencyEntries(ROOT_MANIFEST)) {
  if (!entry.table.startsWith("workspace.")) continue;
  rootDeps.set(entry.name, { internal: /\bpath\s*=/.test(entry.value) });
}
if (rootDeps.size === 0) {
  fail(`parsed no entries from [workspace.dependencies] in ${ROOT_MANIFEST} — the parser is broken`);
}

// ------------------------------------------------------------------------------------- the check

const problems = [];
const waived = [];
let checked = 0;

for (const file of members) {
  for (const entry of dependencyEntries(file)) {
    checked++;
    const where = `${file}:${entry.line}`;
    const { name, key, value } = entry;
    const keys = key ? [key] : (inlineKeys(value) ?? []);
    const inherited = key
      ? key === "workspace" && value.trim() === "true"
      : keys.includes("workspace") && /\bworkspace\s*=\s*true\b/.test(value);
    const internal = rootDeps.get(name)?.internal ?? false;

    const found = [];
    if (!inherited) {
      const how = value.startsWith('"')
        ? `a bare version (${value})`
        : /\bgit\s*=/.test(value)
          ? "a git source"
          : /\bpath\s*=/.test(value)
            ? "a path source"
            : "its own requirement";
      found.push(
        `${name} declares ${how} instead of inheriting it — move the requirement to ` +
          `[workspace.dependencies] in ${ROOT_MANIFEST} and write \`${name} = { workspace = true }\` here`
      );
    } else if (!rootDeps.has(name)) {
      found.push(
        `${name} inherits from [workspace.dependencies], which does not declare it — ` +
          `add it to ${ROOT_MANIFEST}`
      );
    }

    for (const k of keys) {
      if (MEMBER_KEYS.has(k)) continue;
      if ((k === "features" || k === "default-features") && inherited) {
        if (internal) continue; // a feature of one of our own crates, gated per consumer
        found.push(
          `${name} sets \`${k}\` here — cargo unifies features across the build, so this is a ` +
            `workspace-wide choice made in one member. Move it onto the ${name} entry in ` +
            `${ROOT_MANIFEST}, with a note saying what it buys`
        );
        continue;
      }
      if (!inherited) continue; // already reported above; its version/path keys are the finding
      found.push(
        `${name} sets \`${k}\`, which belongs on the ${ROOT_MANIFEST} entry — only ` +
          `${[...MEMBER_KEYS].join(", ")} (and a workspace crate's own features) are per-member`
      );
    }

    if (found.length === 0) continue;
    if (entry.waiver) {
      waived.push(`${where}: ${name} — ${entry.waiver}`);
      continue;
    }
    for (const f of found) problems.push({ where, message: f });
  }
}

if (checked === 0) {
  fail(`parsed no dependency entries at all across ${members.length} manifests — the parser is broken`);
}

for (const { where, message } of problems) err(`${where}: ${message}`);

if (waived.length > 0) {
  console.log(`note: ${waived.length} waived by a workspace-deps-allow comment:`);
  for (const w of waived) console.log(`  ${w}`);
}

if (problems.length > 0) {
  console.log("");
  console.log(`One table, one answer: every requirement and every feature flag lives in`);
  console.log(`${ROOT_MANIFEST}, and a member says only that it wants the crate. A feature set`);
  console.log(`written in one member applies to the whole build; a version written in one member`);
  console.log(`can resolve to a second copy of the crate whose types are not the first's.`);
  console.log("");
  console.log(`  cargo install cargo-autoinherit   # once`);
  console.log(`  cargo autoinherit                 # hoists the requirements, keeps the comments`);
  console.log("");
  console.log(`It will not move a \`features = [..]\` for you — that is a decision about what the`);
  console.log(`workspace wants — so move those by hand. A genuine exception takes a comment on the`);
  console.log(`line above the entry:  # workspace-deps-allow: <why>`);
  process.exit(1);
}

console.log(
  `OK: ${checked} dependency entries across ${members.length} member manifests, ` +
    `all inherited from [workspace.dependencies] (${rootDeps.size} entries)`
);
