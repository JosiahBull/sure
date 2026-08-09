import { type Page } from "@playwright/test";

import { DEMO_WHEN } from "./demo-date";
import { test, expect } from "./fixtures";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

test("overview shows net worth, category breakdown and the money-flow sankey", async ({ page }) => {
  await goto(page, "/");
  // Exact: the overview also carries a "Net worth by person" card, which a substring
  // match would tie with the headline "Net worth" one.
  await expect(page.getByRole("heading", { name: "Net worth", exact: true })).toBeVisible();
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
  // Two scoped shots rather than one `fullPage: true`, since Sure started shipping 72 rules of
  // its own (migration 0026): the whole page is now ~22,000px, nine tenths of it near-identical
  // default rows. That is not merely a 1.3MB baseline to review — `maxDiffPixelRatio: 0.03`
  // over an image that tall is ~660px of height free to change without failing, so the check
  // that was meant to catch a layout regression could no longer see one. The viewport covers
  // the rules list's own layout at the top of the page; the audit log, which is what the
  // `.run-when` rewriting above exists for, gets its own element shot.
  await expect(page).toHaveScreenshot("rules.png");
  await expect(page.locator(".card", { hasText: "Audit log" })).toHaveScreenshot("rules-audit.png");
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

test("every column header sorts the transactions list", async ({ page }) => {
  await goto(page, "/transactions");
  await expect(page.locator(".tx-row").first()).toBeVisible();
  // Read out of the DOM rather than off the model: these assertions are about the order the
  // page actually renders, which is the thing a frozen sort key used to get wrong silently.
  const amounts = () =>
    page
      .locator(".tx-row .amt-cell")
      .evaluateAll((els) => els.map((el) => parseFloat((el.textContent ?? "").replace(/[^0-9.-]/g, ""))));
  const texts = async (sel: string) => (await page.locator(sel).allInnerTexts()).map((s) => s.trim().toLowerCase());
  const header = (name: string) => page.getByRole("button", { name: new RegExp(`^${name} —`) });

  // The default is date/newest-first, the one order the day-grouped cards are valid for.
  await expect(page.locator(".day-group").first()).toBeVisible();

  // Amount's first click is descending — biggest income first — and the day cards give way to
  // the flat list, since a day heading assumes consecutive rows share a day.
  await header("Amount").click();
  await expect(page.locator(".day-group")).toHaveCount(0);
  const desc = await amounts();
  expect(desc.length).toBeGreaterThan(1);
  expect(desc).toEqual([...desc].sort((a, b) => b - a));
  // ...and the sorted view is a shareable link, like every other filter on this page.
  expect(page.url()).toContain("sort=amount");

  // Clicking the column that's already active flips it, so both directions are one click away.
  await header("Amount").click();
  expect(await amounts()).toEqual([...(await amounts())].sort((a, b) => a - b));
  expect(page.url()).toContain("dir=asc");

  // Names sort A→Z on the name the row actually shows (merchant-preferred), not the raw
  // description behind it. Plain string order, matching the comparison `sortValue` feeds.
  await header("transaction").click();
  const names = await texts(".tx-row .tx-name");
  expect(names).toEqual([...names].sort());

  // Categories sort A→Z with uncategorised rows clustered at the front rather than filed
  // under "U" — the empty-string sort key in `sortValue`.
  await header("Category label").click();
  const cats = await texts(".tx-row .cat-pill > .ell");
  const named = cats.filter((c) => c !== "uncategorised");
  expect(cats.slice(0, cats.length - named.length).every((c) => c === "uncategorised")).toBe(true);
  expect(named).toEqual([...named].sort());

  // Date brings the grouped view back, so it is never a dead end.
  await header("date").click();
  await expect(page.locator(".day-group").first()).toBeVisible();
});

test("the category filter can select the rows that have no category", async ({ page }) => {
  await goto(page, "/transactions");
  await expect(page.locator(".tx-row").first()).toBeVisible();
  const pills = () => page.locator(".tx-row .cat-pill > .ell");
  // The seed leaves some rows uncategorised, and not every row is — otherwise the filter
  // below would be indistinguishable from no filter at all.
  const before = await pills().allInnerTexts();
  expect(before.some((c) => c.trim() === "Uncategorised")).toBe(true);
  expect(before.some((c) => c.trim() !== "Uncategorised")).toBe(true);

  await page.getByRole("button", { name: "Filter" }).click();
  await page.getByLabel("Filter by category").selectOption("none");

  // Only uncategorised rows survive, and at least one does.
  await expect(pills().first()).toBeVisible();
  for (const c of await pills().allInnerTexts()) expect(c.trim()).toBe("Uncategorised");
  // It gets a removable chip like every other filter, and round-trips through the URL.
  await expect(page.locator(".chip", { hasText: "Uncategorised" })).toBeVisible();
  expect(page.url()).toContain("category=none");

  // Reloading the shared link restores the filter rather than dropping to "all categories".
  await page.reload();
  await page.waitForLoadState("networkidle");
  await expect(pills().first()).toBeVisible();
  for (const c of await pills().allInnerTexts()) expect(c.trim()).toBe("Uncategorised");
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
