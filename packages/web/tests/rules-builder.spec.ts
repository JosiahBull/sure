import { test, expect, type Page } from "@playwright/test";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

async function expressionOf(page: Page, name: string): Promise<string | undefined> {
  const res = await page.request.get("/api/rules");
  const rules = (await res.json()) as Array<{ name: string; expression: string }>;
  return rules.find((r) => r.name === name)?.expression;
}

const SEED_SUPERMARKETS =
  "is_expense and (contains(lower(description), 'countdown') or contains(lower(description), 'new world') or contains(lower(description), \"pak'nsave\"))";

// Editing reconstructs the visual tree from the stored Zen; the seed's three OR'd
// `contains(...)` collapse into one multi-value "Description contains …" row (keeping
// the `"pak'nsave"` value), and saving the rebuilt tree preserves the exact expression.
test("editing a rule reconstructs it and round-trips on save", async ({ page }) => {
  await goto(page, "/rules");
  await page
    .locator(".rule", { hasText: "Supermarkets → Groceries" })
    .getByRole("button", { name: "Edit" })
    .click();

  await expect(page.getByRole("button", { name: "All", exact: true })).toBeVisible();
  await expect(page.locator(".tag", { hasText: "countdown" })).toBeVisible();
  await expect(page.locator(".tag", { hasText: "new world" })).toBeVisible();
  await expect(page.locator(".tag", { hasText: "pak'nsave" })).toBeVisible();

  await page.getByRole("button", { name: "Save changes" }).click();
  await expect(page.locator(".rule", { hasText: "Supermarkets → Groceries" })).toBeVisible();
  expect(await expressionOf(page, "Supermarkets → Groceries")).toBe(SEED_SUPERMARKETS);
});

// Building from scratch: pick a value, see the live match preview, save, and confirm
// the emitted Zen (checked via the API — it's intentionally not shown in the UI).
test("can build a new rule, preview matches, and save", async ({ page }) => {
  await goto(page, "/rules");
  await page.getByRole("button", { name: "+ New rule" }).click();
  await page.getByPlaceholder("Groceries").fill("Coffee shops");

  const valueInput = page.getByPlaceholder("e.g. countdown");
  await valueInput.fill("countdown");
  await valueInput.press("Enter");

  await expect(page.getByText(/Matches \d+ transaction/)).toBeVisible();

  await page.getByRole("button", { name: "Create rule" }).click();
  await expect(page.locator(".rule", { hasText: "Coffee shops" })).toBeVisible();
  expect(await expressionOf(page, "Coffee shops")).toBe("contains(lower(description), 'countdown')");

  // Clean up so the shared test database isn't perturbed for other specs.
  await page.locator(".rule", { hasText: "Coffee shops" }).getByRole("button", { name: "Delete rule" }).click();
  await expect(page.locator(".rule", { hasText: "Coffee shops" })).toHaveCount(0);
});

// The audit log expands a run into a per-transaction diff (what each change was).
test("audit log expands a run into a per-transaction diff", async ({ page }) => {
  await goto(page, "/rules");
  const runRow = page.locator("table tbody tr", { has: page.getByRole("button", { name: "Show changes" }) }).first();
  await runRow.getByRole("button", { name: "Show changes" }).click();

  const detail = page.locator(".detail-row").first();
  await expect(detail.locator(".txn-change").first()).toBeVisible(); // a changed transaction
  await expect(detail.locator(".txn-desc").first()).not.toBeEmpty(); // its description
  await expect(detail.locator(".txn-diff").first()).toContainText("→"); // before → after
});

// Each run is listed under the rule it applied (not just a generic "Single rule").
test("audit log shows which rule each run applied", async ({ page }) => {
  await goto(page, "/rules");
  await expect(page.getByRole("columnheader", { name: "Rule" })).toBeVisible();
  // The seed ran the supermarkets rule, so the audit body names it.
  await expect(page.locator("table tbody")).toContainText("Supermarkets → Groceries");
});

// The whole row is a toggle, not just the caret.
test("clicking anywhere on an audit row expands its diff", async ({ page }) => {
  await goto(page, "/rules");
  const runRow = page.locator("table tbody tr", { has: page.getByRole("button", { name: "Show changes" }) }).first();
  // Click the "Matched" cell — not the caret — and the row still expands.
  await runRow.getByRole("cell").nth(2).click();
  await expect(page.locator(".detail-row .txn-change").first()).toBeVisible();
  // Clicking again collapses it.
  await runRow.getByRole("cell").nth(2).click();
  await expect(page.locator(".detail-row")).toHaveCount(0);
});

// A changed transaction links through to it on the transactions page, highlighted.
test("a changed transaction links to the transactions page", async ({ page }) => {
  await goto(page, "/rules");
  const runRow = page.locator("table tbody tr", { has: page.getByRole("button", { name: "Show changes" }) }).first();
  await runRow.getByRole("button", { name: "Show changes" }).click();

  const firstTxn = page.locator(".detail-row .txn-line").first();
  await expect(firstTxn).toBeVisible();
  await firstTxn.click();

  await expect(page).toHaveURL(/#\/transactions\?tx=\d+/);
  await expect(page.locator("tr.highlight")).toBeVisible();
});

// Regression for the expanded audit-log diff's layout, on a mobile viewport:
//  • Vertical: the rows were given class="app", colliding with the global
//    `.app { min-height: 100dvh }` shell rule, which stretched every change row to the
//    full viewport height (~900px) and made an expanded run enormous.
//  • Horizontal: the audit table was ~409px wide on a 402px screen, so the diff's amount
//    sat off-screen and the page scrolled sideways.
test("audit-log diff rows stay compact and don't overflow horizontally", async ({ page }) => {
  await goto(page, "/rules");
  const runRow = page.locator("table tbody tr", { has: page.getByRole("button", { name: "Show changes" }) }).first();
  await runRow.getByRole("button", { name: "Show changes" }).click();

  const firstChange = page.locator(".detail-row .txn-change").first();
  await expect(firstChange).toBeVisible();

  // Each change row is two short lines — nowhere near a full viewport height.
  const height = await firstChange.evaluate((el) => el.getBoundingClientRect().height);
  expect(height).toBeLessThan(200);

  // The whole diff panel scales with its rows, not the viewport.
  const detail = page.locator(".detail-row td").first();
  const rows = await page.locator(".detail-row .txn-change").count();
  const panelHeight = await detail.evaluate((el) => el.getBoundingClientRect().height);
  expect(panelHeight).toBeLessThan(rows * 160 + 80);

  // Nothing forces the page wider than the viewport.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(1);
});

// A numeric field exercises the non-text value editor and the `abs_amount` mapping.
test("numeric field builds an amount comparison", async ({ page }) => {
  await goto(page, "/rules");
  await page.getByRole("button", { name: "+ New rule" }).click();
  await page.getByPlaceholder("Groceries").fill("Big spend");

  await page.getByLabel("Field", { exact: true }).selectOption({ label: "Amount" });
  await page.getByLabel("Condition", { exact: true }).selectOption({ label: "is greater than" });
  await page.getByRole("spinbutton").fill("100");

  await page.getByRole("button", { name: "Create rule" }).click();
  await expect(page.locator(".rule", { hasText: "Big spend" })).toBeVisible();
  expect(await expressionOf(page, "Big spend")).toBe("abs_amount > 100");

  await page.locator(".rule", { hasText: "Big spend" }).getByRole("button", { name: "Delete rule" }).click();
  await expect(page.locator(".rule", { hasText: "Big spend" })).toHaveCount(0);
});
