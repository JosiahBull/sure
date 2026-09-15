import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * Every view is the whole household's.
 *
 * Sure was built for two people who split everything, so a control that narrows a report to
 * one of them answers a question neither of them is asking — and invites the one it is not
 * about to read the smaller number as a scoreboard. The filter is gone from the UI; accounts
 * and transactions still *carry* an owner, and the app still labels and groups by it, which is
 * the part that stays useful.
 *
 * These pin the absence, because an absence is the one thing a screenshot diff will not
 * notice coming back.
 */

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/** The two labels the removed selects used, either of which means the filter is back. */
const FILTER_LABELS = ["Whose money", "Filter by who it belongs to"];

test("the header bar offers a period and a one-off toggle, and nothing about people", async ({
  page,
}) => {
  await goto(page, "/");
  const subbar = page.locator(".subbar");
  await expect(subbar.locator("select")).toHaveCount(1);
  await expect(subbar.locator('select[aria-label="Time range"]')).toBeVisible();
  await expect(subbar.locator(".switch")).toBeVisible();
  for (const label of FILTER_LABELS) {
    await expect(subbar.locator(`select[aria-label="${label}"]`)).toHaveCount(0);
  }
});

test("the transactions filter panel offers no owner filter", async ({ page }) => {
  await goto(page, "/transactions");
  await page.getByRole("button", { name: /filter/i }).first().click();

  const panel = page.locator(".filter-panel");
  await expect(panel).toBeVisible();
  // The controls that remain are the ones about the rows themselves, not about whose they are.
  await expect(panel.locator('select[aria-label="Filter by category"]')).toBeVisible();
  for (const label of FILTER_LABELS) {
    await expect(panel.locator(`select[aria-label="${label}"]`)).toHaveCount(0);
  }
  await expect(panel).not.toContainText("Whole household");
});

test("a saved ?owner= link still opens, and shows the household's transactions", async ({
  page,
}) => {
  // Links written before the filter was removed are still in someone's history. The parameter
  // is now ignored rather than rejected, so the page loads normally — what it must not do is
  // come back as a filter, or fail on a value nothing reads any more.
  await goto(page, "/transactions?range=all&owner=person:1");
  await expect(page.locator(".tx-row").first()).toBeVisible();
  const unfiltered = await page.locator(".tx-row").count();

  await goto(page, "/transactions?range=all");
  await expect(page.locator(".tx-row").first()).toBeVisible();
  expect(await page.locator(".tx-row").count()).toBe(unfiltered);

  // And no chip claims a filter is active on account of it — the row is not rendered at all
  // when nothing is filtered, so count the chips rather than reading the row that holds them.
  await expect(page.locator(".chip-row .chip")).toHaveCount(0);
});
