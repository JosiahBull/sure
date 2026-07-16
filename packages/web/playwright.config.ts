import { defineConfig } from "@playwright/test";

// The whole app (SPA + API) is booted by global-setup on port 8099 and seeded with
// deterministic-ish demo data; tests drive it in a fresh mobile Chromium context.
export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  reporter: process.env.CI ? "list" : [["list"]],
  timeout: 30_000,
  globalSetup: "./tests/global-setup.ts",
  globalTeardown: "./tests/global-teardown.ts",
  expect: {
    toHaveScreenshot: { maxDiffPixelRatio: 0.03, animations: "disabled" },
  },
  use: {
    baseURL: "http://127.0.0.1:8099",
    browserName: "chromium",
    // Pin the palette so snapshots are deterministic (the app resolves "auto" from
    // prefers-color-scheme). Dark is the app's default identity.
    colorScheme: "dark",
    // iPhone-ish viewport (the install target), fixed for stable snapshots.
    viewport: { width: 402, height: 874 },
    deviceScaleFactor: 1,
    isMobile: true,
    hasTouch: true,
    // The PWA service worker would cache responses and make tests flaky.
    serviceWorkers: "block",
  },
});
