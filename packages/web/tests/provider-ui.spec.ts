// What the app tells a user about state it learned from a provider — here, the absence of one.
//
// The seed creates two USD accounts against an NZD base currency and no exchange rate at all, so
// `GET /api/reports/net-worth` reports `unconverted: ["USD"]` and the headline total on the
// overview is *missing* that money rather than counting it at 1:1 (see FxNotice.svelte and
// sure_app::fx for why excluded beats confidently-wrong). Whether the report says so is settled
// in `sure_app`'s own tests; whether a user is ever told is only observable in a browser, which
// is what makes this a web spec rather than another api-test.
//
// It is not already covered by `overview.png`. That baseline pins the notice's *presence*, since
// dropping two lines of text changes the page height and a size mismatch fails outright — but it
// cannot pin the words: at 692×2665 the tolerance is 3% of 1,844,180 pixels ≈ 55,000, and the
// whole notice is under 24,000. "Excludes AUD", or a notice that named the wrong report currency,
// is a green screenshot.
//
// No `toHaveScreenshot` here on purpose. A baseline is per-platform (`*-darwin.png` in this tree,
// `*-linux.png` in the container CI pins), so a new one minted on a developer's machine is a file
// that looks authoritative and pins nothing anyone has checked — and at a 3% tolerance it can be
// wrong and still pass. A spec added outside that container asserts on DOM text or asserts nothing.

import { test, expect } from "./fixtures";

test("the overview's net-worth total names the currency it had no rate for", async ({ page }) => {
  await page.goto("/#/");
  await page.waitForLoadState("networkidle");

  // Scoped to the net-worth card rather than the page's only `.fx-notice`, because *which* total
  // is incomplete is half of what the notice means — the same component also sits under the
  // Investments card, where it reports unconverted holdings from a different endpoint.
  const netWorth = page
    .locator("section.card")
    .filter({ has: page.getByRole("heading", { name: "Net worth", exact: true }) });

  // Both directions of the conversion, because either alone reads as harmless: "excludes USD"
  // without a target currency does not say what the total below is denominated in, and a target
  // without the excluded currency does not say what is missing from it.
  await expect(netWorth.locator(".fx-notice")).toContainText("Excludes USD — no exchange rate to NZD.");
  // The advice, which is only accurate while no feed has written a rate — there is no rate-entry
  // screen, so pointing at a setting would be worse than saying nothing. Three things keep that
  // true for this run: the seed writes no rates, `BACKGROUND_TASKS: "off"` stops the poller, and
  // the proxy answers any on-demand fetch with a 503 (see global-setup.ts).
  await expect(netWorth.locator(".fx-notice")).toContainText("Nothing has been polled or imported for it yet.");
});
