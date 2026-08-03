#!/usr/bin/env node
//
// Scan for personal data that must not be committed — CLAUDE.md rule 3 ("Fixtures carry real
// data's shape, never its identifiers"), enforced.
//
// This catches *shapes*: account numbers, IRD numbers, card last-fours, secret-looking
// literals. A shape alone proves nothing — `scripts/seed.mjs` and the ASB/myIR fixtures are
// full of invented numbers that look exactly like real ones — so every match is then checked
// for provenance against `data/sure.db`, the live developer database. A literal that appears
// there came from real data and is a hard failure; one that does not is reported anyway,
// because it may be someone else's data that was never in this database (three of the values
// scrubbed on 2026-08-04 belonged to third parties).
//
// Confirmed-invented literals live in ALLOWED below, baselined from a tree verified clean.
// Adding to it is the intended way to introduce a new fake — the entry is the audit trail.
//
//   node scripts/pii-scan.mjs          # staged additions only (what the pre-commit hook runs)
//   node scripts/pii-scan.mjs --all    # every tracked text file
//
// Reads `data/sure.db` as bytes and never opens a sqlite handle, so it cannot write to the
// live database (see the `data/sure.db` convention in CLAUDE.md).

import { execFileSync } from "node:child_process";
import { existsSync, openSync, readSync, closeSync, statSync } from "node:fs";
import path from "node:path";

const repoRoot = execFileSync("git", ["rev-parse", "--show-toplevel"], {
  encoding: "utf8",
}).trim();
process.chdir(repoRoot);

// ---------------------------------------------------------------------------- what to look for

const PATTERNS = [
  {
    name: "nz-bank-account",
    what: "an NZ bank account number (bank-branch-account[-suffix])",
    re: /\b\d{2}-\d{4}-\d{7}(?:-\d{2,3})?\b/g,
  },
  {
    name: "ird-number",
    what: "an IRD number",
    re: /\b\d{3}-\d{3}-\d{3}\b/g,
  },
  {
    // The undashed spelling, as it arrives in an ASB direct-debit memo: `nnnnnnnnnSLS nnnnnnnnn`.
    name: "ird-undashed",
    what: "an IRD number beside an SLS (student-loan) marker",
    re: /\b(?:\d{9}\s?SLS|SLS\s?\d{9})\b/gi,
  },
  {
    name: "payee-account",
    what: "a bill-payment payee account number",
    re: /\bFC\d{2}-\d{4}-\d{7}-\d{2}\b/g,
  },
  {
    name: "card-last-four",
    what: "a payment-card last-four",
    re: /\bCARD\s\d{4}\b/g,
  },
  {
    // How the real Sharesies device key got committed: as a shell parameter default.
    name: "uuid-as-default",
    what: "a UUID baked in as a shell default (device/session identifiers are per-browser)",
    re: /:-\s*[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/gi,
  },
  {
    // Tolerates whatever sits between the name and the value — `=`, `: `, or a Rust type
    // annotation (`const TOKEN: &str = "…"`). Spaces can't appear inside the value, so
    // ordinary prose that merely mentions "secret:" cannot reach 32 characters and match.
    name: "secret-literal",
    what: "a long literal assigned to a secret-looking name",
    re: /(?:token|cookie|secret|passwo?rd|api[_-]?key|bearer|authorization)\b["']?[^\n"']{0,30}?["']?[A-Za-z0-9._/+-]{32,}/gi,
  },
  {
    // A JWT is recognisable on its own, with no nearby keyword to key off.
    name: "jwt",
    what: "a JWT",
    re: /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}/g,
  },
  {
    name: "email",
    what: "an email address",
    re: /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g,
  },
];

// Literals confirmed invented — each was checked against data/sure.db and returned zero hits.
// `12-3456-…` is the repo's long-standing fake bank/branch (scripts/seed.mjs, accounts.rs).
// The `12-3136-…` pair keeps a real *branch* prefix (a public ASB branch code) with invented
// account digits, because the asb.rs tests pin ASB's 12-character field widths and a
// length-changing replacement would stop exercising them — see CLAUDE.md rule 3.
const ALLOWED = new Set([
  "12-3456-0123456-00",
  "12-3456-0000000-00",
  "12-3456-0000001-51",
  "12-3456-0000002-51",
  "12-3456-0000123",
  "12-3456-0000456-00",
  "12-3136-0000123-50",
  "12-3136-0000123-51",
  "012-345-678",
  "FC02-1234-0000001-00",
  "FC12-3456-0000002-00",
  "CARD 1111",
  "CARD 1234",
  "CARD 2222",
  // As it appears in the direct-debit memo fixture: digits and marker run together, which is
  // the form the pattern's trailing \b actually settles on.
  "012345678SLS",
]);

/** Documentation domains and the like are never someone's address. */
const SAFE_EMAIL = /@(?:[\w-]+\.)*example\.(?:com|org|net)$|\.(?:test|invalid|local|localhost)$/i;

const SKIP_FILE = [
  /^pnpm-lock\.yaml$/,
  /^Cargo\.lock$/,
  // Binary: nothing to read as text, and a rendered PNG can't be grepped anyway (which is
  // exactly why the Playwright baselines had to be regenerated rather than rewritten).
  /\.(?:png|jpe?g|gif|webp|ico|pdf|zip|xlsx|db|db-wal|db-shm|woff2?|ttf|otf|wasm)$/i,
];

// ------------------------------------------------------------------------------- provenance

/** Byte-search the live developer database (and its WAL) for `needles`. */
function liveDbHits(needles) {
  const hits = new Map();
  const targets = ["data/sure.db", "data/sure.db-wal"].filter((f) => existsSync(f));
  if (targets.length === 0 || needles.length === 0) return null; // absent (e.g. CI): unknowable

  const bufs = needles.map((n) => ({ needle: n, buf: Buffer.from(n, "utf8") }));
  const overlap = Math.max(...bufs.map((b) => b.buf.length));
  const CHUNK = 4 << 20;

  for (const file of targets) {
    const size = statSync(file).size;
    if (size > 1 << 30) continue; // absurdly large; don't try
    const fd = openSync(file, "r");
    try {
      const buf = Buffer.allocUnsafe(CHUNK + overlap);
      let pos = 0;
      let carry = 0;
      while (pos < size) {
        const read = readSync(fd, buf, carry, CHUNK, pos);
        if (read <= 0) break;
        const view = buf.subarray(0, carry + read);
        for (const { needle, buf: nb } of bufs) {
          let at = 0;
          while ((at = view.indexOf(nb, at)) !== -1) {
            hits.set(needle, (hits.get(needle) ?? 0) + 1);
            at += 1;
          }
        }
        // Carry the tail so a needle straddling a chunk boundary is still found.
        carry = Math.min(overlap, view.length);
        view.subarray(view.length - carry).copy(buf, 0);
        pos += read;
      }
    } finally {
      closeSync(fd);
    }
  }
  return hits;
}

// ----------------------------------------------------------------------------- what to scan

const skip = (file) => SKIP_FILE.some((re) => re.test(file));

/** Every tracked text file, as `{ file, line, text }` rows. */
function wholeTree() {
  const files = execFileSync("git", ["ls-files", "-z"], { maxBuffer: 1 << 28 })
    .toString("utf8")
    .split("\0")
    .filter((f) => f && !skip(f));
  const rows = [];
  for (const file of files) {
    let content;
    try {
      content = execFileSync("git", ["show", `:${file}`], { maxBuffer: 1 << 28 });
    } catch {
      continue;
    }
    if (content.subarray(0, 8000).includes(0)) continue; // binary
    content
      .toString("utf8")
      .split("\n")
      .forEach((text, i) => rows.push({ file, line: i + 1, text }));
  }
  return rows;
}

/** Only the lines this commit *adds*, so pre-existing content never blocks a commit. */
function stagedAdditions() {
  const diff = execFileSync(
    "git",
    ["diff", "--cached", "-U0", "--no-color", "--diff-filter=ACMR"],
    { maxBuffer: 1 << 28 }
  ).toString("utf8");

  const rows = [];
  let file = null;
  let lineNo = 0;
  for (const raw of diff.split("\n")) {
    if (raw.startsWith("+++ ")) {
      const p = raw.slice(4);
      file = p === "/dev/null" ? null : p.replace(/^b\//, "");
      if (file && skip(file)) file = null;
    } else if (raw.startsWith("@@")) {
      const m = /^@@ -\S+ \+(\d+)/.exec(raw);
      lineNo = m ? Number(m[1]) : 0;
    } else if (file && raw.startsWith("+")) {
      rows.push({ file, line: lineNo, text: raw.slice(1) });
      lineNo += 1;
    }
  }
  return rows;
}

// ------------------------------------------------------------------------------------- scan

const all = process.argv.includes("--all");
const rows = all ? wholeTree() : stagedAdditions();

const findings = [];
for (const row of rows) {
  for (const { name, what, re } of PATTERNS) {
    re.lastIndex = 0;
    for (const m of row.text.matchAll(re)) {
      const literal = m[0].trim();
      if (ALLOWED.has(literal)) continue;
      if (name === "email" && SAFE_EMAIL.test(literal)) continue;
      findings.push({ ...row, name, what, literal });
    }
  }
}

if (findings.length === 0) {
  console.log(`✓ no personal-data shapes in ${all ? "the tree" : "staged changes"}`);
  process.exit(0);
}

const hits = liveDbHits([...new Set(findings.map((f) => f.literal))]);

console.error(
  `\n✗ possible personal data in ${all ? "the tree" : "staged changes"} ` +
    `(${findings.length} match${findings.length === 1 ? "" : "es"})\n`
);
for (const f of findings) {
  console.error(`  ${f.file}:${f.line}  [${f.name}]  ${f.literal}`);
  console.error(`      looks like ${f.what}`);
  if (hits === null) {
    console.error(`      ↳ data/sure.db not present — provenance unchecked`);
  } else if (hits.get(f.literal)) {
    console.error(
      `      ↳ FOUND in data/sure.db (${hits.get(f.literal)}×) — this is REAL data, not a fixture`
    );
  } else {
    console.error(
      `      ↳ not in data/sure.db — invented, or someone else's data that was never here`
    );
  }
}

console.error(`
CLAUDE.md rule 3: fixtures carry real data's shape, never its identifiers.

  * Replace it length-for-length with an invented value — the ASB/myIR tests pin
    ASB's 12-character field widths, so a longer or shorter stand-in silently stops
    exercising the boundary the test exists for.
  * If the literal is genuinely invented, add it to ALLOWED in ${path.relative(
    repoRoot,
    new URL(import.meta.url).pathname
  )}
    so the next commit is quiet and the decision is recorded.
  * Grep the whole tree before you finish: one value spreads into doc comments, the
    generated client schema, another crate's arithmetic, and rendered PNG baselines.

Last resort: git commit --no-verify
`);
process.exit(1);
