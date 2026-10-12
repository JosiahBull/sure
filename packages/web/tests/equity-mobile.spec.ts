import type { Locator, TestInfo } from "@playwright/test";
import { test, expect } from "./fixtures";

// Global setup serves a throwaway database seeded by scripts/seed.mjs. Never point these
// screenshot captures at a developer or production server.
async function capture(panel: Locator, testInfo: TestInfo, name: string) {
  const page = panel.page();
  await page.evaluate(() => document.fonts.ready);
  const viewport = page.viewportSize()!;
  const bounds = (await panel.boundingBox())!;
  // Fit the whole panel vertically before cropping it. Chromium can otherwise clip a tall
  // element's top after scrolling it into view; its responsive width stays unchanged.
  await page.setViewportSize({ ...viewport, height: Math.max(viewport.height, Math.ceil(bounds.height) + 200) });
  try {
    await panel.scrollIntoViewIfNeeded();
    await panel.screenshot({
      path: testInfo.outputPath(`${name}.png`),
      // Keep fixed shell navigation out of these component-only review images.
      style: ".rail, .subbar, .panel-col { visibility: hidden !important; }",
    });
  } finally {
    await page.setViewportSize(viewport);
  }
}

async function fits(panel: Locator) {
  const overflow = await panel.evaluate((el) => {
    const bounds = el.getBoundingClientRect();
    return [...el.querySelectorAll("input, button, .figure, .record-grant, .record-value")]
      .filter((node) => node.getBoundingClientRect().width > 0)
      .filter((node) => {
        const r = node.getBoundingClientRect();
        return r.left < bounds.left - 1 || r.right > bounds.right + 1;
      })
      .map((node) => node.textContent || node.getAttribute("aria-label") || node.tagName);
  });
  expect(overflow, "controls and data must fit inside the equity panel").toEqual([]);
}

for (const width of [320, 402, 768, 1280]) {
  test(`ESOP controls fit at ${width}px`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width, height: 874 });
    await page.goto("/#/settings/accounts");
    await page.locator(".acct", { hasText: "Startco Options" })
      .getByRole("button", { name: "Holdings", exact: true }).click();
    const panel = page.locator(".equity");
    await expect(panel.locator(".grant")).toHaveCount(1);
    await expect(panel.getByRole("button", { name: "Record today" })).toHaveCount(0);
    await expect(panel.getByText("Current value", { exact: true })).toHaveCount(0);
    await expect(panel.locator(".terms")).toHaveCount(0);
    const title = (await panel.locator(".grant-name").boundingBox())!;
    const value = (await panel.locator(".grant-value").boundingBox())!;
    expect(Math.abs(value.x - title.x)).toBeLessThan(1);
    expect(value.y).toBeGreaterThan(title.y);
    const menu = (await panel.locator(".menu-trigger").boundingBox())!;
    expect(menu.x).toBeGreaterThan(title.x);
    expect(Math.abs(menu.y - title.y)).toBeLessThan(8);
    await expect(panel.getByText("Vesting schedule", { exact: true })).toHaveCount(0);
    await capture(panel, testInfo, `holdings-${width}`);
    await fits(panel);
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await panel.getByRole("menuitem", { name: "Vesting schedule", exact: true }).click();
    await expect(panel.getByRole("menu")).toHaveCount(0);
    await expect(panel.locator(".grant-terms")).toContainText("48 months");
    await fits(panel);
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await expect(panel.getByRole("menuitem", { name: "Vesting schedule", exact: true })).toHaveAttribute("aria-expanded", "true");
    await panel.getByRole("menuitem", { name: "Vesting schedule", exact: true }).click();
    await expect(panel.locator(".grant-terms")).toHaveCount(0);
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await expect(panel.getByRole("menu")).toBeVisible();
    await fits(panel);
    await capture(panel, testInfo, `grant-menu-${width}`);
    await page.keyboard.press("ArrowDown");
    await expect(panel.getByRole("menuitem", { name: "Edit", exact: true })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(panel.getByRole("menu")).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Actions for Startco", exact: true })).toBeFocused();
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await panel.getByRole("menuitem", { name: "Delete", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Delete grant", exact: true })).toBeVisible();
    await panel.getByRole("button", { name: "Keep grant", exact: true }).click();
    await expect(panel.locator(".grant")).toHaveCount(1);
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await panel.locator(".total").click();
    await expect(panel.getByRole("menu")).toHaveCount(0);
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await panel.getByRole("menuitem", { name: "Exercise", exact: true }).click();
    await capture(panel, testInfo, `exercise-${width}`);
    await fits(panel);
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await panel.getByRole("menuitem", { name: "Edit", exact: true }).click();
    await expect(panel.locator("form.equity-form")).toHaveCount(1);
    await expect(panel.locator(".sub-title")).toHaveText("Edit grant");
    await capture(panel, testInfo, `edit-grant-${width}`);
    await fits(panel);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("button", { name: "Add grant", exact: true }).click();
    await expect(panel.getByLabel("Company", { exact: true })).toHaveValue("");
    await capture(panel, testInfo, `add-grant-${width}`);
    await fits(panel);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("tab", { name: /Activity/ }).click();
    await expect(panel.getByRole("list", { name: "Equity activity" })).toBeVisible();
    const eventCount = Number(await panel.getByRole("tab", { name: /Activity/ }).locator(".count").textContent());
    await expect(panel.locator(".activity-record")).toHaveCount(eventCount);
    await expect(panel.locator(".activity-record").first()).toContainText("250 units");
    const record = (await panel.locator(".activity-record").first().boundingBox())!;
    expect(record.height).toBeLessThan(64);
    await capture(panel, testInfo, `activity-${width}`);
    await fits(panel);
    await panel.getByRole("tab", { name: /Valuations/ }).click();
    await expect(panel.getByText("Price per unit", { exact: true })).toBeVisible();
    await expect(panel.getByRole("button", { name: "Record today" })).toBeVisible();
    await capture(panel, testInfo, `valuations-${width}`);
    await fits(panel);
  });
}

test("multiple grants with long labels fit on a small phone and in the account ledger", async ({ page }, testInfo) => {
  const created = await page.request.post("/api/accounts", {
    data: {
      name: "Startco mobile demo", kind: "shares_private", currency_code: "USD",
      opening_balance_minor: 0, opening_balance_date: "2024-01-01",
      ownership: { kind: "joint" },
      metadata: { profile: "shares", broker: "Carta", ticker: "STARTCO" },
    },
  });
  expect(created.ok(), await created.text()).toBe(true);
  const account: { id: number } = await created.json();
  try {
    for (const note of ["Initial grant with a long descriptive label", "Additional grant"]) {
      const grant = await page.request.post(`/api/accounts/${account.id}/equity-grants`, {
        data: {
          company: "Startco", grant_date: "2024-01-01", quantity: 12000,
          strike_minor: 120, unit_value_minor: 800, vest_months: 48, cliff_months: 12, note,
        },
      });
      expect(grant.ok()).toBe(true);
    }
    await page.setViewportSize({ width: 320, height: 874 });
    await page.goto(`/#/transactions?account=${account.id}&range=all`);
    const panel = page.locator(".equity");
    await expect(panel.locator(".grant")).toHaveCount(2);
    await fits(panel);
    await capture(panel, testInfo, "multiple-grants-320");
    // Exercise the native inputs through the UI, then verify exact persisted cents/units.
    await panel.getByRole("button", { name: "Add grant", exact: true }).click();
    let form = panel.locator("form.equity-form");
    await form.getByLabel("Company", { exact: true }).fill("Startco");
    await form.getByLabel("Vesting starts", { exact: true }).fill("2024-01-01");
    const units = form.getByLabel("Units granted", { exact: true });
    await expect(units).toHaveAttribute("type", "number");
    await units.fill("1.5");
    await form.getByRole("button", { name: "Add grant", exact: true }).click();
    await expect(panel.locator(".grant")).toHaveCount(2);
    await units.fill("1.25e3");
    await form.getByLabel("Strike price", { exact: true }).fill("1.234e1");
    await form.getByLabel("Vesting (months)", { exact: true }).fill("12");
    await form.getByLabel("Cliff (months)", { exact: true }).fill("0");
    await form.locator(".form-optional summary").click();
    const label = form.getByLabel("Grant label (optional)", { exact: true });
    await label.fill("Touch input grant");
    await label.fill("");
    await expect(label).toBeVisible();
    await label.fill("Touch input grant");
    await fits(panel);
    await form.getByRole("button", { name: "Add grant", exact: true }).click();
    await expect(panel.locator(".grant")).toHaveCount(3);
    const grants = await (await page.request.get(`/api/accounts/${account.id}/equity-grants`)).json() as Array<{
      id: number; quantity: number; strike_minor: number; note: string | null;
    }>;
    const saved = grants.find((g) => g.note === "Touch input grant")!;
    expect(saved.quantity).toBe(1250);
    expect(saved.strike_minor).toBe(1234);
    await panel.getByRole("button", { name: "Actions for Touch input grant", exact: true }).click();
    await panel.getByRole("menuitem", { name: "Exercise", exact: true }).click();
    form = panel.locator("form.equity-form");
    await expect(form.getByLabel("Date", { exact: true })).toHaveAttribute("type", "date");
    await form.getByLabel("Date", { exact: true }).fill("2026-07-15");
    await form.getByLabel("Units", { exact: true }).fill("5e1");
    await form.getByLabel("Price paid per unit", { exact: true }).fill("1.23");
    await form.locator(".form-optional summary").click();
    await form.getByLabel("Note (optional)", { exact: true }).fill("Touch exercise");
    await form.getByRole("button", { name: "Record exercise", exact: true }).click();
    await expect(form).toHaveCount(0);
    const exercises = await (await page.request.get(`/api/equity-grants/${saved.id}/exercises`)).json();
    expect(exercises).toEqual(expect.arrayContaining([
      expect.objectContaining({ quantity: 50, price_minor: 123, note: "Touch exercise" }),
    ]));
    await panel.getByRole("tab", { name: /Activity/ }).click();
    await expect(panel.locator(".record-grant").first()).toContainText(/grant/);
    await fits(panel);
    await capture(panel, testInfo, "multiple-grants-activity-320");
  } finally {
    // Remove only the synthetic account this test owns.
    expect((await page.request.delete(`/api/accounts/${account.id}`)).ok()).toBe(true);
  }
});
