import { type Page } from "@playwright/test";

import { DEMO_WHEN } from "./demo-date";
import { test, expect } from "./fixtures";

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
  // The Sankey (formerly its own tab) is now the last card on the overview. Its nodes
  // and flows are both <path> (nodes have fill, flows use fill:none), so assert DOM
  // presence rather than the visibility heuristic.
  await expect(page.getByRole("heading", { name: "Money flow" })).toBeVisible();
  await expect(page.locator("svg path")).not.toHaveCount(0);
  await expect(page).toHaveScreenshot("overview.png", { fullPage: true });
});

test("rules lists the seeded rule and its audit run", async ({ page }) => {
  await goto(page, "/settings/rules");
  // The rule name now appears both in the Active rules card and the audit log, so scope.
  await expect(page.getByText("Supermarkets → Groceries").first()).toBeVisible();
  await expect(page.getByRole("heading", { name: "Audit log" })).toBeVisible();
  // The seed ran the rule, so the audit log lists a run under its rule's name.
  await expect(page.locator("table tbody")).toContainText("Supermarkets → Groceries");
  // The audit log's "When" is stamped by SQLite's own clock when the seed runs the rules —
  // the one date on the page that neither SEED_TODAY nor the fixed browser clock reaches.
  // Rewritten to the pinned date rather than masked, for two reasons: `.table` sizes its
  // columns from their content, so a different-length date is free to move everything
  // beside it — a shift no mask over the cell itself would contain — and a real date keeps
  // the snapshot showing what the page actually looks like, where a mask would leave a
  // block of colour. The column's contents stay covered textually, here and in
  // rules-builder.spec.ts.
  await page
    .locator("table tbody .run-when")
    .evaluateAll((els, when) => els.forEach((el) => (el.textContent = when)), DEMO_WHEN);
  await expect(page).toHaveScreenshot("rules.png", { fullPage: true });
});

test("accounts show share vesting and property paid-off %", async ({ page }) => {
  await goto(page, "/settings/accounts");
  // Exact: the row's own summary line now reads "Single Family Home · Wellington" (the
  // property's subtype), which a substring match would tie with the account name.
  await expect(page.getByText("Family Home", { exact: true })).toBeVisible();
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

test("transactions list renders, preferring a transaction's merchant name over its raw description", async ({ page }) => {
  await goto(page, "/transactions");
  await expect(page.getByRole("heading", { name: "Transactions" })).toBeVisible();
  // The default (date-sorted) view groups rows by day as ".tx-row" divs, not a <table> —
  // merchant support shows as the row's primary name (txName prefers merchant over the
  // raw description) rather than a separate "Merchant" column.
  await expect(page.locator(".tx-row").first()).toBeVisible();
  await expect(page.locator(".tx-row .tx-name").first()).not.toBeEmpty();
  await expect(page).toHaveScreenshot("transactions.png", { fullPage: true });
});

test("merchants settings page lists seeded merchants", async ({ page }) => {
  await goto(page, "/settings/merchants");
  await expect(page.getByRole("heading", { name: "Merchants", exact: true })).toBeVisible();
  // A seeded merchant is listed.
  await expect(page.getByText("Netflix").first()).toBeVisible();
  await expect(page).toHaveScreenshot("settings-merchants.png", { fullPage: true });
});

test("preferences settings page exposes config backup", async ({ page }) => {
  await goto(page, "/settings/preferences");
  await expect(page.getByRole("heading", { name: "Backup & restore" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Export JSON" })).toBeVisible();
  await expect(page).toHaveScreenshot("settings-preferences.png", { fullPage: true });
});

test("can add a transaction and see it in the list", async ({ page }) => {
  await goto(page, "/transactions");
  await page.getByRole("button", { name: "New transaction" }).click();
  await page.getByPlaceholder("-12.50").fill("-42.50");
  await page.getByLabel("Description").fill("Playwright test coffee");
  await page.getByRole("button", { name: "Save transaction" }).click();
  await expect(page.getByText("Playwright test coffee")).toBeVisible();
});

// Mutates shared state, so it runs after every snapshot assertion above. Drives the
// real AccountForm: picking a kind reveals that kind's typed fields, and creating
// persists them (exercising the major→minor / metadata-building conversions).
test("can create an account with typed metadata via the form", async ({ page }) => {
  await goto(page, "/settings/accounts");
  await page.getByRole("button", { name: "+ Add account" }).click();

  // By role with an exact name: a plain getByLabel("Type") would also match the profile's own
  // "Subtype" select, and getByLabel(…, { exact: true }) matches neither — a wrapping <label>'s
  // accessible name picks up the selected <option>'s text ("Type Bank"), which the role-based
  // accessible-name computation for the control itself does not.
  await page.getByRole("combobox", { name: "Type", exact: true }).selectOption("vehicle");
  await page.getByLabel("Name", { exact: true }).fill("Test Van");

  // Make/model/year identify a vehicle and are required, as is a starting value — the server
  // seeds that as the account's first valuation, so it can't be left for a second request.
  await page.getByLabel("Make", { exact: true }).fill("Ford");
  await page.getByLabel("Model", { exact: true }).fill("Transit");
  await page.getByLabel("Year", { exact: true }).fill("2019");
  await page.getByLabel("Estimated value", { exact: true }).fill("28500");

  // Identifiers rather than setup, so they live behind the collapsed disclosure.
  await page.getByText("Additional details").click();
  await page.getByLabel("Nickname", { exact: true }).fill("Vanny");
  await page.getByLabel("Plate", { exact: true }).fill("VAN999");
  await page.getByRole("button", { name: "Create" }).click();

  // It appears under Assets with its metadata summary rendered from the stored fields.
  const row = page.locator(".acct", { hasText: "Test Van" });
  await expect(row).toBeVisible();
  await expect(row).toContainText("Vanny");
  await expect(row).toContainText("VAN999");
});
