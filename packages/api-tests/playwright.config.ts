import { defineConfig } from "@playwright/test";

// Pure API e2e — no browser is ever launched (tests only use the `api`/`server`
// fixtures). global-setup builds the backend binary once; each test then spawns its
// own isolated instance (see fixtures.ts).
const workers = process.env.SURE_API_TEST_WORKERS
  ? Number(process.env.SURE_API_TEST_WORKERS)
  : undefined;
if (workers !== undefined && (!Number.isInteger(workers) || workers < 1)) {
  throw new Error("SURE_API_TEST_WORKERS must be a positive integer.");
}

export default defineConfig({
  testDir: "./specs",
  fullyParallel: true,
  // Let local hooks use the capacity available on a busy development machine.
  workers,
  reporter: [["list"]],
  timeout: 20_000,
  globalSetup: "./global-setup.ts",
});
