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
// Recorded HTTP snapshots (`*.ndjson`) need a decode before any of that applies: they are
// text, so they sail past the binary check in `wholeTree`, but their request and response
// bodies are base64 and every pattern below matches nothing against base64. See
// `expandSnapshots` — and `PRIVATE_UPSTREAMS` for the two upstreams whose recordings are not
// committable at all.
//
// A commit *message* is history too, and the two file modes below never see one: both read
// blobs. That gap was not theoretical — the commit that introduced this script quoted, in its
// own message, the third-party account number it had just found in an `asb.rs` doc comment, and
// the 2026-08-04 scrub missed it because `--replace-text` rewrites blobs while messages need
// `--replace-message`. It survived until 2026-08-11, the day before the first push to GitHub.
// Hence `--message`, run by `.githooks/commit-msg` — see `commitMessage` for what git itself
// strips before storing, which this has to strip too or it reports on text nobody committed.
//
//   node scripts/pii-scan.mjs                  # staged additions only (the pre-commit hook)
//   node scripts/pii-scan.mjs --all            # every tracked text file
//   node scripts/pii-scan.mjs --message FILE   # one commit message (the commit-msg hook)
//
// Reads `data/sure.db` as bytes and never opens a sqlite handle, so it cannot write to the
// live database (see the `data/sure.db` convention in CLAUDE.md).

import { execFileSync } from "node:child_process";
import { existsSync, openSync, readSync, closeSync, statSync, readFileSync } from "node:fs";
import path from "node:path";
import { gunzipSync, inflateSync } from "node:zlib";

// Resolved before the chdir below, because `--message` takes a path from git — relative to the
// worktree root, which is where git runs hooks from — and chdir would silently re-anchor it.
const invokedFrom = process.cwd();

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
  // The import pipeline's own examples: `sure_app::import`'s routing tests and doc comments, the
  // `assign` query parameter's format in `routes::import` (which reaches the generated client
  // through its OpenAPI description, and is exactly the spread rule 3 warns about — invented, so
  // harmless, but the entry is why nobody has to work that out twice), and docs/IMPORT.md.
  // Same digits as the `12-3456-…` bank fake CLAUDE.md rule 3 points at, with ASB's two suffixes.
  "12-3456-0000123-50",
  "12-3456-0000123-51",
  // The `-92` sibling a loan facility posts its interest and principal to, in the
  // `LOAN INT`/`LOAN PRIN` fixtures. Same invented account digits, new suffix.
  "12-3136-0000123-92",
  // The joint-account fixtures: one number two logins both report (`sure_app::sync`'s survey
  // tests, provider-linking.spec.ts, akahu.spec.ts), and the single-login control that has to
  // stay unpaired beside it — a test asserting "these two are one account" says nothing unless
  // something in the same response is left alone. Same `12-3456-…` fake bank/branch as above.
  "12-3456-0000123-00",
  "12-3456-0000999-00",
  "012-345-678",
  // The second borrower, for the tests that route two student loans in one household apart
  // (`routing::match_by_holder`, and the only-candidate tier declining when a two-item upload
  // names someone else's loan). Those tests say nothing with one IRD number, and this is the
  // digits of the established fake above reversed — invented, and deliberately not a near-miss
  // of it, so a grep for either cannot pick up the other.
  "098-765-432",
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

/** Declared here rather than at the scan below, because `scannedPaths` reads it. */
const all = process.argv.includes("--all");

/** Absolute path of the commit message to scan, or `null` in the two file modes. */
const messageFile = (() => {
  const at = process.argv.indexOf("--message");
  if (at === -1) return null;
  const given = process.argv[at + 1];
  if (!given || given.startsWith("--")) {
    console.error("pii-scan: --message needs a file path (git passes it to the commit-msg hook)");
    process.exit(2);
  }
  return path.resolve(invokedFrom, given);
})();

/** What this run is answerable for, for the one-line verdict and the failure header. */
const scope = all ? "the tree" : messageFile ? "the commit message" : "staged changes";

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

/**
 * One commit message, reduced to what git will actually store.
 *
 * Two things have to come off first, or this reports on text that never becomes part of any
 * commit — and a gate that cries wolf is a gate people start bypassing:
 *
 *   * Comment lines. Git strips them, and the default template is full of `git status` output:
 *     branch names, paths, and — after `git commit -s`, or an amend — an author line carrying
 *     an email address, which the `email` pattern below would otherwise fire on every time.
 *   * Everything from the scissors line. `commit.verbose` / `-v` appends the whole staged diff
 *     under it; git drops it, and the pre-commit scan has already read exactly those lines.
 *
 * Line numbers stay those of the file on disk, so a reported line is one the author can find in
 * their editor rather than an index into some post-filter buffer.
 */
function commitMessage(file) {
  let raw;
  try {
    raw = readFileSync(file, "utf8");
  } catch (e) {
    console.error(`pii-scan: cannot read the commit message at ${file}: ${e.message}`);
    process.exit(2);
  }

  // `core.commentChar` may be a single character or `auto`, in which case git picks the first
  // candidate the message does not already use at line start — `#` unless the message is
  // unusual, and guessing wrong here only ever means scanning a line git will discard.
  let commentChar = "#";
  try {
    const configured = execFileSync("git", ["config", "--get", "core.commentChar"], {
      encoding: "utf8",
    }).trim();
    if (configured && configured !== "auto") commentChar = configured;
  } catch {
    // Unset: `git config --get` exits non-zero, and `#` is already the default.
  }
  const isScissors = (text) => text.startsWith(commentChar) && /-{2,}\s*>8\s*-{2,}/.test(text);

  const rows = [];
  const lines = raw.split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const text = lines[i];
    if (isScissors(text)) break;
    if (text.startsWith(commentChar)) continue;
    rows.push({ file: "(commit message)", line: i + 1, text });
  }
  return rows;
}

/**
 * The paths this run is answerable for, names only.
 *
 * Separate from the row sources above because the Akahu guard is a judgement about a *path*,
 * and a path can be disqualifying while contributing no rows to scan: an empty file has no
 * added lines, and a pure rename has none either. Costs one extra `git` call in the
 * milliseconds this whole script is budgeted for.
 */
function scannedPaths() {
  const args = all
    ? ["ls-files", "-z"]
    : ["diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR"];
  return execFileSync("git", args, { maxBuffer: 1 << 28 })
    .toString("utf8")
    .split("\0")
    .filter(Boolean);
}

// ------------------------------------------------------- inside recorded HTTP snapshots

const NDJSON_FILE = /\.ndjson$/i;

/**
 * The upstreams whose recordings must never be committed, by `Upstream::name()`.
 *
 * Two of the four are public market data — a Frankfurter rate table and a Yahoo close series
 * say nothing about whose money it is — so those snapshots *may* be recorded and committed, and
 * are (`packages/providers/tests/snapshots/`). That is what makes this guard load-bearing rather
 * than theoretical: the recorder that wrote those two is one `--upstream` away from writing one
 * of these.
 *
 * For both of these the traffic *is* the personal data, so the policy is categorical rather
 * than per-literal: the fixture is hand-authored with invented identifiers, and a real
 * recording is a local developer tool that never lands.
 *
 * - **akahu** — real account numbers, balances, transaction memos and payee names, in bulk. No
 *   scrub gets them back out once they are in history; the last one cost a 58-commit rewrite.
 * - **house_pricer** — a dossier on one dwelling: street address, GPS centroid, title boundary
 *   polygon, legal description, land and improvement values. It is not market data about a
 *   security, it is *where somebody lives*, and a single recorded exchange carries the lot.
 *
 * `.gitignore` covers the directories; this covers the `git add -f` that walks past them, and
 * the recording dropped somewhere an ignore rule does not name.
 */
const PRIVATE_UPSTREAMS = ["akahu", "house_pricer"];

/**
 * Whether a path is one of those recordings, by name.
 *
 * Two spellings per upstream, because there are two ways a recording gets its name. A
 * per-upstream *directory* is the layout `.gitignore` names. But `sure_testproxy` names snapshot
 * files from `Upstream::name()` — `<name>.ndjson`, side by side — which is the layout the
 * committed captures actually use, and where a recorder pointed at every upstream would drop
 * these beside the public ones. The content check below would catch such a file once it had a
 * line in it; it would not catch an *empty* one, which is exactly what a credential-less
 * recording run leaves behind.
 */
const PRIVATE_SNAPSHOT_PATH = new RegExp(
  `(?:^|/)(?:snapshots/(?:${PRIVATE_UPSTREAMS.join("|")})/[^/]*|(?:${PRIVATE_UPSTREAMS.join("|")}))\\.ndjson$`,
  "i",
);

/** As `serde_json` writes it, plus tolerance for a re-serialiser that adds spaces. */
const PRIVATE_UPSTREAM_FIELD = new RegExp(
  `"upstream"\\s*:\\s*"(?:${PRIVATE_UPSTREAMS.join("|")})"`,
  "i",
);

/**
 * Ceiling on what one *decompression* may produce, which is the only step here whose output
 * is not bounded by its input: a kilobyte of gzip expands to gigabytes, and this is the
 * cheapest gate in the pre-commit hook precisely because nothing in it is allowed to take
 * long. Well clear of anything a legitimate fixture holds — `sure_providers::http`'s
 * `MAX_BODY_BYTES` refuses to buffer more than 8MiB in the first place, so a body that hits
 * this never came from one of our own clients.
 */
const MAX_DECODED_BYTES = 64 << 20;

/**
 * Expand each `*.ndjson` row into itself plus a row per decoded body, and note which files
 * turn out to be recordings of a [`PRIVATE_UPSTREAMS`] host.
 *
 * The raw row is always kept, and kept first: header values, the URI, `labels` and anything
 * else stored as plain text are already scannable as JSON, and a body that turns out not to
 * be base64 after all is still covered that way. The decoded rows only ever *add* reach.
 *
 * Every decoded row inherits the NDJSON line number of the exchange it came out of, which is
 * the only line number that means anything here — one line is one whole request/response
 * pair, so "line 7" is a place a reviewer can actually look. It carries `inside` as well, so
 * the report can warn that grepping the file for the literal will come up empty; without
 * that, a reviewer checking the finding by hand concludes it was a false positive.
 */
function expandSnapshots(rows) {
  const privateFiles = new Map();
  /** First reason wins: the path is the more actionable one to report when both apply. */
  const flagPrivate = (file, why) => {
    if (!privateFiles.has(file)) privateFiles.set(file, why);
  };
  for (const file of scannedPaths()) {
    // The two spellings PRIVATE_SNAPSHOT_PATH covers report differently, because the fix
    // differs: a directory is a whole recording tree to move, a bare `<name>.ndjson` is one file
    // an all-upstreams recorder dropped beside the public captures.
    const match = PRIVATE_SNAPSHOT_PATH.exec(file);
    if (match) {
      const bare = PRIVATE_UPSTREAMS.find(
        (name) =>
          new RegExp(`(?:^|/)${name}\\.ndjson$`, "i").test(file) &&
          !new RegExp(`/${name}/`, "i").test(file),
      );
      flagPrivate(
        file,
        bare
          ? `is a ${bare} snapshot file (named from Upstream::name())`
          : `sits under a snapshots/<upstream>/ path for a feed that may not be recorded`,
      );
    }
  }

  const out = [];
  for (const row of rows) {
    out.push(row);
    if (!NDJSON_FILE.test(row.file) || row.text.trim() === "") continue;

    let exchange;
    try {
      exchange = JSON.parse(row.text);
    } catch {
      // Fail soft, never throw: a gate that crashes is a gate that gets bypassed, and the
      // raw line is already in `out` so nothing goes unscanned — only the decode is lost.
      // A truncated line must still trip the Akahu guard, hence the textual fallback.
      if (PRIVATE_UPSTREAM_FIELD.test(row.text)) {
        flagPrivate(row.file, `line ${row.line} records a feed that may not be recorded`);
      }
      continue;
    }
    if (exchange === null || typeof exchange !== "object") continue;
    if (PRIVATE_UPSTREAMS.includes(exchange.upstream)) {
      flagPrivate(row.file, `line ${row.line} records ${exchange.upstream}`);
    }

    // `outcome` is internally tagged: only `kind: "response"` carries a body at all, an
    // error outcome is a message. Both `?.` chains guard a hand-edited fixture that is
    // valid JSON but not a valid exchange.
    const bodies = [
      ["request", exchange.request?.body],
      ["response", exchange.outcome?.kind === "response" ? exchange.outcome.body : undefined],
    ];
    for (const [where, encoded] of bodies) {
      const text = decodeBody(encoded);
      if (text === null) continue;
      out.push({
        file: row.file,
        line: row.line,
        text,
        inside: `the base64 ${where} body`,
        of: exchangeLabel(exchange),
      });
    }
  }
  return { rows: out, privateFiles };
}

/** `METHOD /path` when the exchange has one, for orienting a reader inside a long file. */
function exchangeLabel(exchange) {
  const method = exchange.request?.method;
  const uri = exchange.request?.uri;
  return typeof method === "string" && typeof uri === "string" ? `${method} ${uri}` : null;
}

/**
 * Base64 → text, `null` for anything that isn't a body worth scanning.
 *
 * `Buffer.from(s, "base64")` never throws — it skips characters outside the alphabet — so
 * there is no validity check to make here and no error path to take. A field that wasn't
 * base64 decodes to noise no pattern matches, which is harmless: the raw JSON line carrying
 * that same field verbatim is scanned regardless.
 */
function decodeBody(encoded) {
  if (typeof encoded !== "string" || encoded === "") return null;
  const bytes = Buffer.from(encoded, "base64");
  if (bytes.length === 0) return null;
  return decompressed(bytes).toString("utf8");
}

/**
 * Undo transport compression, recognised by magic bytes rather than by the recorded
 * `content-encoding` header — the header is a claim, the magic is the payload.
 *
 * Not currently reachable: the workspace builds `reqwest` with `default-features = false`
 * and no `gzip`, so our clients never send `Accept-Encoding` and no upstream we record has
 * reason to compress. `partly-proxy` has no content-encoding handling of any kind (grepped:
 * zero hits), so it stores whatever bytes crossed the wire — which means the day someone
 * adds `"gzip"` to that feature list, or records a fixture with `curl`, every body in the
 * snapshot becomes opaque to the patterns above and this gate goes quiet without saying so.
 * Ten lines to make that a non-event.
 */
function decompressed(bytes) {
  const options = { maxOutputLength: MAX_DECODED_BYTES };
  try {
    if (bytes[0] === 0x1f && bytes[1] === 0x8b) return gunzipSync(bytes, options);
    // zlib-wrapped deflate: low nibble 8 is the only defined compression method, and the
    // two-byte header is a multiple of 31.
    if ((bytes[0] & 0x0f) === 0x08 && ((bytes[0] << 8) | bytes[1]) % 31 === 0) {
      return inflateSync(bytes, options);
    }
  } catch {
    // Truncated, over the ceiling, or a coincidental magic-byte match on plain content.
    // Scanning the bytes as they are beats scanning nothing.
  }
  return bytes;
}

// ------------------------------------------------------------------------------------- scan

// Snapshot expansion is a file-mode concern: it decodes `*.ndjson` bodies and judges staged
// *paths* for the never-record guard, and a message has neither. Skipping it also skips the
// `git diff` it runs, which is most of why the commit-msg hook costs a file read and nothing
// else.
const { rows, privateFiles } = messageFile
  ? { rows: commitMessage(messageFile), privateFiles: new Map() }
  : expandSnapshots(all ? wholeTree() : stagedAdditions());

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

// Reported first, and separately: this one does not go through ALLOWED or the provenance
// grep, because the failure is not "that literal might be real" but "this file cannot be in
// this repository at all" — a hand-authored fixture with invented identifiers has nothing to
// record and so never reaches here.
if (privateFiles.size > 0) {
  console.error(`\n✗ recorded traffic from a private upstream cannot be committed\n`);
  for (const [file, why] of privateFiles) console.error(`  ${file}  (${why})`);
  console.error(`
Frankfurter and Yahoo snapshots are public market data and may be committed. These may not:

  * akahu — real account numbers, balances, transaction memos and payee names. No scrub gets
    them back out once they are in history; the last one cost a 58-commit rewrite.
  * house_pricer — a street address, GPS centroid, title boundary polygon, legal description
    and land value. One exchange is a dossier on where somebody lives.

  * Record either locally if you need to, under packages/api-tests/snapshots/<upstream>/, which
    .gitignore already excludes. Do not \`git add -f\` it.
  * A committed fixture for these is hand-authored with invented identifiers. The proxy records
    a stub-served exchange (see packages/providers/tests/proxy_contract.rs), so a fixture
    can be built that way without an upstream ever being contacted.
`);
}

if (findings.length === 0) {
  if (privateFiles.size === 0) {
    console.log(`✓ no personal-data shapes in ${scope}`);
    process.exit(0);
  }
  process.exit(1);
}

const hits = liveDbHits([...new Set(findings.map((f) => f.literal))]);

console.error(
  `\n✗ possible personal data in ${scope} ` +
    `(${findings.length} match${findings.length === 1 ? "" : "es"})\n`
);
for (const f of findings) {
  console.error(`  ${f.file}:${f.line}  [${f.name}]  ${f.literal}`);
  console.error(`      looks like ${f.what}`);
  if (f.inside) {
    // Said before the provenance verdict, because it changes how the finding is checked: a
    // reviewer who greps the file for this literal will not find it, and would otherwise
    // write the whole thing off as a false positive.
    const where = f.of ? `${f.inside} of ${f.of}` : f.inside;
    console.error(`      ↳ inside ${where} on that line — decoded, so not greppable as text`);
  }
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

const selfPath = path.relative(repoRoot, new URL(import.meta.url).pathname);

console.error(
  messageFile
    ? `
CLAUDE.md rule 3 reaches commit messages: a message is history on the same terms a fixture is,
and rewriting one costs the same filter-repo run — with --replace-message rather than
--replace-text, which is precisely the half the 2026-08-04 scrub left out. Nothing has been
committed yet, so this is the cheapest moment this will ever be fixable.

  * Reword it. Naming the shape — "a third party's account number" — says everything the
    message needed to say; quoting the value is the part that becomes history.
  * Your text is not lost. Git kept it, so recover and edit it with
      git commit -e -F .git/COMMIT_EDITMSG
  * If the literal is genuinely invented, add it to ALLOWED in ${selfPath}
    so the next commit is quiet and the decision is recorded.

Last resort: git commit --no-verify
`
    : `
CLAUDE.md rule 3: fixtures carry real data's shape, never its identifiers.

  * Replace it length-for-length with an invented value — the ASB/myIR tests pin
    ASB's 12-character field widths, so a longer or shorter stand-in silently stops
    exercising the boundary the test exists for.
  * If the literal is genuinely invented, add it to ALLOWED in ${selfPath}
    so the next commit is quiet and the decision is recorded.
  * Grep the whole tree before you finish: one value spreads into doc comments, the
    generated client schema, another crate's arithmetic, and rendered PNG baselines.

Last resort: git commit --no-verify
`
);
process.exit(1);
