import { execSync } from "node:child_process";
import path from "node:path";

// Build the two binaries this suite runs, once, so every test can spawn them quickly: the
// backend, and the record/replay proxy that stands in for every third-party host it reaches.
//
// The proxy is not optional. Every server the suite spawns is pointed at it (see fixtures.ts),
// so a missing `sure-testproxy` is a suite that cannot start rather than a feature some specs
// skip.
//
// `pnpm test:api:blocked` runs the suite through scripts/blocked.mjs, which sets
// SURE_BLOCKED (plus the RUSTFLAGS and CARGO_TARGET_DIR that build needs): the binary then
// carries the tokio blocking detector, and every server a test spawns reports its long
// polls. See fixtures.ts for the log level and output handling that go with it. Both builds
// inherit that CARGO_TARGET_DIR, which is the directory fixtures.ts resolves both binaries out
// of.
export default async function globalSetup() {
  const repoRoot = path.resolve(process.cwd(), "..", "..");
  const features = process.env.SURE_BLOCKED ? " --features blocking-detector" : "";
  execSync(`cargo build -p sure-server --bin sure-api${features}`, { cwd: repoRoot, stdio: "inherit" });
  // A second invocation rather than a second `-p` on the first: `blocking-detector` is a feature
  // of `sure-server` alone, and cargo refuses an unqualified `--features` once more than one
  // package is selected. Sequential, so the two share one build-directory lock rather than
  // contending for it.
  execSync("cargo build -p sure-testproxy --bin sure-testproxy", { cwd: repoRoot, stdio: "inherit" });
}
