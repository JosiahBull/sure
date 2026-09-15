import { type APIRequestContext, type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * The dashboard's Assets and Liabilities cards.
 *
 * Each row is a kind — Real estate, Mortgage — summarising the accounts under it, and the bar
 * above is the same figures as widths. The three things asserted here are the three a reader
 * does with them: read what a slice is without leaving the bar, follow one to the transactions
 * behind it, and see which way a kind moved over the period on screen rather than by comparing
 * two balances in their head.
 */

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/** The card pair, scoped past the net-worth card above them (which also says "assets"). */
const cards = (page: Page) =>
  page.locator(".grid.two").filter({ has: page.locator(".weightbar") }).first();
const assetsCard = (page: Page) => cards(page).locator("section.card").first();

test("the weight bar has a segment per kind, and hovering one says what it is", async ({ page }) => {
  await goto(page, "/");
  const card = assetsCard(page);
  await expect(card.getByRole("heading", { name: "Assets" })).toBeVisible();

  const rows = await card.locator(".legend-row .ell").allInnerTexts();
  const segments = card.locator(".weightbar .seg");
  expect(rows.length, "the card listed no kinds").toBeGreaterThan(1);
  await expect(segments).toHaveCount(rows.length);

  // No tooltip until something is pointed at.
  await expect(card.locator(".tip")).toHaveCount(0);

  await segments.first().hover();
  const tip = card.locator(".tip");
  await expect(tip).toBeVisible();
  // The biggest slice is the first row, and the tooltip restates that row: its name, its share
  // and its money.
  await expect(tip).toContainText(rows[0]);
  await expect(tip).toContainText("%");
  // A bare "$": the base currency needs no prefix — see tests/currency.spec.ts.
  await expect(tip).toContainText("$");

  // A slice a few pixels wide is the one a title attribute made unhittable; the target is the
  // full row height even though the bar is 8px.
  const smallest = segments.nth(rows.length - 1);
  expect((await smallest.boundingBox())!.width, "the last slice is not a narrow one").toBeLessThan(40);
  await smallest.hover();
  await expect(card.locator(".tip")).toContainText(rows[rows.length - 1]);

  await page.mouse.move(0, 0);
  await expect(card.locator(".tip")).toHaveCount(0);
});

test("the slices are reachable without a mouse", async ({ page }) => {
  await goto(page, "/");
  const card = assetsCard(page);

  // `role="img"` on the bar would make this zero: it renders the whole subtree presentational,
  // so the buttons inside it would be invisible to assistive tech while looking fine on screen.
  const buttons = card.locator(".weightbar").getByRole("button");
  const rows = await card.locator(".legend-row .ell").allInnerTexts();
  await expect(buttons).toHaveCount(rows.length);

  // Each carries what the tooltip shows, since a tooltip is a pointer affordance and this is
  // the same information for everyone else.
  const first = buttons.first();
  await expect(first).toHaveAttribute("aria-label", new RegExp(`^${rows[0]}, \\d`));
  await expect(first).toHaveAttribute("aria-label", /Click for .+ transactions/);

  // Focus shows the tooltip, and Enter does what it says.
  await card.locator(".weightbar .seg").first().focus();
  await expect(card.locator(".tip")).toBeVisible();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/transactions\?account=\d+&range=/);
});

test("clicking a slice opens the transactions behind it", async ({ page }) => {
  await goto(page, "/");
  const card = assetsCard(page);
  const segment = card.locator(".weightbar .seg").first();

  // The tooltip says what the click will do, so the two cannot drift apart.
  await segment.hover();
  await expect(card.locator(".tip")).toContainText(/Click for .+'s transactions/);

  await segment.click();
  await expect(page).toHaveURL(/#\/transactions\?account=\d+&range=/);
  // The list really is filtered, not merely navigated to.
  await expect(page.locator(".chip")).toContainText(/\w/);
});

test("a kind holding several accounts opens the list instead of guessing", async ({ page, playwright, baseURL }) => {
  // Seeded on the spot: the demo household has exactly one account of every kind, so the
  // ambiguous case — which is the whole reason the click is not always a navigation — cannot
  // be reached without one.
  const request: APIRequestContext = await playwright.request.newContext({ baseURL });
  const created = await request.post("/api/accounts", {
    data: {
      name: "Second Everyday",
      kind: "bank",
      currency_code: "NZD",
      institution: "ANZ",
      ownership: { kind: "person", person_id: 1 },
      opening_balance_minor: 540_000,
      opening_balance_date: "2025-06-01",
    },
  });
  expect(created.ok(), `create -> ${created.status()} ${await created.text()}`).toBe(true);
  const account = (await created.json()) as { id: number };

  try {
    await goto(page, "/");
    const card = assetsCard(page);
    const rows = await card.locator(".legend-row .ell").allInnerTexts();
    const bank = rows.indexOf("Bank");
    expect(bank, "no Bank row to test the ambiguous case with").toBeGreaterThanOrEqual(0);

    const segment = card.locator(".weightbar .seg").nth(bank);
    await segment.hover();
    await expect(card.locator(".tip"), "the tooltip should offer the list, not one account")
      .toContainText("Click to list 2 accounts");

    await segment.click();
    // Stays put and opens the group, rather than picking one of the two on the reader's behalf.
    await expect(page).toHaveURL(/#\/$/);
    const subRows = card.locator(".sub-row");
    await expect(subRows).toHaveCount(2);

    // And each account underneath is the filter the bar could not offer.
    await subRows.first().click();
    await expect(page).toHaveURL(/#\/transactions\?account=\d+&range=/);
  } finally {
    // Put the household back, so the specs after this one see what they were seeded with.
    await request.delete(`/api/accounts/${account.id}`);
    await request.dispose();
  }
});

test("each kind shows how it moved over the period, signed and coloured", async ({ page }) => {
  await goto(page, "/");
  const card = assetsCard(page);

  // Real estate appreciates in the seeded history, and is the one kind no other spec adds an
  // account to — a group that gains a brand-new account gains its whole value as "change", so
  // an assertion about a specific figure has to pick a group nothing else touches.
  const gained = card.locator("li", { hasText: "Real estate" }).locator(".bs-change");
  await expect(gained).toHaveText(/^\+\d+\.\d%$/);
  await expect(gained).toHaveClass(/\bpos\b/);

  /*
   * And the rule itself, over every row on both cards: the sign and the colour agree.
   *
   * Asserted as an invariant rather than as two more example figures, because the examples are
   * what a preceding spec can move. It also states the part that is not obvious — green means
   * "better off", not "the number went up". A liability is held negative, so paying one down
   * moves it toward zero and comes out positive on both counts; that is the same rule, not an
   * exception to it.
   */
  const rows = cards(page).locator(".bs-change");
  const count = await rows.count();
  expect(count, "no change figures on either card to check").toBeGreaterThan(0);
  for (let i = 0; i < count; i++) {
    const text = (await rows.nth(i).innerText()).trim();
    const classes = (await rows.nth(i).getAttribute("class")) ?? "";
    if (text.startsWith("+")) {
      expect(classes, `${text} is not green`).toContain("pos");
    } else if (text.startsWith("-")) {
      expect(classes, `${text} is not red`).toContain("neg");
    } else {
      // A standstill rounds to "0.0%" and takes neither colour, so the column does not claim a
      // direction the figure does not have.
      expect(text, "an unsigned change should be a flat zero").toMatch(/^0\.0%$/);
      expect(classes).not.toContain("pos");
      expect(classes).not.toContain("neg");
    }
  }
});

test("the change figures follow the selected period", async ({ page }) => {
  await goto(page, "/");
  const row = assetsCard(page).locator("li", { hasText: "Real estate" }).locator(".bs-change");
  const twelveMonths = await row.innerText();

  await page.selectOption('select[aria-label="Time range"]', "last_30");
  // A month of a year's growth is a different number; if it were not, the figure would be
  // measured from something other than the period.
  await expect(row).not.toHaveText(twelveMonths);

  // "All time" starts before any history, so every account began at nothing and a percentage
  // would only restate the balance. The column goes quiet rather than claiming one.
  await page.selectOption('select[aria-label="Time range"]', "all");
  await expect(assetsCard(page).locator(".bs-change")).toHaveCount(0);
});
