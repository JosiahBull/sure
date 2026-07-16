import { defineConfig } from "@playwright/test";

// Pure API e2e — no browser is ever launched (tests only use the `api`/`server`
// fixtures). global-setup builds the backend binary once; each test then spawns its
// own isolated instance (see fixtures.ts).
export default defineConfig({
  testDir: "./specs",
  fullyParallel: true,
  reporter: [["list"]],
  timeout: 20_000,
  globalSetup: "./global-setup.ts",
});
