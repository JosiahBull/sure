import { execSync } from "node:child_process";
import path from "node:path";

// Build the two binaries this suite runs, once, so every test can spawn them quickly: the
// backend, and the record/replay proxy that stands in for every third-party host it reaches.
//
// The proxy is not optional. Every server the suite spawns is pointed at it (see fixtures.ts),
// so a missing `sure-testproxy` is a suite that cannot start rather than a feature some specs
// skip.
//
// One cargo invocation for both, so they share a single build-directory lock instead of
// contending for it. It honours CARGO_TARGET_DIR, which is the directory fixtures.ts resolves
// both binaries out of.
export default async function globalSetup() {
  const repoRoot = path.resolve(process.cwd(), "..", "..");
  execSync("cargo build -p sure-server --bin sure-api -p sure-testproxy --bin sure-testproxy", {
    cwd: repoRoot,
    stdio: "inherit",
  });
}
