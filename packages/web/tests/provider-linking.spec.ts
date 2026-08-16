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
import type { Schemas } from "@sure/client";

import { test, expect } from "./fixtures";

// Both fixtures below are annotated `Schemas["ProviderAccount"]` — the type generated from the
// OpenAPI document by `pnpm gen:client` — rather than left to inference. Inference is what let
// the `original_amount_hint_minor` spread further down be a type error nobody saw: `tests/` was
// outside every tsconfig until `tsconfig.tests.json`, so the field was silently absent from the
// inferred union and the row it describes was never a mortgage the page could prefill from.
// Naming the real type also means a field renamed in Rust fails `pnpm --filter @sure/web check`
// here, instead of leaving a green spec asserting against a response no server sends.

/** A discovered Sharesies cash wallet, shaped as `map_account` in the Akahu adapter emits one. */
function wallet(
  externalId: string,
  name: string,
  currency: string,
  authorisation: string,
  institution = "Sharesies",
): Schemas["ProviderAccount"] {
  return {
    external_id: externalId,
    name,
    currency_code: currency,
    institution,
    authorisation_id: authorisation,
    // Left null on purpose: a platform wallet has no bank account number.
    account_number: null,
    kind_hint: "brokerage",
    balance_minor: 1_00,
    supports_transactions: true,
    // The server decides this now (`sure_app`'s `survey_accounts`), so a stub states it
    // rather than the page inferring it from the rows it happens to have been given.
    joint: false,
  };
}

/** An ordinary discovered bank account, the kind that links one row at a time. */
function bankAccount(
  externalId: string,
  name: string,
  institution: string,
  authorisation: string,
  extra: Partial<Schemas["ProviderAccount"]> = {},
): Schemas["ProviderAccount"] {
  return {
    external_id: externalId,
    name,
    currency_code: "NZD",
    institution,
    authorisation_id: authorisation,
    account_number: null,
    kind_hint: "bank",
    balance_minor: 1_00,
    supports_transactions: true,
    joint: false,
    ...extra,
  };
}

/**
 * Answer Akahu discovery with `accounts`, and capture any link request — single or group —
 * instead of performing it. Returns the captured requests, empty until one is sent.
 */
async function stubDiscovery(
  page: Page,
  wallets: Schemas["ProviderAccount"][],
): Promise<Request[]> {
  const linked: Request[] = [];
  await page.route("**/api/provider-kinds/akahu/accounts", (route) =>
    route.fulfill({ json: wallets }),
  );
  await page.route("**/api/providers/link", (route) => {
    linked.push(route.request());
    // Only the absence of an error is read back; the fields go unused.
    return route.fulfill({
      status: 201,
      json: {
        id: 901,
        name: "Akahu — linked",
        kind: "akahu",
        account_id: 901,
        config: {},
        enabled: true,
        last_synced_at: null,
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      },
    });
  });
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

test("linking a row does not re-order the login groups around it", async ({ page }) => {
  // The dialog puts the biggest login first, because the everyday-banking one is what you came
  // to link. It used to derive that from the rows a login still has *left* — which is the one
  // number linking changes. Link two of one login's three and it fell behind a login with two:
  // the block you were working through jumped down the page mid-task, everything below it moved,
  // and because the heading numbered array positions the groups renumbered as well.
  const linked = await stubDiscovery(page, [
    // Deliberately not in ranked order, and not grouped: whatever order discovery answers in,
    // the ranking is what decides the display order — not the order rows happen to arrive.
    bankAccount("acc_kb_spending", "Spending", "Kiwibank", "auth_kb"),
    bankAccount("acc_asb_everyday", "Everyday", "ASB", "auth_asb"),
    bankAccount("acc_wp_joint", "Joint", "Westpac", "auth_wp"),
    bankAccount("acc_asb_savings", "Savings", "ASB", "auth_asb"),
    bankAccount("acc_kb_holiday", "Holiday", "Kiwibank", "auth_kb"),
    bankAccount("acc_asb_bills", "Bills", "ASB", "auth_asb"),
  ]);
  await openAkahuConnect(page);

  const headings = page.locator(".login-title");
  await expect(headings).toHaveText(["ASB · login 1", "Kiwibank · login 2", "Westpac · login 3"]);

  /** Link one row by name, and wait for it to leave the list. */
  async function link(name: string) {
    const row = page.locator(".row-card", { hasText: name });
    await row.getByRole("button", { name }).click();
    await row.getByRole("button", { name: "Link account" }).click();
    await expect(page.locator(".row-card", { hasText: name })).toHaveCount(0);
  }

  await link("Everyday");
  await link("Savings");

  // ASB is down to one unlinked row against Kiwibank's two — the exact point the old sort
  // swapped them. The heading still says so, and the group has not moved.
  expect(linked).toHaveLength(2);
  await expect(headings).toHaveText(["ASB · login 1", "Kiwibank · login 2", "Westpac · login 3"]);
  await expect(page.locator(".row-card")).toHaveCount(4);

  // And a login that empties out entirely takes its number with it, rather than handing it to
  // the next group down: "login 2" stays the group it was when you started reading the list.
  await link("Bills");
  await expect(headings).toHaveText(["Kiwibank · login 2", "Westpac · login 3"]);
});

test("rows are alphabetical within a login, whatever order discovery answered in", async ({
  page,
}) => {
  // The other half of the same complaint: the group order was unstable while linking, and the
  // order *inside* a group was whatever the feed returned — which no feed promises, so closing
  // and reopening the dialog dealt the same rows out differently.
  await stubDiscovery(page, [
    bankAccount("acc_sav10", "Savings 10", "ASB", "auth_one"),
    wallet("acc_sh_nzd", "NZD Wallet", "NZD", "auth_one"),
    bankAccount("acc_ev", "everyday", "ASB", "auth_one"),
    wallet("acc_hatch_usd", "USD Wallet", "USD", "auth_one", "Hatch"),
    bankAccount("acc_bills", "Bills", "ASB", "auth_one"),
    bankAccount("acc_sav2", "Savings 2", "ASB", "auth_one"),
  ]);
  await openAkahuConnect(page);

  await expect(page.locator(".row-card .name")).toHaveText([
    // Brokerage platforms still lead — they are a different sort of row, and one row stands for
    // several wallets — but they are alphabetical between themselves now too.
    "Hatch",
    "Sharesies",
    // Then the ordinary accounts: case is not an ordering ("everyday" sits where an E sits, not
    // after every capital letter), and "Savings 2" reads above "Savings 10" rather than below it.
    "Bills",
    "everyday",
    "Savings 2",
    "Savings 10",
  ]);
});

test("an account both logins report is badged joint, owned by the household, and linked once", async ({
  page,
}) => {
  // The shape of the thing: one bank account, two logins, a different `external_id` and a
  // different nickname in each — so nothing but the account number pairs them, and the server
  // is what does the pairing. The page is handed the answer and has to act on it: badge the
  // rows, refuse to let the owner be anyone, and — the part that used to go wrong — take the
  // *other* login's copy out of the list when either one is linked. Before, linking one left
  // the twin sitting there looking like a separate account, with the old warning gone because
  // the row it compared against had been filtered out of the response.
  const number = "12-3456-0000123-00";
  const linked = await stubDiscovery(page, [
    bankAccount("acc_hers", "Everyday", "ASB", "auth_hers", { account_number: number, joint: true }),
    bankAccount("acc_his", "Joint acct", "ASB", "auth_his", { account_number: number, joint: true }),
    bankAccount("acc_solo", "Savings", "ASB", "auth_hers", { account_number: "12-3456-0000999-00" }),
  ]);
  await openAkahuConnect(page);

  const hers = page.locator(".row-card", { hasText: "Everyday" });
  const his = page.locator(".row-card", { hasText: "Joint acct" });
  const solo = page.locator(".row-card", { hasText: "Savings" });

  // Both views carry the badge; the account only one login can see does not.
  await expect(hers.locator(".badge.joint")).toHaveText("Joint");
  await expect(his.locator(".badge.joint")).toHaveText("Joint");
  await expect(solo.locator(".badge.joint")).toHaveCount(0);

  await hers.getByRole("button", { name: "Everyday" }).click();
  // Settled rather than asked: the control is visible, so the rule is legible, and disabled,
  // so it cannot be answered differently.
  const owner = hers.getByLabel("Owner");
  await expect(owner).toBeDisabled();
  await expect(owner).toHaveValue("joint");

  await hers.getByRole("button", { name: "Link account" }).click();

  expect(linked).toHaveLength(1);
  expect(linked[0].postDataJSON().new_account.ownership).toEqual({ kind: "joint" });

  // The row that was linked, and the other holder's view of the same account, both go. The
  // unrelated account stays — this prunes one account, not a login.
  await expect(hers).toHaveCount(0);
  await expect(his).toHaveCount(0);
  await expect(solo).toHaveCount(1);
  await expect(page.getByText("Linked Everyday as a joint account.")).toBeVisible();
});

test("a mortgage's original amount arrives prefilled from its drawdown", async ({ page }) => {
  // The field is required to link at all — a mortgage without its terms cannot be forecast, so
  // `AMORTISING_REQUIRED` is enforced on the link path too — which is exactly why prefilling it
  // is worth doing. It is also the term people mistype: what comes to mind is the balance the
  // day they connected the bank, not the advance that opened the loan.
  //
  // The server decides the number (it reads the drawdown out of the account's history); the page
  // only has to put it in the box, in major units, and leave it editable.
  const linked = await stubDiscovery(page, [
    {
      ...bankAccount("acc_mortgage", "Prime Housing Lending", "ASB", "auth_one"),
      kind_hint: "mortgage",
      balance_minor: -484_210_00,
      original_amount_hint_minor: 485_000_00,
    },
  ]);
  await openAkahuConnect(page);

  const row = page.locator(".row-card", { hasText: "Prime Housing Lending" });
  await row.getByRole("button", { name: "Prime Housing Lending" }).click();

  const original = row.getByLabel("Original amount borrowed");
  await expect(original).toHaveValue("485000");
  // Prefilled, not imposed: a lender who advanced a different figure can still type over it.
  await expect(original).toBeEnabled();

  // The rest of the terms are still asked for — a feed reports a mortgage's balance, never its
  // rate or its term, so this saves one field rather than the whole form. It is the field worth
  // saving: the others are read off the loan document, and this one people reconstruct from
  // memory. Floating, so the refix terms a fixed rate would also demand stay out of the way.
  await row.getByLabel("Interest rate (%)").fill("5.49");
  await row.getByLabel("Rate type").selectOption("floating");
  await row.getByLabel("Overall term (months)").fill("360");
  await row.getByLabel("Start date").fill("2026-03-02");

  const link = row.getByRole("button", { name: "Link account" });
  await expect(link).toBeEnabled();
  await link.click();

  expect(linked).toHaveLength(1);
  expect(linked[0].postDataJSON().new_account.metadata).toMatchObject({
    original_amount_minor: 48_500_000,
  });
});

test("a mortgage whose drawdown is out of view is left blank, and still asks", async ({ page }) => {
  await stubDiscovery(page, [
    {
      ...bankAccount("acc_mortgage", "Old Lending", "ASB", "auth_one"),
      kind_hint: "mortgage",
      balance_minor: -512_400_00,
    },
  ]);
  await openAkahuConnect(page);

  const row = page.locator(".row-card", { hasText: "Old Lending" });
  await row.getByRole("button", { name: "Old Lending" }).click();

  await expect(row.getByLabel("Original amount borrowed")).toHaveValue("");
  // Still required, so the dialog holds the link until it is answered — an empty field the user
  // fills in is the honest outcome when the history cannot say.
  await expect(row.getByRole("button", { name: "Link account" })).toBeDisabled();
});
