import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * How money is labelled.
 *
 * The base currency is the one the reader is already thinking in, so it prints a bare `$` and
 * everything else carries its ISO code. The second half is not decoration: the narrow symbol
 * for NZD, USD and AUD is the same `$`, so a symbol on a non-base currency would read as the
 * base one — a US holding and a NZ one would look identical in a net-worth list.
 */

/** `Intl` separates a currency code from its number with a non-breaking space. */
const NB = " ";

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/** Every money figure on the accounts page, which lists one account per currency in the seed. */
const accountFigures = (page: Page) => page.locator(".acct .tabular");

async function setBaseCurrency(page: Page, code: string) {
  await goto(page, "/settings/preferences");
  await page.getByRole("combobox").first().selectOption(code);
  await expect(page.locator(".badge")).toContainText("Base currency updated");
}

test("the base currency prints a bare symbol and everything else prints its code", async ({
  page,
}) => {
  await goto(page, "/settings/accounts");
  const figures = await accountFigures(page).allInnerTexts();
  expect(figures.length, "no money on the accounts page").toBeGreaterThan(3);

  // The seed holds NZD accounts and a USD one, which is the whole point of asserting here.
  expect(figures.some((t) => /^-?\$[\d,]/.test(t)), `no bare-symbol figure in ${figures}`).toBe(true);
  expect(
    figures.some((t) => t.includes(`USD${NB}`)),
    `no USD figure in ${figures}`,
  ).toBe(true);

  // And the old form is gone: "NZ$" was the prefix on every figure in the app.
  expect(figures.join(" ")).not.toContain("NZ$");
});

test("the dashboard's figures follow the same rule", async ({ page }) => {
  await goto(page, "/");
  // The pie legends are where the prefix was most obviously noise — a column of NZ$ down a card
  // whose every row is the same currency.
  const legend = page.locator(".card", { hasText: "Where money went" }).locator(".legend-row");
  await expect(legend.first()).toBeVisible();
  const rows = (await legend.allInnerTexts()).join(" ");
  expect(rows).toMatch(/\$[\d,]/);
  expect(rows).not.toContain("NZ$");
});

test("changing the base currency in settings flips which figures carry a prefix", async ({
  page,
}) => {
  await goto(page, "/settings/accounts");
  const before = (await accountFigures(page).allInnerTexts()).join(" ");
  expect(before, "expected a bare-symbol NZD figure to start from").toMatch(/\$[\d,]/);
  expect(before).toContain(`USD${NB}`);

  try {
    await setBaseCurrency(page, "USD");

    // Navigating, not reloading: the setting is republished to the rest of the app as it is
    // saved, so a page that was already loaded must not keep labelling against the old base.
    await goto(page, "/settings/accounts");
    const after = (await accountFigures(page).allInnerTexts()).join(" ");
    expect(after, "NZD should carry its code once it is not the base").toContain(`NZD${NB}`);
    expect(after, "USD should lose its code once it is the base").not.toContain(`USD${NB}`);
  } finally {
    // Every later spec reads money off the screen, so this cannot be left flipped.
    await setBaseCurrency(page, "NZD");
  }

  await goto(page, "/settings/accounts");
  const restored = (await accountFigures(page).allInnerTexts()).join(" ");
  expect(restored).toContain(`USD${NB}`);
  expect(restored).not.toContain(`NZD${NB}`);
});
