import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * The selected period lives in the address bar.
 *
 * Which makes a view linkable and survive a reload — and, because the two have to agree in both
 * directions, is the kind of thing that quietly turns into a fight: the reader putting the URL's
 * period back a tick after the writer changed it, so the select snaps to its old value the
 * instant you touch it. Several of these exist to pin that down.
 */

const RANGE = 'select[aria-label="Time range"]';

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/** The hash, without the origin — what a shared link would actually carry. */
const hash = (page: Page) => page.url().split("#")[1] ?? "";

test("a visit with nothing selected starts on last month, and says nothing in the URL", async ({
  page,
}) => {
  await goto(page, "/");
  await expect(page.locator(RANGE)).toHaveValue("last_month");
  // The default is left out: a parameter that names the value you would have got by saying
  // nothing makes every plain link longer and reads as a choice nobody made.
  expect(hash(page)).toBe("/");
});

test("picking a period puts it in the URL, and it survives a reload", async ({ page }) => {
  await goto(page, "/");
  await page.selectOption(RANGE, "last_90");

  await expect.poll(() => hash(page)).toBe("/?range=last_90");

  await page.reload();
  await page.waitForLoadState("networkidle");
  await expect(page.locator(RANGE)).toHaveValue("last_90");
});

test("a period in the URL is adopted on arrival", async ({ page }) => {
  await goto(page, "/?range=ytd");
  await expect(page.locator(RANGE)).toHaveValue("ytd");

  // And it survives moving between pages, because the shell owns it rather than the page.
  await goto(page, "/transactions?range=ytd");
  await expect(page.locator(RANGE)).toHaveValue("ytd");
});

test("going back to the default takes the parameter out again", async ({ page }) => {
  await goto(page, "/?range=ytd");
  await page.selectOption(RANGE, "last_month");
  await expect.poll(() => hash(page)).toBe("/");
});

test("the select does not snap back when it is changed", async ({ page }) => {
  // The failure mode this guards: the URL→filters effect also reading `filters`, so changing
  // the period re-runs it, it reads the URL as it stood *before* the writer updated it, finds a
  // disagreement and puts the old value back. The symptom is a control that will not move.
  await goto(page, "/");
  for (const range of ["mtd", "last_30", "last_90", "ytd", "all"]) {
    await page.selectOption(RANGE, range);
    await expect(page.locator(RANGE), `${range} did not stick`).toHaveValue(range);
  }
  await expect.poll(() => hash(page)).toBe("/?range=all");
});

test("a run of period changes leaves one entry to go back from", async ({ page }) => {
  await goto(page, "/transactions");
  await goto(page, "/");
  for (const range of ["mtd", "last_30", "last_90", "ytd"]) {
    await page.selectOption(RANGE, range);
    await expect(page.locator(RANGE)).toHaveValue(range);
  }

  // Four changes, one entry: they replace rather than push, so Back is still "the page I came
  // from" and not a walk backwards through a menu.
  await page.goBack();
  await page.waitForLoadState("networkidle");
  await expect(page).toHaveURL(/#\/transactions/);
});

test("a deep link that carries its own period still wins", async ({ page }) => {
  // `?account=` means "show me this account", so the transactions page widens the range to all
  // — and that has to survive the shell, which would otherwise be entitled to overwrite it with
  // whatever was selected before. Silence in the URL means "leave it alone", not "reset".
  await goto(page, "/?range=last_90");
  await goto(page, "/transactions?account=1");
  await expect(page.locator(RANGE)).toHaveValue("all");
  // And the widened period is then recorded, so the link in the address bar describes the view.
  await expect.poll(() => hash(page)).toContain("range=all");
});

test("a brushed window round-trips as start and end", async ({ page }) => {
  await goto(page, "/transactions?start=2026-03-01&end=2026-03-31");
  // A shared window outranks any preset — the transactions page has always applied that
  // precedence, and putting the period in the URL must not quietly change it.
  await expect(page.locator(".zoom-out")).toBeVisible();
  expect(hash(page)).toContain("start=2026-03-01");
  expect(hash(page)).toContain("end=2026-03-31");

  // Clearing the zoom takes both out and leaves the preset behind.
  await page.locator(".zoom-out").click();
  await expect.poll(() => hash(page)).not.toContain("start=");
  await expect.poll(() => hash(page)).not.toContain("end=");
});
