import { readFileSync } from "node:fs";
import path from "node:path";

import { type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * What a phone has to be able to do.
 *
 * The visual specs photograph the app; these assert the properties a photograph cannot, and
 * each one is here because it was broken:
 *
 *   - Nine of the eleven settings pages were unreachable. Their only navigation lived in a
 *     panel that `@media (max-width: 720px)` gave `width: 0; overflow: hidden`, so the links
 *     were laid out at x=102..370, clipped to nothing, and a tap on one landed on the page
 *     behind it. Nothing failed; the pages simply could not be opened.
 *   - The page scrolled sideways. The dashboard's document was 485px wide inside a 402px
 *     viewport, and because the rail and the sub-bar are sticky, scrolling sideways slid the
 *     content out from under them.
 *   - A transaction's *amount* was off-screen. The list's 560px-wide grid scrolled inside its
 *     own container, which put the one number anyone opens the page for behind a horizontal
 *     gesture nothing advertised.
 *
 * The suite already runs in a 402x874 mobile context (playwright.config.ts), so these are
 * ordinary tests rather than a separate project.
 */

/** The phone viewport the whole suite runs at, from playwright.config.ts. */
const PHONE = { width: 402, height: 874 };

/** Every route the shell can show, so a regression cannot hide on the page nobody checks. */
const ROUTES = [
  "/",
  "/transactions",
  "/settings/accounts",
  "/settings/import",
  "/settings/household",
  "/settings/providers",
  "/settings/tax",
  "/settings/preferences",
  "/settings/appearance",
  "/settings/scheduled",
  "/settings/categories",
  "/settings/rules",
  "/settings/merchants",
];

/** The settings pages whose only route in is the panel — Accounts is the landing page. */
const DRAWER_ONLY = ROUTES.filter((r) => r.startsWith("/settings/") && r !== "/settings/accounts");

async function goto(page: Page, route: string) {
  await page.goto(`/#${route}`);
  await page.waitForLoadState("networkidle");
}

/** How much wider than the viewport the document is. Zero, or the page scrolls sideways. */
const overhang = (page: Page) =>
  page.evaluate(() => {
    const de = document.documentElement;
    return Math.max(de.scrollWidth, document.body.scrollWidth) - de.clientWidth;
  });

test("no page scrolls sideways at phone width", async ({ page }) => {
  for (const route of ROUTES) {
    await goto(page, route);
    expect(await overhang(page), `${route} is wider than the screen`).toBeLessThanOrEqual(1);
  }
});

test("every settings page is reachable from a phone", async ({ page }) => {
  await goto(page, "/settings/accounts");

  // Closed to begin with — a phone must not land on a page covered by its own menu.
  await expect(page.locator(".shell")).toHaveClass(/panel-collapsed/);

  for (const route of DRAWER_ONLY) {
    // Reopen each time: the drawer closes itself on navigation, which is the next assertion.
    if (await page.locator(".shell.panel-collapsed").count()) {
      await page.getByRole("button", { name: /^Show settings menu$/ }).click();
    }
    const link = page.locator(`a[href='#${route}']`).first();
    // `click()` rather than a URL assertion on its own: a link clipped to zero width by an
    // overflow-hidden parent still *exists* and still has the right href — it just cannot be
    // tapped, which is exactly the bug this covers. Playwright's actionability check is the
    // part that catches it.
    await link.click();
    await expect(page).toHaveURL(new RegExp(`#${route.replace("/", "\\/")}$`));
    // And it gets out of the way once it has done its job.
    await expect(page.locator(".shell")).toHaveClass(/panel-collapsed/);
  }
});

test("the drawer closes on Escape and on a tap outside it", async ({ page }) => {
  await goto(page, "/");
  const shell = page.locator(".shell");
  const toggle = page.getByRole("button", { name: /^Show accounts panel$/ });

  await toggle.click();
  await expect(shell).not.toHaveClass(/panel-collapsed/);
  // Nothing behind an open drawer may scroll — on iOS a drag over the scrim otherwise takes
  // the page with it.
  await expect(page.locator("body")).toHaveClass(/scroll-locked/);
  await page.keyboard.press("Escape");
  await expect(shell).toHaveClass(/panel-collapsed/);

  await toggle.click();
  await expect(shell).not.toHaveClass(/panel-collapsed/);
  // The scrim's centre is behind the drawer itself, so tap the strip beside it — which is
  // where a thumb reaching for "not this" actually lands.
  await page.locator(".panel-scrim").click({ position: { x: PHONE.width - 20, y: 500 } });
  await expect(shell).toHaveClass(/panel-collapsed/);
});

test("a closed drawer is out of the tab order", async ({ page }) => {
  await goto(page, "/settings/accounts");
  // `inert`, not just hidden: the links are still laid out (the drawer slides rather than
  // collapsing), so without it they stay focusable and a keyboard or screen reader walks into
  // a menu that is off the side of the screen.
  await expect(page.locator(".panel-col")).toHaveAttribute("inert", "");
});

test("a transaction's amount is readable without scrolling sideways", async ({ page }) => {
  await goto(page, "/transactions");
  const row = page.locator(".tx-row").first();
  await expect(row).toBeVisible();

  const amount = row.locator(".amt-cell");
  await expect(amount).toBeVisible();
  const box = await amount.boundingBox();
  expect(box, "the amount cell has no box").not.toBeNull();
  expect(box!.x + box!.width, "the amount runs off the right of the screen").toBeLessThanOrEqual(
    PHONE.width,
  );
  expect(box!.x, "the amount starts off the left of the screen").toBeGreaterThanOrEqual(0);

  // And the list is not parked inside a horizontal scroller either — a visible amount that
  // scrolls away on the first sideways nudge is the same bug wearing a different hat.
  const scroller = page.locator(".tx-scroll");
  expect(
    await scroller.evaluate((el) => el.scrollWidth - el.clientWidth),
    "the transaction list still scrolls horizontally at phone width",
  ).toBeLessThanOrEqual(1);
});

test("the primary navigation is a bottom tab bar on a phone", async ({ page }) => {
  await goto(page, "/");
  const rail = page.locator(".rail");
  const box = await rail.boundingBox();
  expect(box).not.toBeNull();
  // Across the bottom, not down the left: the 80px column was a fifth of the screen.
  expect(box!.width).toBe(PHONE.width);
  expect(box!.y + box!.height).toBeGreaterThan(PHONE.height - 80);

  // And the content clears it, rather than ending underneath it.
  const clearance = await page.evaluate(
    () => parseFloat(getComputedStyle(document.querySelector(".main-col")!).paddingBottom) || 0,
  );
  expect(clearance).toBeGreaterThanOrEqual(box!.height);
});

/**
 * The tab bar has to be on the part of the screen you can *see*.
 *
 * A fixed element resolves `bottom: 0` against the layout viewport, and on a phone that is
 * taller than the visible area whenever the browser's toolbars are up — so the bar sat below
 * the fold and the app had no reachable navigation at all. This viewport is the smallest the
 * app supports and the one where Chromium's mobile emulation reproduces the gap exactly
 * (a 648px layout viewport inside 568px of visible screen), which is why the checks below
 * measure against `clientHeight` rather than against the configured viewport height: the two
 * are the same number at 402x874, which is how the bug got past the first version of this file.
 */
test.describe("on the smallest supported screen", () => {
  test.use({ viewport: { width: 320, height: 568 } });

  for (const route of ["/", "/transactions", "/settings/accounts"]) {
    test(`the tab bar is on screen and tappable on ${route}`, async ({ page }) => {
      await goto(page, route);

      const geom = await page.evaluate(() => {
        const de = document.documentElement;
        const r = document.querySelector(".rail")!.getBoundingClientRect();
        // Every tab, because the bar overflowing to the right clipped only the last one.
        const tabs = [...document.querySelectorAll(".rail .rail-item")].map((el) => {
          const b = el.getBoundingClientRect();
          return { href: el.getAttribute("href"), right: b.right, left: b.left };
        });
        return {
          bar: { top: r.top, bottom: r.bottom, right: r.right },
          tabs,
          visibleH: de.clientHeight,
          visibleW: de.clientWidth,
        };
      });
      expect(geom.bar.bottom, "the tab bar's bottom edge is below the visible viewport").toBeLessThanOrEqual(
        geom.visibleH + 1,
      );
      expect(geom.bar.top, "the tab bar starts below the visible viewport").toBeLessThan(geom.visibleH);
      expect(geom.bar.right, "the tab bar runs past the right of the screen").toBeLessThanOrEqual(
        geom.visibleW + 1,
      );
      for (const tab of geom.tabs) {
        expect(tab.right, `the ${tab.href} tab is clipped off the right`).toBeLessThanOrEqual(
          geom.visibleW + 1,
        );
        expect(tab.left, `the ${tab.href} tab starts off the left`).toBeGreaterThanOrEqual(-1);
      }

      // Tappable, not merely positioned: an actual click is what proves nothing is painted on
      // top of it, which is how this first surfaced.
      await page.locator(".rail a[href='#/settings/accounts']").click();
      await expect(page).toHaveURL(/#\/settings\/accounts$/);
    });
  }

  test("the bulk-action bar sits above the tab bar, not under the fold", async ({ page }) => {
    await goto(page, "/transactions");
    // Scroll a row clear of the sticky sub-bar before ticking it. Playwright's own
    // scroll-into-view uses "nearest", which parks the row directly beneath that bar and then
    // cannot click it — an artefact of the harness, not of the page: the app's one programmatic
    // scroll (the `?tx=` deep link) uses `block: "center"` and lands in open space.
    await page.evaluate(() => window.scrollBy(0, 400));
    const row = page.locator(".tx-row").filter({ has: page.locator("label.tx-check") }).nth(2);
    await row.locator("input[type=checkbox]").check();

    const geom = await page.evaluate(() => {
      const bulk = document.querySelector(".bulkbar")!.getBoundingClientRect();
      const rail = document.querySelector(".rail")!.getBoundingClientRect();
      return {
        bulkBottom: bulk.bottom,
        bulkTop: bulk.top,
        bulkRight: bulk.right,
        railTop: rail.top,
        visibleW: document.documentElement.clientWidth,
      };
    });
    expect(geom.bulkBottom, "the bulk bar overlaps the tab bar").toBeLessThanOrEqual(geom.railTop);
    expect(geom.bulkTop, "the bulk bar is off the top of the screen").toBeGreaterThan(0);
    expect(geom.bulkRight, "the bulk bar runs past the right of the screen").toBeLessThanOrEqual(
      geom.visibleW + 1,
    );
  });

  test("the drawer and its scrim fit the screen they cover", async ({ page }) => {
    await goto(page, "/settings/accounts");
    await page.getByRole("button", { name: /^Show settings menu$/ }).click();
    // Eleven 44px rows do not fit 568px of screen, so the menu scrolls — and the last link is
    // what proves the drawer is sized to the screen rather than to the layout viewport: too
    // tall, and its own scroll container ends below the bottom edge and the last row can never
    // be brought into view at all.
    const last = page.locator("a[href='#/settings/merchants']");
    await last.scrollIntoViewIfNeeded();
    await expect(last).toBeInViewport();
    await last.click();
    await expect(page).toHaveURL(/#\/settings\/merchants$/);

    // Reopen for the geometry checks below; navigating closed it.
    await page.getByRole("button", { name: /^Show settings menu$/ }).click();

    const geom = await page.evaluate(() => {
      const de = document.documentElement;
      const drawer = document.querySelector(".panel-col")!.getBoundingClientRect();
      const scrim = document.querySelector(".panel-scrim")!.getBoundingClientRect();
      return {
        drawerBottom: drawer.bottom,
        scrimRight: scrim.right,
        scrimBottom: scrim.bottom,
        visibleH: de.clientHeight,
        visibleW: de.clientWidth,
      };
    });
    expect(geom.drawerBottom, "the drawer runs past the bottom of the screen").toBeLessThanOrEqual(
      geom.visibleH + 1,
    );
    expect(geom.scrimRight).toBeLessThanOrEqual(geom.visibleW + 1);
    expect(geom.scrimBottom).toBeLessThanOrEqual(geom.visibleH + 1);
  });
});

test("the documented breakpoints are the ones the stylesheet actually uses", () => {
  // app.css names --bp-phone and --bp-dock and explains what each is for, but CSS cannot
  // interpolate a custom property into a media query, so every rule restates the number. This
  // is what stops the explanation and the behaviour drifting apart.
  const css = readFileSync(path.join(process.cwd(), "src", "app.css"), "utf8");
  const declared = (name: string) => {
    const m = new RegExp(`--${name}:\\s*(\\d+)px`).exec(css);
    expect(m, `--${name} is not declared in app.css`).not.toBeNull();
    return Number(m![1]);
  };
  expect(declared("bp-phone")).toBe(720);
  expect(declared("bp-dock")).toBe(1000);

  // The phone breakpoint is restated in app.css itself and in App.svelte. The dock breakpoint's
  // only restatement is App.svelte's `matchMedia` call — the copy most likely to be forgotten,
  // because it is the one that is JavaScript rather than CSS.
  const app = readFileSync(path.join(process.cwd(), "src", "App.svelte"), "utf8");
  expect(css).toContain("@media (max-width: 720px)");
  expect(app).toContain("@media (max-width: 720px)");
  expect(app).toContain('matchMedia("(min-width: 1000px)")');
});
