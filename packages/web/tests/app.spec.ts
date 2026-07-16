import { test, expect, type Page } from "@playwright/test";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

test("overview shows net worth, category breakdown and the money-flow sankey", async ({ page }) => {
  await goto(page, "/");
  await expect(page.getByRole("heading", { name: "Net worth" })).toBeVisible();
  await expect(page.getByText("Where money went")).toBeVisible();
  // Seeded expense categories roll up to top-level buckets (appears in the pie legend
  // and the sankey, so scope to the first match).
  await expect(page.getByText("Housing").first()).toBeVisible();
  // The Sankey (formerly its own tab) is now the last card on the overview. Its paths
  // use fill:none, so assert DOM presence rather than the visibility heuristic.
  await expect(page.getByRole("heading", { name: "Money flow" })).toBeVisible();
  await expect(page.locator("svg rect").first()).toBeVisible();
  await expect(page.locator("svg path")).not.toHaveCount(0);
  await expect(page).toHaveScreenshot("overview.png", { fullPage: true });
});

test("rules lists the seeded rule and its audit run", async ({ page }) => {
  await goto(page, "/rules");
  // The rule name now appears both in the Active rules card and the audit log, so scope.
  await expect(page.getByText("Supermarkets → Groceries").first()).toBeVisible();
  await expect(page.getByRole("heading", { name: "Audit log" })).toBeVisible();
  // The seed ran the rule, so the audit log lists a run under its rule's name.
  await expect(page.locator("table tbody")).toContainText("Supermarkets → Groceries");
  await expect(page).toHaveScreenshot("rules.png", { fullPage: true });
});

test("accounts show share vesting and property paid-off %", async ({ page }) => {
  await goto(page, "/accounts");
  await expect(page.getByText("Family Home")).toBeVisible();
  await expect(page.getByText("Home Loan", { exact: true })).toBeVisible();

  // Private-shares equity (vesting).
  await page.locator(".acct", { hasText: "Startco Options" }).getByRole("button", { name: "Equity" }).click();
  await expect(page.getByText(/vested/)).toBeVisible();

  // Property equity: value − secured loans => paid-off %. Expanding this collapses the
  // shares panel, so the screenshot captures the property view.
  await page.locator(".acct", { hasText: "Family Home" }).getByRole("button", { name: "Equity" }).click();
  await expect(page.getByText(/paid off/)).toBeVisible();
  await expect(page).toHaveScreenshot("accounts.png", { fullPage: true });
});

test("transactions list renders with a merchant column", async ({ page }) => {
  await goto(page, "/transactions");
  await expect(page.getByRole("heading", { name: "Transactions" })).toBeVisible();
  await expect(page.getByRole("columnheader", { name: "Merchant" })).toBeVisible();
  await expect(page.locator("table tbody tr").first()).toBeVisible();
  await expect(page).toHaveScreenshot("transactions.png", { fullPage: true });
});

test("settings exposes merchant management and config backup", async ({ page }) => {
  await goto(page, "/settings");
  await expect(page.getByRole("heading", { name: "Merchants" })).toBeVisible();
  // A seeded merchant is listed.
  await expect(page.getByText("Netflix").first()).toBeVisible();
  await expect(page.getByRole("heading", { name: "Backup & restore" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Export JSON" })).toBeVisible();
  await expect(page).toHaveScreenshot("settings.png", { fullPage: true });
});

test("can add a transaction and see it in the list", async ({ page }) => {
  await goto(page, "/transactions");
  await page.getByRole("button", { name: "+ Add" }).click();
  await page.getByPlaceholder("-12.50").fill("-42.50");
  await page.getByLabel("Description").fill("Playwright test coffee");
  await page.getByRole("button", { name: "Save transaction" }).click();
  await expect(page.getByText("Playwright test coffee")).toBeVisible();
});
