import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.reload(); // a hash change keeps the component mounted; these want a clean load
  await page.waitForLoadState("networkidle");
}

/**
 * A valuation is a level, not a movement. These cover the ways that distinction has to hold in
 * a list built entirely around movements — the sums, the selection, and the day headings — plus
 * the reason the feature exists: setting a balance for a date in the past.
 */

// Family Home: seeded with manual valuations and no transactions at all, which is exactly the
// shape that used to render "No transactions." over the top of its own history.
const HOME = "/transactions?account=4";

test("an account's manual valuations show as rows in its history", async ({ page }) => {
  await goto(page, HOME);

  const rows = page.locator(".val-row");
  await expect(rows.first()).toBeVisible();
  await expect(rows.first()).toContainText("Value set");
  await expect(rows.first()).toContainText("manual");
  // A level reads `= $X`, against a transaction's +/- delta.
  await expect(rows.first().locator(".val-amount")).toContainText("=");
});

/**
 * The all-accounts view is a movement stream; a level belonging to one of six accounts says
 * nothing in it. This is also what keeps the committed full-page baseline honest.
 */
test("valuation rows never appear in the all-accounts view", async ({ page }) => {
  await goto(page, "/transactions");
  await expect(page.locator(".tx-row").first()).toBeVisible();
  expect(await page.locator(".val-row").count()).toBe(0);
});

/**
 * `[].every(...)` is `true`, so a day holding only a valuation used to render a ticked
 * select-all box that selected nothing — and `dayGroups` has no entry for it, so the heading
 * would have claimed a `$0.00` net for a day on which nothing moved.
 */
test("a day with only a valuation offers no checkbox and claims no total", async ({ page }) => {
  await goto(page, HOME);

  const dayGroup = page.locator(".day-group").filter({ has: page.locator(".val-row") }).first();
  await expect(dayGroup).toBeVisible();
  expect(await dayGroup.locator(".day-head .tx-check input").count()).toBe(0);
  expect(await dayGroup.locator(".day-total").count()).toBe(0);
});

test("a back-dated balance can be set, and lands in date order", async ({ page }) => {
  await goto(page, HOME);
  await page.getByRole("button", { name: "Set value", exact: true }).click();

  await page.getByLabel("Value", { exact: true }).fill("650000");
  await page.getByLabel("Value as of").fill("2019-06-01");
  await page.locator('input[placeholder*="opening balance"]').fill("bought it");
  await page.getByRole("button", { name: "Set value", exact: true }).last().click();

  // Newest-first, so a 2019 value is the last row on the page.
  const rows = page.locator(".val-row");
  await expect(rows.last()).toContainText("650,000");
  await expect(rows.last()).toContainText("bought it");

  // Clean up: this spec shares the seeded database with every other one.
  await page
    .locator(".valuations .line", { hasText: "650,000" })
    .getByRole("button", { name: /Delete value/ })
    .click();
  await page.getByRole("button", { name: "Delete?" }).click();
  await expect(page.locator(".val-row", { hasText: "650,000" })).toHaveCount(0);
});

/**
 * Selection is over transactions only — a valuation can never be part of a bulk edit, and its
 * id would collide with a transaction's in the selected set.
 *
 * Asserted on an account whose history is *entirely* valuations, so the claim is exact and
 * doesn't depend on how many transactions happen to be on the current page (select-all spans
 * the whole filtered set, not the page).
 */
test("select-all ignores valuation rows entirely", async ({ page }) => {
  await goto(page, HOME);
  await expect(page.locator(".val-row").first()).toBeVisible();
  expect(await page.locator(".tx-row:not(.val-row)").count()).toBe(0);

  await page.getByRole("checkbox", { name: "Select all" }).check();
  // Nothing selectable here, so no bulk bar — rather than one claiming two rows.
  await expect(page.locator(".bulkbar")).toHaveCount(0);
});
