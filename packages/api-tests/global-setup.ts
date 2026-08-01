import { execSync } from "node:child_process";
import path from "node:path";

// Build the backend once so every test can spawn the compiled binary quickly.
//
// `pnpm test:api:blocked` runs the suite through scripts/blocked.mjs, which sets
// SURE_BLOCKED (plus the RUSTFLAGS and CARGO_TARGET_DIR that build needs): the binary then
// carries the tokio blocking detector, and every server a test spawns reports its long
// polls. See fixtures.ts for the log level and output handling that go with it.
export default async function globalSetup() {
  const repoRoot = path.resolve(process.cwd(), "..", "..");
  const features = process.env.SURE_BLOCKED ? " --features blocking-detector" : "";
  execSync(`cargo build -p sure-server --bin sure-api${features}`, { cwd: repoRoot, stdio: "inherit" });
}
