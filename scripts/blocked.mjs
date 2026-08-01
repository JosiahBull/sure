// Run a dev command with the tokio blocking detector switched on, then get out of the way.
//
// Usage: node scripts/blocked.mjs <command> [args...]
//   pnpm dev:api:blocked    → node scripts/blocked.mjs node scripts/dev-api.mjs
//   pnpm test:api:blocked   → node scripts/blocked.mjs pnpm --filter @sure/api-tests test
//
// All this does is set three environment variables and exec the rest of the argv. The
// consumers (scripts/dev-api.mjs, packages/api-tests) read SURE_BLOCKED to decide whether
// to pass `--features blocking-detector` to cargo and to keep the backend's log output.
//
//   * RUSTFLAGS gets `--cfg tokio_unstable`. Tokio's task instrumentation — the spans the
//     detector measures — is compiled out without it, so the feature alone reports nothing
//     (the server says so at startup rather than looking clean).
//   * CARGO_TARGET_DIR moves to target/blocked. A different RUSTFLAGS is a different build
//     fingerprint: sharing target/ would mean a full recompile of the workspace on every
//     switch between a normal run and a detector run, in *both* directions. Costs a few GB
//     of disk and one cold build; `pnpm clean` removes it with the rest of target/.
//   * SURE_BLOCKED marks the run for the two scripts above.
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const [command, ...args] = process.argv.slice(2);

if (!command) {
  console.error("usage: node scripts/blocked.mjs <command> [args...]");
  process.exit(2);
}

const CFG = "--cfg tokio_unstable";
// Appended, not replaced: a developer may already be passing their own flags, and cargo
// splits RUSTFLAGS on whitespace.
const rustflags = process.env.RUSTFLAGS?.includes(CFG)
  ? process.env.RUSTFLAGS
  : [process.env.RUSTFLAGS, CFG].filter(Boolean).join(" ");

const env = {
  ...process.env,
  RUSTFLAGS: rustflags,
  // Absolute, because the consumers resolve it from different working directories.
  CARGO_TARGET_DIR: process.env.CARGO_TARGET_DIR
    ? path.resolve(ROOT, process.env.CARGO_TARGET_DIR)
    : path.join(ROOT, "target", "blocked"),
  SURE_BLOCKED: "1",
};

console.log(`[blocked] RUSTFLAGS="${env.RUSTFLAGS}" CARGO_TARGET_DIR=${path.relative(ROOT, env.CARGO_TARGET_DIR)}`);
console.log("[blocked] first build in this target dir compiles the whole dependency graph — give it a few minutes");

const child = spawn(command, args, { cwd: ROOT, env, stdio: "inherit", shell: false });
child.on("error", (err) => {
  console.error(`[blocked] could not start ${command}: ${err.message}`);
  process.exit(1);
});
// Forward the signals a Ctrl-C or a `pnpm dev` shutdown sends, so the child stops the way
// it would if it had been run directly.
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => child.kill(signal));
child.on("exit", (code, signal) => process.exit(signal ? 1 : (code ?? 0)));
