import { test, expect } from "./fixtures";
import type { Page } from "@playwright/test";

/**
 * The transactions page on a phone.
 *
 * It used to be a desktop table folded into cards: a page title and three buttons, a three-row
 * stat card, a search card, a sort header, and then a card per transaction carrying a checkbox,
 * an avatar, three pills and a picker — about 600px of a 874px screen before the second
 * transaction. The Filter popover also hung off a document-level click listener, over a DOM
 * heavy enough (every row carried a <select> of every category) that a tap landed late and
 * sometimes closed what it had just opened.
 *
 * The suite runs in a 402x874 mobile context (playwright.config.ts), so these are ordinary tests.
 */

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

test("a phone row is a line of text, not a card of controls", async ({ page }) => {
  await goto(page, "/transactions");
  const first = page.locator(".tx-row").first();
  await expect(first).toBeVisible();

  // Two lines, 56–70px. A row with a pill and a badge row was ~110.
  const box = await first.boundingBox();
  expect(box!.height, "a transaction row is taller than two lines of text").toBeLessThan(72);
  expect(box!.height, "a transaction row is too short to tap").toBeGreaterThanOrEqual(44);

  // None of the per-row controls: those live in the row's sheet.
  expect(await page.locator(".tx-row select").count(), "a row still carries an inline picker").toBe(0);
  expect(
    await page.locator(".tx-row input[type=checkbox]").count(),
    "checkboxes show outside select mode",
  ).toBe(0);
  expect(await page.locator(".tx-head").count(), "the desktop column header is still rendered").toBe(0);

  // The first day's heading is on the first screen, not under three cards.
  const head = await page.locator(".day-head").first().boundingBox();
  expect(head!.y, "the list starts too far down the screen").toBeLessThan(140);
});

test("search, filter and new float at the bottom, and the top bar is only the period", async ({ page }) => {
  await goto(page, "/transactions");
  const bar = page.locator(".subbar");
  await expect(bar.getByLabel("Time range")).toBeVisible();
  await expect(bar.getByRole("searchbox")).toHaveCount(0);
  // The tab bar already says where you are.
  await expect(bar.locator(".breadcrumb")).toBeHidden();
  expect((await bar.boundingBox())!.height, "the top bar wrapped").toBeLessThan(64);
  await expect(page.locator(".statcard")).toHaveCount(0);

  // The thumb's end of the screen: a row above the tab bar, inside the screen's edges.
  const dock = (await page.locator(".dock").boundingBox())!;
  const rail = (await page.locator(".rail").boundingBox())!;
  expect(dock.y + dock.height, "the dock sits under the tab bar").toBeLessThanOrEqual(rail.y);
  expect(rail.y - (dock.y + dock.height), "the dock floats far above the tab bar").toBeLessThan(24);
  expect(dock.x + dock.width, "the dock is off the right edge").toBeLessThanOrEqual(402);
  for (const name of ["Filter", "New transaction"]) {
    const b = (await page.locator(".dock").getByRole("button", { name, exact: true }).boundingBox())!;
    expect(Math.min(b.width, b.height), `${name} is too small to tap`).toBeGreaterThanOrEqual(44);
  }
  const search = (await page.locator(".dock").getByRole("searchbox").boundingBox())!;
  expect(search.width, "search is squeezed to nothing").toBeGreaterThan(150);

  // The list starts straight under the top bar.
  const head = (await page.locator(".day-head").first().boundingBox())!;
  expect(head.y, "more chrome above the list than the top bar").toBeLessThan(120);
});

test("the filter sheet stays open while choosing, and closes when asked to", async ({ page }) => {
  await goto(page, "/transactions");
  await expect(page.locator(".tx-row").first()).toBeVisible();
  const dialog = page.getByRole("dialog", { name: "Filter and sort" });
  const open = () => page.getByRole("button", { name: "Filter", exact: true }).click();

  await open();
  await expect(dialog).toBeVisible();
  // Choosing a filter applies it and leaves the sheet where it is — the old popover closed on
  // whatever the page did next.
  await dialog.getByLabel("Filter by category").selectOption("none");
  await page.waitForTimeout(600);
  await expect(dialog).toBeVisible();
  await expect(page.locator(".chip", { hasText: "Uncategorised" })).toBeVisible();
  await expect(page.locator(".tb-badge")).toHaveText("1");

  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(dialog).toBeHidden();

  await open();
  await expect(dialog).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // The scrim, tapped above the sheet.
  await open();
  await expect(dialog).toBeVisible();
  await page.mouse.click(200, 40);
  await expect(dialog).toBeHidden();

  // And the page behind it was never left locked.
  await expect(page.locator("body")).not.toHaveClass(/scroll-locked/);
});

test("a row swipes to reveal Delete, and deleting asks first", async ({ page }) => {
  // Its own transaction, so the shared seed is left exactly as it was found.
  await goto(page, "/transactions?range=last_30");
  await page.getByRole("button", { name: "New transaction" }).click();
  await page.getByPlaceholder("-12.50").fill("-1.23");
  await page.getByLabel("Description").fill("Swipe me away");
  await page.getByRole("button", { name: "Save transaction" }).click();

  const row = page.locator(".tx-row", { hasText: "Swipe me away" });
  await expect(row).toBeVisible();
  const box = (await row.boundingBox())!;
  const y = box.y + box.height / 2;

  await page.mouse.move(box.x + box.width - 30, y);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width - 190, y, { steps: 8 });
  await page.mouse.up();

  // Revealed, and the swipe did not also count as a tap on the row.
  const del = page.getByRole("button", { name: "Delete", exact: true });
  await expect(del).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);

  await del.click();
  const confirm = page.getByRole("dialog", { name: "Delete transaction?" });
  await expect(confirm).toContainText("Swipe me away");
  // Declining leaves it exactly where it was.
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(row).toBeVisible();

  const reveal = async () => {
    const b = (await row.boundingBox())!;
    await page.mouse.move(b.x + b.width - 30, y);
    await page.mouse.down();
    await page.mouse.move(b.x + b.width - 190, y, { steps: 8 });
    await page.mouse.up();
  };
  await reveal();
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page
    .getByRole("dialog", { name: "Delete transaction?" })
    .getByRole("button", { name: "Delete", exact: true })
    .click();
  await expect(page.locator(".tx-row", { hasText: "Swipe me away" })).toHaveCount(0);
});

test("tapping a row opens it for editing in a sheet", async ({ page }) => {
  await goto(page, "/transactions");
  await page.locator(".tx-row .tx-tap").first().click();
  const sheet = page.getByRole("dialog", { name: "Transaction" });
  await expect(sheet).toBeVisible();
  await expect(sheet.getByLabel("Category")).toBeAttached();
  await expect(sheet.getByRole("button", { name: "Delete transaction" })).toBeVisible();
  await sheet.getByRole("button", { name: "Done" }).click();
  await expect(sheet).toBeHidden();
});

/**
 * Narrower than any phone: a Split View window, or a text size that leaves a column of 250px.
 * The layout has to give way by shrinking and truncating, never by wrapping into a taller row
 * or pushing the page sideways.
 */
test.describe("in a very narrow window", () => {
  test.use({ viewport: { width: 280, height: 640 } });

  test("nothing overflows and every amount is on screen", async ({ page }) => {
    await goto(page, "/transactions");
    await expect(page.locator(".tx-row").first()).toBeVisible();

    const geom = await page.evaluate(() => {
      const de = document.documentElement;
      const outside = [...document.querySelectorAll<HTMLElement>(".tx-row .amt-cell, .dock > *")]
        .map((el) => ({ cls: el.className, r: el.getBoundingClientRect() }))
        .filter(({ r }) => r.width > 0 && (r.right > de.clientWidth + 1 || r.left < -1))
        .map(({ cls }) => String(cls));
      return { overflow: de.scrollWidth - de.clientWidth, outside };
    });
    expect(geom.overflow, "the page scrolls sideways").toBeLessThanOrEqual(1);
    expect(geom.outside, "something runs off the screen").toEqual([]);

    // The bar stays one row, rather than wrapping into the two it used to be.
    const bar = await page.locator(".subbar").boundingBox();
    expect(bar!.height, "the top bar wrapped").toBeLessThan(72);
  });
});
