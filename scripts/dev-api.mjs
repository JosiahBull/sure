// Rebuild and restart the backend whenever a Rust source, a crate manifest, a migration, or
// `.env` changes — so `pnpm dev` reloads the API the way Vite already reloads the SPA.
//
// Usage: node scripts/dev-api.mjs   (this is what `pnpm dev:api` runs)
//
// Three deliberate choices:
//
//   * No watcher dependency (cargo-watch, watchexec, nodemon). Node's recursive `fs.watch` is
//     enough, so a fresh clone still needs nothing but `pnpm install` and a Rust toolchain.
//   * `cargo build` + spawn the binary, not `cargo run`. Nothing then holds the cargo target
//     lock while the server runs, so a rebuild never waits on the process it is replacing.
//   * It builds *before* it stops the old server, and leaves the old one running when a build
//     fails. A compile takes seconds and a half-finished edit is normal, so the SPA keeps
//     talking to the last binary that worked instead of collecting proxy errors in between.
//     Overwriting the executable under a live process is fine on macOS and Linux: cargo
//     unlinks the path and links the new file, and the running process keeps its own inode.
import { spawn } from "node:child_process";
import { existsSync, watch } from "node:fs";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const BIN = "sure-api";
// `pnpm dev:api:blocked` runs this script through scripts/blocked.mjs, which sets
// SURE_BLOCKED along with the RUSTFLAGS and CARGO_TARGET_DIR the detector build needs.
const BLOCKED = Boolean(process.env.SURE_BLOCKED);
const BUILD_ARGS = ["build", "-p", "sure-server", "--bin", BIN];
if (BLOCKED) BUILD_ARGS.push("--features", "blocking-detector");
// `cargo build` honours CARGO_TARGET_DIR, so finding the binary afterwards has to as well.
const TARGET_DIR = process.env.CARGO_TARGET_DIR
  ? path.resolve(ROOT, process.env.CARGO_TARGET_DIR)
  : path.join(ROOT, "target");
const BIN_PATH = path.join(TARGET_DIR, "debug", BIN);

// Coalesce the burst of events one save produces — and the hundreds a `git checkout` does.
const DEBOUNCE_MS = 200;
// A SIGTERMed server drains in-flight requests first (SHUTDOWN_GRACE_SECS, 15s by default).
// In dev there is nothing to drain, so a server still alive after this is wedged, not busy.
const STOP_TIMEOUT_MS = 3_000;

// `.rs` is the obvious one. `Cargo.toml`/`Cargo.lock` change what gets built. `.sql` is a
// migration, embedded at compile time by `sqlx::migrate!`. `.env` is read once at startup, so
// a restart is the only way to pick an edit up — cargo no-ops the build in that case.
const WATCHED = /(\.rs|\.sql|Cargo\.toml|Cargo\.lock|\.env)$/;
// Build output and package dirs generate far more events than everything else combined.
const IGNORED = /(^|[/\\])(target|node_modules|\.git)([/\\]|$)/;

const log = (message) => console.log(`[dev:api] ${message}`);

/** Spawn a child, inheriting stdio, and resolve when it exits — never reject. */
function spawnProc(command, args, label) {
  const proc = spawn(command, args, { cwd: ROOT, stdio: "inherit" });
  const done = new Promise((resolve) => {
    proc.once("error", (err) => {
      console.error(`[dev:api] could not start ${label}: ${err.message}`);
      resolve({ code: null, signal: null, failedToStart: true });
    });
    proc.once("exit", (code, signal) => resolve({ code, signal }));
  });
  return { proc, done };
}

let server = null; // the running backend, if any
let build = null; // the in-flight `cargo build`, if any
// Bumped on every change. A build or restart whose generation is stale has been superseded by
// a newer edit and bails out, so a flurry of saves converges on one final restart.
let generation = 0;
let stopping = false;

async function stopServer() {
  if (!server) return;
  const { proc, done } = server;
  server = null; // clearing first marks the exit as deliberate for the handler in startServer
  proc.kill("SIGTERM");
  const escalate = setTimeout(() => {
    log(`${BIN} ignored SIGTERM for ${STOP_TIMEOUT_MS / 1000}s — killing it`);
    proc.kill("SIGKILL");
  }, STOP_TIMEOUT_MS);
  await done;
  clearTimeout(escalate);
}

function startServer() {
  const started = spawnProc(BIN_PATH, [], BIN);
  server = started;
  started.done.then((res) => {
    // Only an exit we did not ask for is news. A crash (a panic, a port already taken, a bad
    // migration) leaves the watcher up: the fix is an edit away, and exiting here would take
    // the Vite process down with us via concurrently's --kill-others-on-fail.
    if (server !== started) return;
    server = null;
    if (res.failedToStart) return;
    const how = res.signal ? `signal ${res.signal}` : `exit code ${res.code}`;
    log(`${BIN} stopped on its own (${how}) — waiting for a change to restart it`);
  });
}

async function rebuildAndRestart(gen, reason) {
  if (gen !== generation || stopping) return; // superseded while queued
  log(`${reason} — building…`);
  build = spawnProc("cargo", BUILD_ARGS, "cargo build");
  const res = await build.done;
  build = null;
  // Superseded mid-build (including by the SIGTERM `trigger` sends): the newer generation
  // owns the restart, and says so in its own log line.
  if (gen !== generation || stopping) return;
  if (res.failedToStart) return;
  if (res.code !== 0) {
    const fallback = server ? `leaving the running ${BIN} up` : "nothing to serve";
    log(`build failed — ${fallback}; waiting for a change`);
    return;
  }
  await stopServer();
  if (gen !== generation || stopping) return;
  startServer();
  log(`${BIN} is running from ${path.relative(ROOT, BIN_PATH)}`);
}

let queue = Promise.resolve();

function trigger(reason) {
  const gen = ++generation;
  // Whatever is compiling now is building code that is already out of date. Stop it rather
  // than waiting out a compile whose result we would throw away.
  if (build) build.proc.kill("SIGTERM");
  queue = queue.then(() => rebuildAndRestart(gen, reason)).catch((err) => console.error(`[dev:api] ${err}`));
}

/** The workspace's Rust crate directories, read from the root manifest's `members`. */
async function crateDirs() {
  const manifest = await readFile(path.join(ROOT, "Cargo.toml"), "utf8");
  const members = /members\s*=\s*\[([^\]]*)\]/.exec(manifest)?.[1] ?? "";
  const dirs = [...members.matchAll(/"([^"]+)"/g)]
    .map((match) => path.join(ROOT, match[1]))
    .filter((dir) => existsSync(dir));
  if (dirs.length === 0) {
    // Not fatal: root-level edits are still watched, and the first build still happens.
    log("could not read [workspace] members from Cargo.toml — only root-level files are watched");
  }
  return dirs;
}

let pendingTimer = null;
const pendingFiles = new Set();

function onChange(dir, filename) {
  if (!filename) return; // some platforms report a bare event with no name; nothing to act on
  if (IGNORED.test(filename) || !WATCHED.test(filename)) return;
  pendingFiles.add(path.relative(ROOT, path.join(dir, filename)));
  clearTimeout(pendingTimer);
  pendingTimer = setTimeout(() => {
    const files = [...pendingFiles];
    pendingFiles.clear();
    trigger(files.length === 1 ? `${files[0]} changed` : `${files[0]} and ${files.length - 1} more changed`);
  }, DEBOUNCE_MS);
}

const watchers = [];

function watchDir(dir, recursive) {
  try {
    watchers.push(watch(dir, { recursive }, (_event, filename) => onChange(dir, filename)));
  } catch (err) {
    log(`cannot watch ${path.relative(ROOT, dir) || "."}: ${err.message}`);
  }
}

async function shutdown() {
  if (stopping) return;
  stopping = true;
  generation++; // invalidate anything queued behind us
  for (const watcher of watchers) watcher.close();
  if (build) build.proc.kill("SIGTERM");
  await stopServer();
  process.exit(0);
}

for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, shutdown);

// The root itself, non-recursively, for `Cargo.toml`, `Cargo.lock` and `.env`. Watching those
// as individual files would miss the atomic rename most editors and `cargo` write with.
watchDir(ROOT, false);
// A crate added to [workspace] members needs the watcher restarted to be picked up.
for (const dir of await crateDirs()) watchDir(dir, true);

log(`watching for changes in ${watchers.length - 1} crates — edit any .rs file to reload`);
trigger("starting");
