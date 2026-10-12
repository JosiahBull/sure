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
      style: ".rail, .subbar { visibility: hidden !important; }",
    });
  } finally {
    await page.setViewportSize(viewport);
  }
}

async function fits(panel: Locator) {
  const overflow = await panel.evaluate((el) => {
    const bounds = el.getBoundingClientRect();
    return [...el.querySelectorAll("input, button, .figure, .c-grant, .c-value")]
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
    expect(value.x).toBeGreaterThan(title.x);
    expect(Math.abs(value.y - title.y)).toBeLessThan(8);
    await capture(panel, testInfo, `holdings-${width}`);
    await fits(panel);
    await panel.getByText("Vesting schedule", { exact: true }).click();
    await expect(panel.locator(".grant-terms p")).toContainText("48 months");
    await fits(panel);
    await panel.getByText("Vesting schedule", { exact: true }).click();
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
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("button", { name: "Actions for Startco", exact: true }).click();
    await panel.getByRole("menuitem", { name: "Edit", exact: true }).click();
    await capture(panel, testInfo, `edit-grant-${width}`);
    await fits(panel);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("button", { name: "Add grant", exact: true }).click();
    await expect(panel.getByLabel("Company", { exact: true })).toHaveValue("");
    await capture(panel, testInfo, `add-grant-${width}`);
    await fits(panel);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("tab", { name: /Activity/ }).click();
    await expect(panel.locator(".log")).toBeVisible();
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
    await panel.getByRole("tab", { name: /Activity/ }).click();
    await expect(panel.locator("td.c-grant").first()).toContainText(/grant/);
    await fits(panel);
    await capture(panel, testInfo, "multiple-grants-activity-320");
  } finally {
    // Remove only the synthetic account this test owns.
    expect((await page.request.delete(`/api/accounts/${account.id}`)).ok()).toBe(true);
  }
});
