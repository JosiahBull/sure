import { type Page, type Route } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * Launching from the last-known figures (src/lib/lastKnown.ts).
 *
 * A launch draws what this device last saw at once, marked stale, and replaces it with the
 * server's answer — and an edit must never be followed by the pre-edit figure. Every test here
 * holds the API rather than racing it: a kept figure that is on screen while no report has been
 * allowed to answer can only have come from storage, which is the property under test, and a
 * held request is the only way to look at the moment between the two.
 */

const STORE = "sure:last-known";
const FIGURE = ".stat .value";
const STALE = '.cards[data-stale="true"]';
const FRESH = '.cards[data-stale="false"]';

/** The shape `lastKnown.ts` persists. Kept local: the module is not importable from a spec. */
type Store = { v: string; entries: Record<string, { data: unknown; at: number }> };
type NetWorth = { points: { net_worth_minor: number }[] };

/** A net worth nobody has, so a figure on screen can be traced back to the store. */
const SENTINEL_MINOR = 123_456_789_00;
const SENTINEL = "$123,456,789.00";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/**
 * Hold every report request until `release()` is called. The rest of the API answers as usual,
 * so the base currency and the household still load.
 */
async function holdReports(page: Page): Promise<() => void> {
  let release!: () => void;
  const released = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/api/reports/**", async (route: Route) => {
    await released;
    // The page may have moved on by the time it is let go.
    await route.continue().catch(() => {});
  });
  return release;
}

const readStore = (page: Page) =>
  page.evaluate((key) => {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as Store) : null;
  }, STORE);

const writeStore = (page: Page, store: Store) =>
  page.evaluate(([key, value]) => localStorage.setItem(key, value), [STORE, JSON.stringify(store)]);

/** The kept store, with the dashboard's net worth replaced by {@link SENTINEL_MINOR}. */
async function storeWithSentinel(page: Page): Promise<Store> {
  const store = await readStore(page);
  expect(store, "the first visit kept nothing").not.toBeNull();
  const nw = store!.entries["net-worth:last_3m"]?.data as NetWorth | undefined;
  expect(nw, `no kept net worth in ${Object.keys(store!.entries)}`).toBeDefined();
  nw!.points[nw!.points.length - 1].net_worth_minor = SENTINEL_MINOR;
  return store!;
}

test("a launch draws the last-known figures before the API answers, then the server's", async ({
  page,
}) => {
  await goto(page, "/");
  const fresh = await page.locator(FIGURE).innerText();
  await writeStore(page, await storeWithSentinel(page));

  const release = await holdReports(page);
  await page.reload();

  // No report has answered, so this can only have come from storage — and it is marked.
  await expect(page.locator(FIGURE)).toHaveText(SENTINEL);
  await expect(page.locator(STALE)).toBeVisible();
  await expect(page.locator(".spinner")).toHaveCount(0);

  release();
  // The server's answer wins, and the marking lifts with it.
  await expect(page.locator(FIGURE)).toHaveText(fresh);
  await expect(page.locator(FRESH)).toBeVisible();
});

test("only the launch screen's summaries are kept — never a transaction list", async ({ page }) => {
  await goto(page, "/");
  await goto(page, "/transactions");
  const keys = Object.keys((await readStore(page))!.entries).sort();
  expect(keys).toEqual([
    "balances",
    "balances-at-start:last_3m",
    "net-worth:last_3m",
    "sankey:last_3m:cashflow",
    "settings",
  ]);
});

/**
 * The guarantee `packages/api/src/cache.rs` is written around: an edit followed by a look at
 * the figures it moved shows the server's new answer, never the one from before the edit.
 * Driven through the app's own UI so the write takes the same path a person's does.
 *
 * Restores the account afterwards: this spec shares the seeded database with every other one.
 */
test("after an edit, the dashboard waits for the server instead of drawing the pre-edit figure", async ({
  page,
}) => {
  await goto(page, "/");
  const before = await page.locator(FIGURE).innerText();

  await goto(page, "/settings/accounts");
  const row = page.locator(".acct", { hasText: "Family Home" });
  const toggle = row.getByRole("checkbox", { name: "Count Family Home toward net worth" });
  const label = row.locator("label.switch");
  await expect(toggle).toBeChecked();

  try {
    await label.click();
    await expect(toggle).not.toBeChecked();
    // The pre-edit dashboard is gone from storage, not merely superseded.
    await expect
      .poll(async () => Object.keys((await readStore(page))?.entries ?? {}))
      .not.toContain("net-worth:last_3m");

    const release = await holdReports(page);
    // In-app navigation, not a reload: the case where a kept figure is most tempting to reuse.
    await page.goto("/#/");
    await expect(page.locator("main .spinner")).toBeVisible();
    await expect(page.locator(FIGURE)).toHaveCount(0);

    release();
    await expect(page.locator(FIGURE)).toBeVisible();
    await expect(page.locator(FIGURE)).not.toHaveText(before);
    await expect(page.locator(FRESH)).toBeVisible();
  } finally {
    await page.unrouteAll({ behavior: "ignoreErrors" });
    await goto(page, "/settings/accounts");
    if (!(await toggle.isChecked())) await label.click();
    await expect(toggle).toBeChecked();
  }
});

test("figures kept under another API shape are discarded, not drawn", async ({ page }) => {
  await goto(page, "/");
  const fresh = await page.locator(FIGURE).innerText();
  const store = await storeWithSentinel(page);
  const current = store.v;
  await writeStore(page, { ...store, v: "1+an-older-schema" });

  const release = await holdReports(page);
  await page.reload();

  await expect(page.locator("main .spinner")).toBeVisible();
  await expect(page.locator(FIGURE)).toHaveCount(0);

  release();
  await expect(page.locator(FIGURE)).toHaveText(fresh);
  // …and what the new build keeps replaces the old store rather than sitting beside it.
  await expect.poll(async () => (await readStore(page))?.v).toBe(current);
});

test("a refresh that fails leaves the kept figures, still marked, and says how old they are", async ({
  page,
}) => {
  await goto(page, "/");
  const kept = await page.locator(FIGURE).innerText();

  await page.route("**/api/reports/**", (route) => route.abort("internetdisconnected"));
  await page.reload();

  await expect(page.locator(".last-known").first()).toContainText("Couldn't refresh");
  await expect(page.locator(FIGURE)).toHaveText(kept);
  await expect(page.locator(STALE)).toBeVisible();
  // The quiet line replaces the error banner a page with nothing to show still gets.
  await expect(page.locator(".error-banner")).toHaveCount(0);
});
