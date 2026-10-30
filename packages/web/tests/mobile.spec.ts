import { readFileSync, writeFileSync } from "node:fs";
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
  "/settings/preferences",
  "/settings/appearance",
  "/settings/scheduled",
  "/settings/categories",
  "/settings/rules",
  "/settings/merchants",
  // Not in the menu: five taps on its version line open it (tests/debug-page.spec.ts).
  "/settings/debug",
];

/** The settings pages whose only route in is the panel — Accounts is the landing page. */
const DRAWER_ONLY = ROUTES.filter(
  (r) => r.startsWith("/settings/") && r !== "/settings/accounts" && r !== "/settings/debug",
);

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

/**
 * Installed with Safari's "Add to Home Screen", which is how this app is used.
 *
 * Standalone mode takes the browser away — no address bar, no reload button, no pull to
 * refresh — and iOS reads things off the page that a tab never looks at. These check what can
 * be checked without a phone. The status bar and the launch screen cannot be: they are drawn
 * by iOS, and no browser Playwright drives has either.
 */
test.describe("installed on an iPhone's Home Screen", () => {
  /** The dark palette's page colour, from the one place that defines it. */
  function darkBg(): string {
    const css = readFileSync(path.join(process.cwd(), "src", "app.css"), "utf8");
    const palette = /:root,\s*:root\[data-theme="dark"\]\s*\{([\s\S]*?)\}/.exec(css);
    expect(palette, "app.css no longer opens the dark palette the way this expects").not.toBeNull();
    const bg = /--bg:\s*(#[0-9a-f]{6})/i.exec(palette![1]);
    expect(bg, "the dark palette declares no --bg").not.toBeNull();
    return bg![1].toLowerCase();
  }

  test("every statement of the dark page colour agrees with the palette", async ({ request }) => {
    // Four places say it, because each is read before or without the next: the manifest
    // (before the page exists), index.html's meta and pre-paint script (before the CSS), and
    // theme.svelte.ts (after). The manifest kept the previous palette's navy after the other
    // three had moved on.
    const bg = darkBg();
    const manifest = await (await request.get("/manifest.webmanifest")).json();
    expect(manifest.theme_color).toBe(bg);
    expect(manifest.background_color).toBe(bg);
    expect(manifest.display).toBe("standalone");
    expect(manifest.id).toBe("/");

    const html = readFileSync(path.join(process.cwd(), "index.html"), "utf8");
    expect(html, "index.html's theme-color meta").toContain(`name="theme-color" content="${bg}"`);
    expect(html, "index.html's pre-paint script").toContain(`"dark" ? "${bg}"`);
    const theme = readFileSync(path.join(process.cwd(), "src", "lib", "theme.svelte.ts"), "utf8");
    expect(theme, "theme.svelte.ts's THEME_COLOR").toContain(`dark: "${bg}"`);
  });

  test("the icons that get cropped are square, opaque, and keep the mark inside the crop", async ({
    page,
    request,
  }) => {
    await goto(page, "/");
    const touch = page.locator('link[rel="apple-touch-icon"]');
    await expect(touch).toHaveAttribute("sizes", "180x180");

    const manifest = await (await request.get("/manifest.webmanifest")).json();
    const icons: { src: string; purpose?: string }[] = manifest.icons;
    const maskable = icons.find((i) => i.purpose === "maskable");
    expect(maskable, "the manifest has no maskable icon").toBeDefined();
    expect(
      icons.filter((i) => i.purpose !== "maskable").map((i) => i.src),
      "the maskable icon is one of the plain ones again",
    ).not.toContain(maskable!.src);

    /** Size, how many pixels are not fully opaque, and how many of the mark's lie outside the
     *  central circle of radius 40% — the part a maskable icon is promised to keep. */
    const measure = (src: string) =>
      page.evaluate(async (src) => {
        const img = new Image();
        img.src = src;
        await img.decode();
        const n = img.naturalWidth;
        const canvas = document.createElement("canvas");
        canvas.width = canvas.height = n;
        const ctx = canvas.getContext("2d")!;
        ctx.drawImage(img, 0, 0);
        const px = ctx.getImageData(0, 0, n, n).data;
        const tile = [px[0], px[1], px[2]];
        let translucent = 0;
        let outside = 0;
        for (let y = 0; y < n; y++) {
          for (let x = 0; x < n; x++) {
            const i = (y * n + x) * 4;
            if (px[i + 3] < 255) translucent++;
            const ink =
              Math.abs(px[i] - tile[0]) + Math.abs(px[i + 1] - tile[1]) + Math.abs(px[i + 2] - tile[2]);
            if (ink > 24 && Math.hypot(x + 0.5 - n / 2, y + 0.5 - n / 2) > 0.4 * n) outside++;
          }
        }
        return { width: n, height: img.naturalHeight, translucent, outside };
      }, src);

    // iOS fills transparency with black before cutting its own mask, so rounded corners drawn
    // into the image show as dark slivers inside the system's curve.
    const apple = await measure((await touch.getAttribute("href"))!);
    expect(apple).toMatchObject({ width: 180, height: 180, translucent: 0 });

    const mask = await measure(`/${maskable!.src}`);
    expect(mask.translucent, "the maskable icon has transparent pixels a launcher would show").toBe(0);
    expect(mask.outside, "the mark reaches outside the maskable safe circle").toBe(0);
  });

  test("you can pinch-zoom, and no field is small enough for iOS to zoom into on focus", async ({
    page,
  }) => {
    await goto(page, "/");
    // `maximum-scale` / `user-scalable=no` stop pinch-zoom in an installed app, which is how a
    // small figure gets read. The one thing they were good for is the next assertion's job.
    expect(await page.locator('meta[name="viewport"]').getAttribute("content")).not.toMatch(
      /maximum-scale|user-scalable/,
    );

    /** Every rendered text field under 16px — which iOS zooms into on focus, and never back. */
    const smallFields = () =>
      page.evaluate(() => {
        const UNTYPED = ["checkbox", "radio", "range", "color", "file", "hidden", "button", "submit", "reset", "image"];
        const found = [...document.querySelectorAll<HTMLElement>("input, select, textarea")]
          .filter((el) => !(el instanceof HTMLInputElement && UNTYPED.includes(el.type)))
          .filter((el) => el.getClientRects().length > 0)
          .filter((el) => parseFloat(getComputedStyle(el).fontSize) < 16)
          .map((el) => `<${el.tagName.toLowerCase()} class="${el.className}"> at ${getComputedStyle(el).fontSize}`);
        return [...new Set(found)];
      });

    for (const route of ROUTES) {
      await goto(page, route);
      expect(await smallFields(), `on ${route}`).toEqual([]);
    }

    // The fields that only exist once something is open or loaded. The transaction list's
    // category and owner pills are the case that was wrong: a transparent <select> over the
    // drawn pill, so its size never showed, but it is what takes focus on a tap.
    await goto(page, "/transactions");
    await expect(page.locator(".tx-row .pill-select").first()).toBeAttached();
    expect(await smallFields(), "on the transaction list").toEqual([]);

    await goto(page, "/settings/accounts");
    await page.locator(".acct", { hasText: "Everyday" }).getByRole("button", { name: "Edit" }).click();
    expect(await smallFields(), "in the account editor").toEqual([]);

    await goto(page, "/settings/accounts");
    await page.getByRole("button", { name: "+ Add account" }).click();
    expect(await smallFields(), "in the new-account dialog").toEqual([]);

    await goto(page, "/settings/rules");
    await page.getByRole("button", { name: "+ New rule" }).click();
    expect(await smallFields(), "in the rule builder").toEqual([]);
  });

  /**
   * A deploy reaching a phone that resumes the app rather than reopening it (src/lib/pwa.ts).
   *
   * End to end, with a real second build of the worker. The server reads dist/ from disk and
   * the browser compares a worker script byte for byte, so a line appended to dist/sw.js is a
   * deploy as far as anything here can tell — and global-setup rebuilds dist/ every run, so a
   * run that dies before putting it back costs nothing. Routing cannot stand in for it: the
   * browser's own update check for the worker script does not pass through `context.route`.
   *
   * It is end to end on purpose. The worker has to skip waiting and claim the page for any of
   * this to happen, and vite-plugin-pwa quietly stops generating it that way once `injectRegister`
   * is set to anything but its default — a version of this test that announced the takeover with
   * a synthetic event passed against exactly that broken build. This one fails against it.
   */
  test.describe("with its service worker running", () => {
    test.use({ serviceWorkers: "allow" });

    const workerFile = path.join(process.cwd(), "dist", "sw.js");
    let shipped = "";
    test.beforeAll(() => {
      shipped = readFileSync(workerFile, "utf8");
    });
    test.afterEach(() => {
      writeFileSync(workerFile, shipped);
    });
    const deploy = () => writeFileSync(workerFile, `${shipped}\n// the next build\n`);

    const loadedBefore = (page: Page) =>
      page.evaluate(() => document.documentElement.dataset.loadedBefore ?? "reloaded");
    /** A Home Screen app coming back to the foreground — which is not a navigation. */
    const resume = (page: Page) => page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    /** Deploy, resume, and wait for the new worker to have taken the page over. */
    const deployAndResume = async (page: Page) => {
      deploy();
      const noticed = page.waitForEvent("console", (m) => m.text().includes("a new version is installed"));
      await resume(page);
      await noticed;
    };

    test.beforeEach(async ({ page }) => {
      await goto(page, "/");
      await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
      await page.evaluate(() => (document.documentElement.dataset.loadedBefore = "yes"));
    });

    test("a new build loads at the next change of page, not under the user's thumb", async ({ page }) => {
      await deployAndResume(page);
      expect(await loadedBefore(page)).toBe("yes");
      // A change that stays on the page — the way opening a transaction does — is no seam.
      await page.evaluate(() => (window.location.hash = "#/?range=ytd"));
      await expect(page).toHaveURL(/range=ytd/);
      expect(await loadedBefore(page)).toBe("yes");

      const reloaded = page.waitForEvent("load");
      await page.locator(".rail a[href='#/transactions']").click();
      await reloaded;
      await expect(page).toHaveURL(/#\/transactions$/);
      expect(await loadedBefore(page)).toBe("reloaded");
      await expect(page.locator(".tx-row").first()).toBeVisible();
    });

    test("or at the next return to the foreground, if that comes first", async ({ page }) => {
      await deployAndResume(page);
      const reloaded = page.waitForEvent("load");
      await resume(page);
      await reloaded;
      expect(await loadedBefore(page)).toBe("reloaded");
    });
  });
});

/**
 * Safe-area insets, which `viewport-fit=cover` hands to the page to honour.
 *
 * Chromium can report them (`Emulation.setSafeAreaInsetsOverride`), so this is the real CSS
 * reading real `env()` values rather than a check that the right words appear in it. The
 * numbers are an iPhone 16 Pro's, the device the suite's 402x874 viewport is.
 */
async function emulateInsets(page: Page, insets: { top: number; left: number; right: number; bottom: number }) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setSafeAreaInsetsOverride", { insets });
}

test("upright, the tab bar's tabs clear the home indicator", async ({ page }) => {
  await emulateInsets(page, { top: 0, left: 0, right: 0, bottom: 34 });
  await goto(page, "/");
  const geom = await page.evaluate(() => ({
    bar: document.querySelector(".rail")!.getBoundingClientRect().bottom,
    tabs: [...document.querySelectorAll(".rail .rail-item")].map((el) => el.getBoundingClientRect().bottom),
    visibleH: document.documentElement.clientHeight,
  }));
  // The bar's background reaches the bottom edge; its tabs stop above the indicator.
  expect(geom.bar).toBeGreaterThanOrEqual(geom.visibleH - 1);
  for (const bottom of geom.tabs) expect(bottom).toBeLessThanOrEqual(geom.visibleH - 34);
});

test.describe("on its side, with the Dynamic Island at one edge", () => {
  // Wide enough for the rail rather than the tab bar, and too narrow to dock the panel: the
  // rail and the drawer are what meet the left edge.
  test.use({ viewport: { width: PHONE.height, height: PHONE.width } });
  const INSETS = { top: 0, left: 62, right: 62, bottom: 21 };

  test.beforeEach(async ({ page }) => {
    await emulateInsets(page, INSETS);
  });

  test("nothing the app draws sits under it", async ({ page }) => {
    await goto(page, "/");
    const right = PHONE.height - INSETS.right;

    const shell = await page.evaluate(() => {
      const box = (el: Element) => el.getBoundingClientRect();
      return {
        rail: box(document.querySelector(".rail")!).left,
        tabs: [...document.querySelectorAll(".rail .rail-item")].map((el) => box(el).left),
        controls: [...document.querySelectorAll(".subbar-inner > *")].map((el) => box(el).right),
        cards: [...document.querySelectorAll("main .card")].map((el) => box(el).right),
      };
    });
    // Backgrounds run to the edge — that is what fills the strip beside the island — and what
    // is drawn on them stops short of it.
    expect(shell.rail).toBe(0);
    for (const left of shell.tabs) expect(left, "a rail tab is under the island").toBeGreaterThanOrEqual(INSETS.left);
    expect(shell.controls.length).toBeGreaterThan(0);
    for (const r of shell.controls) expect(r, "a sub-bar control is under the island").toBeLessThanOrEqual(right);
    expect(shell.cards.length).toBeGreaterThan(0);
    for (const r of shell.cards) expect(r, "a card runs under the island").toBeLessThanOrEqual(right);
    expect(await overhang(page)).toBeLessThanOrEqual(1);

    // The drawer slides in over the rail, from the same edge — measured once it has arrived.
    await page.getByRole("button", { name: /^Show accounts panel$/ }).click();
    const drawer = page.locator(".panel-col");
    await expect.poll(async () => (await drawer.boundingBox())?.x).toBe(0);
    const content = await drawer.evaluate((el) =>
      Math.min(...[...el.querySelectorAll("a, button")].map((c) => c.getBoundingClientRect().left)),
    );
    expect(content, "the drawer's links are under the island").toBeGreaterThanOrEqual(INSETS.left);
    await page.keyboard.press("Escape");

    // And the expanded money flow, which is what someone turns the phone on its side to read.
    await page.getByRole("button", { name: "Expand" }).click();
    const modal = await page.locator(".overlay .modal").boundingBox();
    expect(modal).not.toBeNull();
    expect(modal!.x).toBeGreaterThanOrEqual(INSETS.left);
    expect(modal!.x + modal!.width).toBeLessThanOrEqual(right);
  });
});
