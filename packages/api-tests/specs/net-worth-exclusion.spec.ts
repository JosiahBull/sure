import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount, createCategory, createTransaction } from "../helpers";

/**
 * Some balances are real, and yours to see, without being part of what you are worth: money
 * held for someone else, a company account sharing a login, a pot you track but don't count.
 *
 * These pin the two halves of that sentence. The flag is about the *pot*, so it leaves every
 * net-worth figure the app produces — the series, the balance-sheet total, and the forecast's
 * projection, which are three separate account enumerations behind the scenes. It is not about
 * the *movements*, so spending on the account keeps counting in the category reports.
 */

const WINDOW = { from: "2026-01-01", to: "2026-12-31" };

async function household(api: SureClient) {
  const groceries = (await createCategory(api, "Groceries")).id;
  const mine = await createAccount(api, "Everyday", "bank", "NZD", {
    opening_balance_minor: 1000_00,
    opening_balance_date: "2026-01-01",
  });
  // The pot that is not ours to count.
  const theirs = await createAccount(api, "Held for the club", "bank", "NZD", {
    opening_balance_minor: 5000_00,
    opening_balance_date: "2026-01-01",
  });
  await createTransaction(api, {
    account_id: theirs.id,
    posted_at: "2026-03-01",
    amount_minor: -200_00,
    description: "club groceries",
    category_id: groceries,
  });
  return { mine, theirs, groceries };
}

const exclude = (api: SureClient, id: number, excluded_from_net_worth: boolean) =>
  api.PUT("/api/accounts/{id}/excluded-from-net-worth", {
    params: { path: { id } },
    body: { excluded_from_net_worth },
  });

const latestNetWorth = async (api: SureClient) => {
  const { data } = await api.GET("/api/reports/net-worth", {
    params: { query: { ...WINDOW, interval: "month" } },
  });
  return data!.points[data!.points.length - 1].net_worth_minor;
};

const expenseTotal = async (api: SureClient) => {
  const { data } = await api.GET("/api/reports/category-breakdown", {
    params: { query: WINDOW },
  });
  return (data?.expense ?? []).reduce((sum, c) => sum + c.total_minor, 0);
};

test("excluding an account moves every net-worth figure by exactly its balance", async ({
  api,
}) => {
  const { theirs } = await household(api);

  const before = {
    netWorth: await latestNetWorth(api),
    balances: (await api.GET("/api/reports/balances", {})).data!.total_minor,
    spend: await expenseTotal(api),
  };
  // $1,000 + $5,000 less the $200 spent.
  expect(before.netWorth).toBe(5800_00);

  const { response } = await exclude(api, theirs.id, true);
  expect(response.status).toBe(200);

  const after = {
    netWorth: await latestNetWorth(api),
    balances: (await api.GET("/api/reports/balances", {})).data!.total_minor,
    spend: await expenseTotal(api),
  };

  // The club account holds $5,000 less the $200 it spent.
  expect(before.netWorth - after.netWorth).toBe(4800_00);
  expect(before.balances - after.balances).toBe(4800_00);
  // …and its spending is untouched, because the flag is about the pot, not the movements.
  expect(after.spend).toBe(before.spend);
  expect(after.spend).toBe(200_00);
});

test("an excluded account is still listed, and marked, so it can be put back", async ({ api }) => {
  const { theirs } = await household(api);
  await exclude(api, theirs.id, true);

  const { data } = await api.GET("/api/reports/balances", {});
  const row = data!.accounts.find((a) => a.account_id === theirs.id);
  // Listed, not hidden — hiding an account is what `archived` means, and a row you cannot see
  // is a row you cannot un-exclude.
  expect(row).toBeDefined();
  expect(row!.excluded_from_net_worth).toBe(true);
  expect(row!.value_minor).toBe(4800_00);

  // And it goes back.
  await exclude(api, theirs.id, false);
  expect(await latestNetWorth(api)).toBe(5800_00);
});

/**
 * The reason the flag is deliberately absent from `SaveAccount`. That body is a full replace
 * sent by the account form, the seed script, this suite's own helper and the provider-link
 * path; if the field lived on it, any caller that forgot it would silently clear the user's
 * setting on their next ordinary save. This is the test that fails the day someone "tidies up"
 * by moving it onto the DTO.
 */
test("a full-replace account save leaves the exclusion alone", async ({ api }) => {
  const { theirs } = await household(api);
  await exclude(api, theirs.id, true);

  const current = (await api.GET("/api/accounts/{id}", { params: { path: { id: theirs.id } } }))
    .data!;
  const { response } = await api.PUT("/api/accounts/{id}", {
    params: { path: { id: theirs.id } },
    body: {
      name: "Held for the club (renamed)",
      kind: current.kind,
      currency_code: current.currency_code,
      institution: current.institution ?? undefined,
      metadata: current.metadata,
      archived: current.archived,
      sort_order: current.sort_order,
      ownership: current.ownership,
    },
  });
  expect(response.status).toBe(200);

  const after = (await api.GET("/api/accounts/{id}", { params: { path: { id: theirs.id } } }))
    .data!;
  expect(after.name).toBe("Held for the club (renamed)");
  expect(after.excluded_from_net_worth).toBe(true);
});

test("excluding an unknown account is a 404", async ({ api }) => {
  const { response } = await exclude(api, 999_999, true);
  expect(response.status).toBe(404);
});

/**
 * The three snapshot column lists are easy to miss one of, and the failure is silent until
 * someone restores a backup and finds their setting gone.
 */
test("the exclusion survives an export and re-import", async ({ api }) => {
  const { theirs } = await household(api);
  await exclude(api, theirs.id, true);

  const exported = (await api.GET("/api/config/export", {})).data!;
  const { response } = await api.POST("/api/config/import", {
    body: exported as never,
  });
  expect(response.status).toBe(200);

  const restored = (await api.GET("/api/reports/balances", {})).data!.accounts.find(
    (a) => a.account_id === theirs.id
  );
  expect(restored?.excluded_from_net_worth).toBe(true);
});
