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

test("an account's values show as rows in its history, whoever wrote them", async ({ page }) => {
  await goto(page, HOME);

  const rows = page.locator(".val-row");
  await expect(rows.first()).toBeVisible();
  await expect(rows.first()).toContainText("Value set");
  // A level reads `= $X`, against a transaction's +/- delta.
  await expect(rows.first().locator(".val-amount")).toContainText("=");

  // Every source, not just the ones typed by hand. Filtering to `manual` by default meant an
  // account whose value is *only* ever synced — a loan the lender reports a balance for and
  // nothing else — rendered an empty page, which is the opposite of the point.
  // Wait for the fetch to land before counting: the rows arrive after the first paint.
  await expect.poll(() => rows.count()).toBeGreaterThan(1);
  const badges = new Set(await rows.locator(".badge").allTextContents());
  expect(badges.has("manual")).toBe(true);
  expect(badges.has("scheduled")).toBe(true);

  // …and they can still be narrowed to the ones you set.
  const all = await rows.count();
  await page.locator("label.only-mine").click();
  await expect.poll(() => rows.count()).toBeLessThan(all);
  expect(new Set(await rows.locator(".badge").allTextContents())).toEqual(new Set(["manual"]));
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

/**
 * The router keys the active page on the path *without* its query and does not remount on a
 * query-only change, so a page seeding state from a param has to keep following it. Clicking a
 * different account in the side panel navigates to the page it is already on — which changed
 * the URL and nothing else.
 *
 * Driven through the real panel, so it needs a viewport wide enough to have one: below 720px
 * `.panel-col` is `width: 0`.
 */
test("choosing another account from the side panel actually changes the view", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await goto(page, HOME); // Family Home: valuations, no transactions
  await expect(page.locator(".val-row").first()).toBeVisible();

  const panel = page.locator(".panel");
  // Groups collapse by default; open whichever holds the target account.
  const target = panel.locator(".acct-row", { hasText: "Everyday" }).first();
  if (!(await target.isVisible())) {
    await panel.locator(".group-row, .acct-group, button").filter({ hasText: /Cash/i }).first().click();
  }
  await target.click();

  await expect(page).toHaveURL(/account=/);
  // The view followed, not just the URL: this account has transactions and no valuations.
  await expect(page.locator(".tx-row:not(.val-row)").first()).toBeVisible();
  await expect(page.locator(".val-row")).toHaveCount(0);
});

test("a manual value can be edited in place", async ({ page }) => {
  await goto(page, HOME);
  await page.getByRole("button", { name: "Set value", exact: true }).click();

  const row = page.locator(".valuations .line").first();
  const before = await row.innerText();
  await row.getByRole("button", { name: /^Edit value/ }).click();

  // The form is the editor: it loads the row, and saving changes it rather than adding one.
  const count = await page.locator(".valuations .line").count();
  await page.getByLabel("Value", { exact: true }).fill("999000");
  await page.getByRole("button", { name: "Save changes" }).click();

  await expect(page.locator(".valuations .line").first()).toContainText("999,000");
  expect(await page.locator(".valuations .line").count()).toBe(count);

  // Put it back so the shared seed is unchanged.
  const restored = before.match(/[\d,]+\.\d\d/)![0].replace(/,/g, "");
  await page.locator(".valuations .line").first().getByRole("button", { name: /^Edit value/ }).click();
  await page.getByLabel("Value", { exact: true }).fill(restored);
  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator(".valuations .line").first()).toContainText(restored.split(".")[0].slice(0, 3));
});
