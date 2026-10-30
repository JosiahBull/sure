import { test as realClock } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * The connection page (src/pages/Debug.svelte): hidden from the settings menu, reachable in an
 * installed app with no address bar, and showing what it measured.
 *
 * 100.64.12.34 is an invented tailnet address, sent the way a reverse proxy would.
 */

test("the connection page is not in the settings menu, and five taps on the version open it", async ({
  page,
}) => {
  await page.goto("/#/settings/accounts");
  await page.waitForLoadState("networkidle");
  await page.getByRole("button", { name: /^Show settings menu$/ }).click();

  const menu = page.locator(".settings-nav");
  await expect(menu.locator("a[href='#/settings/debug']")).toHaveCount(0);

  const version = menu.getByRole("button", { name: /^Sure \d+\.\d+\.\d+$/ });
  for (let i = 0; i < 4; i++) await version.click();
  await expect(page).toHaveURL(/#\/settings\/accounts$/);
  await version.click();
  await expect(page).toHaveURL(/#\/settings\/debug$/);
  await expect(page.getByRole("heading", { name: "Debug Extra for Experts" })).toBeVisible();
});

// The real clock, not the suite's fixed one: Playwright's fake clock replaces `performance`,
// whose timeline is the whole subject of this page.
realClock("the connection page shows the launch, the probe and the server's view, and copies them", async ({
  page,
  context,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.setExtraHTTPHeaders({ "X-Real-IP": "100.64.12.34" });

  // Launch on the dashboard, then open the page in the same document, as the gesture does.
  await page.goto("/");
  await page.waitForLoadState("networkidle");
  await page.goto("/#/settings/debug");
  // The button reads "Running…" until the probe and the echo have answered.
  await expect(page.getByRole("button", { name: "Run probe again" })).toBeEnabled();

  const section = (title: string) => page.locator(`section[data-section="${title}"]`);
  await expect(section("Launch")).toContainText(/fresh figures\s*\d+(\.\d)? ms/);
  await expect(section("Launch requests")).toContainText("net-worth?");
  await expect(section("Probe")).toContainText("seq wall");
  await expect(section("Probe")).toContainText("par net");
  await expect(section("Server's view")).toContainText("Tailscale · 100.64.12.34 (x-real-ip)");
  await expect(section("Server's view")).toContainText(/tcp peer\s*127\.0\.0\.1 · loopback/);
  await expect(section("Device")).toContainText(/build\s*\d+\.\d+\.\d+ · /);

  await page.getByRole("button", { name: "Copy text" }).click();
  await expect(page.getByRole("button", { name: "Copied" })).toBeVisible();
  const text = await page.evaluate(() => navigator.clipboard.readText());
  for (const heading of ["## Launch", "## Launch requests", "## Probe", "## Server's view", "## Device"]) {
    expect(text).toContain(heading);
  }
  expect(text).toMatch(/client\s+Tailscale · 100\.64\.12\.34/);

  await page.getByRole("button", { name: "Copy JSON" }).click();
  const json = JSON.parse(await page.evaluate(() => navigator.clipboard.readText())) as {
    echo: { data: { client: { network: string } } };
    launch: { name: string }[];
  };
  expect(json.echo.data.client.network).toBe("tailscale");
  expect(json.launch.some((e) => e.name.includes("/api/reports/net-worth"))).toBe(true);
});
