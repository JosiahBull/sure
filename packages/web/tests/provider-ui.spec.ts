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
// No `toHaveScreenshot` here on purpose: at a 3% tolerance a screenshot cannot pin a word, and a
// word is the whole of what this spec asserts. The other half of that reasoning has since changed
// and is worth not re-deriving — a baseline is still per-platform (`*-darwin.png` in this tree,
// `*-linux.png` in the image CI pins), but `pnpm snapshots:update` renders the Linux half in that
// same image locally (docs/TESTING.md), so adding one is now a judgement about tolerance rather
// than about who is able to produce the file.

import { type Page } from "@playwright/test";

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

// ---- what the Bank sync page says about a connection's health ------------------------------
//
// These are browser tests of the real app against a real backend, with one thing faked: the
// `GET /api/providers` response. That is a deliberate seam, not a shortcut, and it is worth
// being precise about where it falls.
//
// Driving a real Akahu 404 all the way into a browser is not available here. This suite runs one
// shared backend for all of its specs, against the seeded demo database the visual baselines are
// taken from, and `global-setup.ts` strips `AKAHU_APP_TOKEN`/`AKAHU_USER_TOKEN` out of its
// environment on purpose — "a suite whose screenshots assert on exact pixels cannot have that
// depend on whose shell it ran in". A spec that wanted a configured backend would have to spawn a
// second one with its own database and its own port and re-point the SPA at it, which is
// infrastructure this suite does not have.
//
// So the coverage is split, and the split is clean:
//
//   * that a retired account *produces* `disconnected` — a real 404 through the test proxy, a
//     real sync, a real `provider_syncs` row, read back off a real `GET /api/providers` — is
//     `packages/api-tests/specs/akahu.spec.ts`;
//   * that the app *renders* it correctly is here.
//
// The risk a split like that invites is drift: the fixture below describes a response no server
// sends any more, and both suites stay green. So it is typed as `Schemas["Provider"]` — generated
// from the OpenAPI document by `pnpm gen:client` — which makes a field renamed or a `SyncOutcome`
// variant added in Rust a type error on this literal, caught by `pnpm --filter @sure/web check`
// and so by CI's Typecheck job.
//
// That enforcement is new, and it did not exist when these tests were written: `tsconfig.json`'s
// `include` is `src/**`, and Playwright transpiles specs with esbuild, which strips types without
// checking them — so nothing looked at this directory at all. `tsconfig.tests.json` is what
// covers it now. Without that, typing the fixture would be decoration.

import type { Schemas } from "@sure/client";

/** A connection as `GET /api/providers` serves one, with whatever last happened to it. */
function connection(
  id: number,
  name: string,
  lastSync: { status: Schemas["SyncOutcome"]; detail?: string } | null,
): Schemas["Provider"] {
  return {
    id,
    name,
    kind: "akahu",
    // Every seeded account id would do; the page only renders the name it resolves to, and an
    // unresolved one reads as "unknown account" without failing.
    account_id: 1,
    config: { external_account_id: `acc_${id}` },
    enabled: true,
    last_synced_at: lastSync?.status === "ok" ? "2026-08-15T09:00:00.000Z" : null,
    last_sync: lastSync
      ? {
          id,
          provider_id: id,
          imported: 0,
          skipped: 0,
          status: lastSync.status,
          detail: lastSync.detail ?? null,
          created_at: "2026-08-15T09:00:00.000Z",
        }
      : null,
    created_at: "2026-01-01T00:00:00.000Z",
    updated_at: "2026-01-01T00:00:00.000Z",
  };
}

async function showConnections(page: Page, rows: Schemas["Provider"][]): Promise<void> {
  await page.route("**/api/providers", (route) => route.fulfill({ json: rows }));
  await page.goto("/#/settings/providers");
  await page.waitForLoadState("networkidle");
}

/**
 * The state that needs a person, said in the three places it has to be said.
 *
 * A `providers` row existing is not the same fact as the feed working, and this page used to
 * conflate them — every connection wore a green "Connected" for as long as it existed. So a bank
 * whose Akahu connection had been removed, expired, or re-authorised looked exactly like one that
 * synced an hour ago, while the account it feeds went on showing a balance that had quietly
 * stopped moving. Retrying cannot fix it (a re-authorisation issues a new account id, so the
 * stored one is gone for good), which is why the row offers Reconnect instead of Sync now.
 *
 * DOM text, not a screenshot: at this suite's 3% tolerance a badge reading the wrong word is a
 * green baseline (see the note at the top of this file).
 */
test("a connection the upstream has retired says so, and offers the fix", async ({ page }) => {
  await showConnections(page, [
    connection(901, "Akahu — Everyday", { status: "ok" }),
    connection(902, "Akahu — Old Savings", {
      status: "disconnected",
      detail: "Akahu no longer has account acc_902. Reconnect the bank in Akahu, then link the account here again.",
    }),
  ]);

  const healthy = page.locator(".conn", { hasText: "Akahu — Everyday" });
  const retired = page.locator(".conn", { hasText: "Akahu — Old Savings" });

  await expect(healthy.locator(".badge.ok")).toHaveText("Connected");
  await expect(retired.locator(".badge.bad")).toHaveText("Disconnected");

  // The server's words reach the row. They are the only place anyone is told what to do, so a
  // badge without them is a dead end.
  await expect(retired.locator(".detail")).toContainText("link the account here again");

  // Sync now is *replaced*, not merely joined: the account id this connection holds no longer
  // exists upstream, so the button would only produce the same failure again.
  await expect(retired.getByRole("button", { name: "Reconnect" })).toBeVisible();
  await expect(retired.getByRole("button", { name: "Sync now" })).toHaveCount(0);
  await expect(healthy.getByRole("button", { name: "Sync now" })).toBeVisible();

  // And it is said once above the list too, because the rest of the app shows that account's
  // last known balance with nothing marking it as stale — so the page that can explain it should
  // do so before you have to find the red row.
  const notice = page.locator(".stale-feed");
  await expect(notice).toContainText("1 connection is no longer connected upstream");
  // No link on this page: it would point at itself. The overview's copy of the same notice is
  // the one that carries it (asserted below).
  await expect(notice.getByRole("link")).toHaveCount(0);
});

/**
 * The other two states, which exist so that "not Connected" is not one undifferentiated red.
 *
 * A failing sync is a bad minute at the bank and clears on its own; a connection nobody has
 * synced yet is not a problem at all. Collapsing either into the disconnected treatment would
 * send a user to re-link a connection that is fine — and that is destructive, because re-linking
 * throws away the sync watermark.
 */
test("a failing sync and a brand-new connection are told apart from a retired one", async ({
  page,
}) => {
  await showConnections(page, [
    connection(903, "Akahu — Flaky", { status: "error", detail: "Internal server error: upstream" }),
    connection(904, "Akahu — Brand New", null),
  ]);

  const failing = page.locator(".conn", { hasText: "Akahu — Flaky" });
  await expect(failing.locator(".badge.warn")).toHaveText("Sync failing");
  // The upstream's own message, which is what makes "failing" diagnosable rather than alarming.
  await expect(failing.locator(".detail")).toContainText("Internal server error");
  // Still worth retrying, unlike a retired account — so the button stays.
  await expect(failing.getByRole("button", { name: "Sync now" })).toBeVisible();

  // `.badge.idle` rather than `.badge`: every row also wears a plain badge naming the provider
  // kind, so the bare class matches two elements. The state class is the assertion anyway — it
  // is what carries the colour, and "not synced yet" must stay the neutral one.
  const fresh = page.locator(".conn", { hasText: "Akahu — Brand New" });
  await expect(fresh.locator(".badge.idle")).toHaveText("Not synced yet");
  await expect(fresh.locator(".detail")).toHaveCount(0);

  // Neither is a disconnection, so the banner that tells a user to go re-link must stay away.
  await expect(page.locator(".stale-feed")).toHaveCount(0);
});

/**
 * The overview carries the same notice, and this is the placement that actually matters.
 *
 * A retired connection leaves the account it fed frozen at its last recorded balance — no gap,
 * no zero, nothing on this page that reads as wrong. Net worth, the balance sheet and every
 * chart below are all built from that number, and the only place the app knew was a sync history
 * nobody visits. So the totals say it themselves, name the connection (there is no list here to
 * match a count against), and point at the page that can fix it.
 */
test("the overview says when a bank connection has stopped feeding its accounts", async ({
  page,
}) => {
  await page.route("**/api/providers", (route) =>
    route.fulfill({
      json: [
        connection(901, "Akahu — Everyday", { status: "ok" }),
        connection(902, "Akahu — Old Savings", { status: "disconnected", detail: "gone" }),
      ],
    }),
  );
  await page.goto("/#/");
  await page.waitForLoadState("networkidle");

  const notice = page.locator(".stale-feed");
  await expect(notice).toContainText("1 connection is no longer connected upstream");
  // Named, which the Bank sync page's copy deliberately does not do: with several accounts on
  // screen and none of them marked, a bare count says a number is stale without saying which.
  await expect(notice).toContainText("Akahu — Old Savings");
  await expect(notice).not.toContainText("Akahu — Everyday");
  // And the way out, since this page cannot fix it.
  await expect(notice.getByRole("link", { name: "Bank sync →" })).toHaveAttribute(
    "href",
    "#/settings/providers",
  );
});

/**
 * The other half, and the one that keeps this from being noise: on a household whose feeds are
 * fine — and on the far more common one with no connections at all — the overview says nothing.
 *
 * This is also what protects `overview.png`. The demo seed creates no providers, so the notice
 * must not render there; if it ever did, the baseline would fail on a height mismatch rather
 * than on anything anyone had decided.
 */
test("the overview stays quiet when every connection is healthy", async ({ page }) => {
  await page.route("**/api/providers", (route) =>
    route.fulfill({ json: [connection(901, "Akahu — Everyday", { status: "ok" })] }),
  );
  await page.goto("/#/");
  await page.waitForLoadState("networkidle");
  await expect(page.locator(".stale-feed")).toHaveCount(0);
});
