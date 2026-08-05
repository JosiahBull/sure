// Linking a brokerage platform (Sharesies) from the Bank sync page.
//
// A brokerage platform arrives from Akahu as several sibling accounts — one cash wallet per
// currency — so the connect dialog groups them and links the group into a single Brokerage
// account (`POST /api/providers/link-group`). The group is keyed by authorisation *and*
// institution, because two people who each connect their own Sharesies would otherwise have
// every wallet merged into one account.
//
// That key is derived in two places: once when `discover()` seeds a form for each group, and
// again by the `brokerageGroups` derivation the rows render from. When they disagree, the
// lookup `groupForms[g.key]` misses, `{#if open && f}` renders nothing, and clicking Link
// expands a row onto an empty box with no form and no button — the group cannot be linked at
// all, while every non-brokerage row (keyed by `external_id` in both places) still works.
// That is exactly what shipped: db3d766 put the authorisation into the derivation and left
// the seeding on institution alone. Nothing caught it, because the two halves of the key are
// four hundred lines apart and neither is wrong on its own.
//
// Both tests below stub discovery in the browser rather than letting the page reach the
// backend's Akahu adapter: the suite's backend has no credentials on purpose (global-setup
// strips them), and what broke is entirely client-side — which accounts get grouped, and
// whether the group's form can be found again afterwards. The link POST is stubbed for a
// second reason: the demo database is shared with the visual baselines, and a spec that
// really created a brokerage account would change the Accounts page out from under them.

import { type Page, type Request } from "@playwright/test";

import { test, expect } from "./fixtures";

/** A discovered Sharesies cash wallet, shaped as `map_account` in the Akahu adapter emits one. */
function wallet(externalId: string, name: string, currency: string, authorisation: string) {
  return {
    external_id: externalId,
    name,
    currency_code: currency,
    institution: "Sharesies",
    authorisation_id: authorisation,
    // Left null on purpose: a platform wallet has no bank account number, and two of them
    // sharing one would trip the "same account from two logins" warning instead.
    account_number: null,
    kind_hint: "brokerage",
    balance_minor: 1_00,
    supports_transactions: true,
  };
}

/**
 * Answer Akahu discovery with `wallets`, and capture any link-group request instead of
 * performing it. Returns the captured requests, which stay empty until one is sent.
 */
async function stubDiscovery(page: Page, wallets: ReturnType<typeof wallet>[]): Promise<Request[]> {
  const linked: Request[] = [];
  await page.route("**/api/provider-kinds/akahu/accounts", (route) =>
    route.fulfill({ json: wallets }),
  );
  await page.route("**/api/providers/link-group", (route) => {
    linked.push(route.request());
    // The shape the page reads back is only `[Provider]`; it uses the count, not the fields.
    return route.fulfill({
      status: 201,
      json: wallets.map((w, i) => ({
        id: 900 + i,
        name: `Akahu — ${w.name}`,
        kind: "akahu",
        account_id: 900,
        config: { external_account_id: w.external_id },
        enabled: true,
        last_synced_at: null,
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      })),
    });
  });
  return linked;
}

/** Open the Bank sync page's Akahu card, which auto-runs discovery on mount. */
async function openAkahuConnect(page: Page) {
  await page.goto("/#/settings/providers");
  await page.waitForLoadState("networkidle");
  await page
    .locator(".cat-card", { hasText: "Akahu" })
    .getByRole("button", { name: "Find accounts" })
    .click();
  await expect(page.getByRole("dialog", { name: "Connect Akahu" })).toBeVisible();
}

test("a Sharesies group's link form opens, and links every wallet into one account", async ({
  page,
}) => {
  const linked = await stubDiscovery(page, [
    wallet("acc_nzd", "NZD Wallet", "NZD", "auth_one"),
    wallet("acc_usd", "USD Wallet", "USD", "auth_one"),
    wallet("acc_aud", "AUD Wallet", "AUD", "auth_one"),
  ]);
  await openAkahuConnect(page);

  // One row for the platform, not three for its wallets.
  const row = page.locator(".row-card", { hasText: "Sharesies" });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("Brokerage · 3 wallets");

  await row.getByRole("button", { name: /Sharesies/ }).click();

  // The regression, stated directly: expanding the row has to produce the form. Before the
  // fix the row opened onto nothing at all, so every assertion from here down failed — and
  // in the app there was no way past this point.
  await expect(row.locator(".row-body")).toBeVisible();
  await expect(row.getByLabel("Link to")).toBeVisible();
  await expect(row.getByLabel("Account name")).toHaveValue("Sharesies");
  // Each wallet is listed with its balance, so what the one account will hold is visible
  // before committing to it.
  await expect(row.locator(".wallets li")).toHaveCount(3);

  const link = row.getByRole("button", { name: "Link brokerage account" });
  await expect(link).toBeEnabled();
  await link.click();

  expect(linked).toHaveLength(1);
  const body = linked[0].postDataJSON();
  expect(body.kind).toBe("akahu");
  // Every wallet, in one request against one new account — the whole point of the group.
  expect(body.members.map((m: { external_id: string }) => m.external_id).sort()).toEqual([
    "acc_aud",
    "acc_nzd",
    "acc_usd",
  ]);
  expect(body.new_account.kind).toBe("brokerage");
  expect(body.existing_account_id).toBeUndefined();

  await expect(page.getByText("Linked 3 wallets into one brokerage account.")).toBeVisible();
});

test("two people's Sharesies logins are separate groups, and neither absorbs the other", async ({
  page,
}) => {
  // The reason the key carries the authorisation at all. Institution alone would make these
  // one group of four wallets — one Sure account holding two households' money — which is
  // also the shape a "fix" that merely aligned the two derivations on `institution` would
  // reintroduce. Same institution, same wallet names, different logins: only the
  // authorisation tells them apart.
  const linked = await stubDiscovery(page, [
    wallet("acc_mine_nzd", "NZD Wallet", "NZD", "auth_mine"),
    wallet("acc_mine_usd", "USD Wallet", "USD", "auth_mine"),
    wallet("acc_theirs_nzd", "NZD Wallet", "NZD", "auth_theirs"),
    wallet("acc_theirs_usd", "USD Wallet", "USD", "auth_theirs"),
  ]);
  await openAkahuConnect(page);

  const rows = page.locator(".row-card", { hasText: "Sharesies" });
  await expect(rows).toHaveCount(2);
  for (const row of await rows.all()) {
    await expect(row).toContainText("Brokerage · 2 wallets");
  }

  // Both rows have to resolve their own form — a key that collided would leave the second
  // one empty in exactly the way the bug above left the first.
  for (const row of await rows.all()) {
    await row.getByRole("button", { name: /Sharesies/ }).click();
    await expect(row.locator(".wallets li")).toHaveCount(2);
    await row.getByRole("button", { name: /Sharesies/ }).click(); // collapse; one opens at a time
  }

  await rows.first().getByRole("button", { name: /Sharesies/ }).click();
  await rows.first().getByRole("button", { name: "Link brokerage account" }).click();

  expect(linked).toHaveLength(1);
  const members: string[] = linked[0]
    .postDataJSON()
    .members.map((m: { external_id: string }) => m.external_id);
  // One login's two wallets — not all four.
  expect(members).toHaveLength(2);
  expect(new Set(members.map((id) => id.split("_")[1]))).toEqual(new Set(["mine"]));
});
