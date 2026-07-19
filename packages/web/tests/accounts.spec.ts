import { test, expect, type Page } from "@playwright/test";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

// These tests never actually delete a seeded account (cancel / blocked paths only), so the
// shared demo database is left intact for the other specs.

test("deleting an account asks for confirmation first, and Cancel keeps it", async ({ page }) => {
  await goto(page, "/accounts");
  const row = page.locator(".acct", { hasText: "Sharesies (US)" });
  await row.getByRole("button", { name: "Delete Sharesies (US)" }).click();

  await expect(page.getByText(/This can't be undone/)).toBeVisible(); // confirmation, not an instant delete
  await page.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByText(/This can't be undone/)).toHaveCount(0);
  await expect(page.locator(".acct", { hasText: "Sharesies (US)" })).toBeVisible();
});

test("an asset with secured debts is blocked from deletion, naming the debts", async ({ page }) => {
  await goto(page, "/accounts");
  const row = page.locator(".acct", { hasText: "Family Home" });
  await row.getByRole("button", { name: "Delete Family Home" }).click();
  await page.getByRole("button", { name: "Delete", exact: true }).click();

  const banner = page.locator(".confirm .error-banner");
  await expect(banner).toContainText("Unlink or delete the debt secured against this account first");
  await expect(banner).toContainText("Home Loan"); // the secured debt is named
  await expect(page.locator(".acct", { hasText: "Family Home" })).toBeVisible();
});

test("Institution shows for banks but not shares (which use a broker)", async ({ page }) => {
  await goto(page, "/accounts");
  // Shares: a broker/platform field, no Institution.
  await page.locator(".acct", { hasText: "Sharesies (US)" }).getByRole("button", { name: "Edit" }).click();
  await expect(page.getByLabel("Broker / platform")).toBeVisible();
  await expect(page.getByLabel("Institution", { exact: true })).toHaveCount(0);
  // Bank: Institution is present.
  await page.locator(".acct", { hasText: "Everyday" }).getByRole("button", { name: "Edit" }).click();
  await expect(page.getByLabel("Institution", { exact: true })).toBeVisible();
});

test("clicking an account jumps to its filtered transactions", async ({ page }) => {
  await goto(page, "/accounts");
  await page
    .locator(".acct", { hasText: "Everyday" })
    .getByRole("button", { name: "View transactions for Everyday" })
    .click();

  await expect(page).toHaveURL(/#\/transactions\?account=\d+$/);
  await expect(page.getByRole("heading", { name: "Transactions" })).toBeVisible();

  const accountFilter = page.getByLabel("Filter by account");
  await expect(accountFilter).not.toHaveValue(""); // not "All accounts"
  const selectedLabel = await accountFilter.locator("option:checked").textContent();
  expect(selectedLabel).toBe("Everyday");

  // Every visible row belongs to the filtered account — not a mix. Columns are
  // checkbox, date, description, account, … so the account is the 4th cell.
  const accountCells = page.locator("table tbody tr td:nth-child(4)");
  await expect(accountCells.first()).toBeVisible();
  const distinctAccounts = new Set(await accountCells.allTextContents());
  expect([...distinctAccounts]).toEqual(["Everyday"]);
});

test("Edit and Delete on an account row don't trigger the transactions jump", async ({ page }) => {
  await goto(page, "/accounts");
  await page.locator(".acct", { hasText: "Everyday" }).getByRole("button", { name: "Edit" }).click();
  await expect(page).toHaveURL(/#\/accounts$/);
  await page.locator(".acct", { hasText: "Everyday" }).getByRole("button", { name: "Close" }).click();

  await page.locator(".acct", { hasText: "Everyday" }).getByRole("button", { name: "Delete Everyday" }).click();
  await expect(page).toHaveURL(/#\/accounts$/);
  await page.getByRole("button", { name: "Cancel" }).click();
});
