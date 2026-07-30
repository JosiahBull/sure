import { spawn, execSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { DEMO_TODAY } from "./demo-date";

const PORT = 8099;
const BASE = `http://127.0.0.1:${PORT}`;

/**
 * Build the app, boot the real backend serving the built SPA against a fresh
 * throwaway SQLite database, and seed it. The backend PID is written to a file so
 * global-teardown can stop it.
 */
export default async function globalSetup() {
  const webDir = process.cwd(); // Playwright runs from packages/web
  const repoRoot = path.resolve(webDir, "..", "..");
  const dbPath = path.join(repoRoot, "data", "test-e2e.db");
  const pidFile = path.join(webDir, "tests", ".server.pid");

  // Build the frontend and backend (both are fast when already compiled).
  execSync("pnpm run build:fast", { cwd: webDir, stdio: "inherit" });
  execSync("cargo build -p sure-api", { cwd: repoRoot, stdio: "inherit" });

  // Fresh database each run.
  for (const suffix of ["", "-shm", "-wal"]) {
    rmSync(dbPath + suffix, { force: true });
  }
  mkdirSync(path.dirname(dbPath), { recursive: true });

  const bin = path.join(repoRoot, "target", "debug", "sure-api");
  const server = spawn(bin, [], {
    cwd: repoRoot,
    env: {
      ...process.env,
      DATABASE_URL: `sqlite:${dbPath}`,
      WEB_DIR: path.join(webDir, "dist"),
      BIND_ADDR: `127.0.0.1:${PORT}`,
      RUST_LOG: "warn",
      // Screenshots can only be stable if the seeded data is. The scheduler's first check
      // runs on startup, so the exchange-rate and stock-price tasks would fetch live
      // figures and rewrite the numbers these snapshots assert on.
      BACKGROUND_TASKS: "off",
    },
    stdio: "ignore",
    detached: true,
  });
  server.unref();
  writeFileSync(pidFile, String(server.pid));

  // Wait for readiness.
  let up = false;
  for (let i = 0; i < 100; i++) {
    try {
      const res = await fetch(`${BASE}/api/health`);
      if (res.ok) {
        up = true;
        break;
      }
    } catch {
      /* not ready yet */
    }
    await sleep(150);
  }
  if (!up) throw new Error("backend did not become ready on " + BASE);

  // Seed demo data, dated against the suite's pinned "today" rather than the real one so
  // the screenshots stay byte-identical whatever day they run on.
  execSync("node scripts/seed.mjs", {
    cwd: repoRoot,
    env: { ...process.env, BASE, SEED_TODAY: DEMO_TODAY },
    stdio: "inherit",
  });

  if (!existsSync(pidFile)) throw new Error("missing server pid file");
}
