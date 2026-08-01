import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount, createCategory, createTransaction } from "../helpers";

const addPerson = (api: SureClient, name: string) =>
  api.POST("/api/people", { body: { name, sort_order: 0 } });

const WINDOW = { from: "2026-01-01", to: "2026-12-31" };

/**
 * A two-person household: Alex's card, Sam's card, a joint account — with one expense on
 * each, plus one on the joint account that Sam has claimed as their own.
 */
async function household(api: SureClient) {
  const alex = (await addPerson(api, "Alex")).data!;
  const sam = (await addPerson(api, "Sam")).data!;
  const groceries = (await createCategory(api, "Groceries")).id;

  const alexs = await createAccount(api, "Alex's card", "credit_card", "NZD", {
    ownership: { kind: "person", person_id: alex.id },
    opening_balance_minor: -1000_00,
  });
  const sams = await createAccount(api, "Sam's card", "credit_card", "NZD", {
    ownership: { kind: "person", person_id: sam.id },
    opening_balance_minor: -500_00,
  });
  const joint = await createAccount(api, "Joint", "bank", "NZD", {
    ownership: { kind: "joint" },
    opening_balance_minor: 3000_00,
  });

  const spend = (account_id: number, description: string, amount: number, ownership?: never | object) =>
    createTransaction(api, {
      account_id,
      posted_at: "2026-03-01",
      amount_minor: amount,
      description,
      category_id: groceries,
      ...(ownership ? { ownership: ownership as never } : {}),
    });

  await spend(alexs.id, "alex groceries", -100_00);
  await spend(sams.id, "sam groceries", -200_00);
  await spend(joint.id, "our groceries", -300_00);
  await spend(joint.id, "sam's claim on the joint card", -50_00, {
    kind: "person",
    person_id: sam.id,
  });

  return { alex, sam, groceries, alexs, sams, joint };
}

/**
 * Total expense in the category breakdown, for one attribution (or the household). The
 * report states expenses as positive magnitudes, not signed amounts.
 */
async function expenseTotal(api: SureClient, attributed_to?: string) {
  const { data, response } = await api.GET("/api/reports/category-breakdown", {
    params: { query: { ...WINDOW, attributed_to } },
  });
  expect(response.status).toBe(200);
  return (data?.expense ?? []).reduce((sum, c) => sum + c.total_minor, 0);
}

test("the category breakdown splits spending by who it belongs to", async ({ api }) => {
  const { alex, sam } = await household(api);

  // Alex: their own card only. Sam: their card plus the joint row they claimed.
  expect(await expenseTotal(api, String(alex.id))).toBe(100_00);
  expect(await expenseTotal(api, String(sam.id))).toBe(250_00);
  expect(await expenseTotal(api, "joint")).toBe(300_00);

  // The three buckets partition the household exactly — nothing lost, nothing counted twice.
  const householdTotal = await expenseTotal(api);
  expect(householdTotal).toBe(650_00);
  expect(100_00 + 250_00 + 300_00).toBe(householdTotal);
});

test("the money-flow graph is filtered the same way", async ({ api }) => {
  const { alex } = await household(api);
  const { data } = await api.GET("/api/reports/sankey", {
    params: { query: { ...WINDOW, attributed_to: String(alex.id) } },
  });
  const flows = (data?.links ?? []).reduce((sum, l) => sum + l.value_minor, 0);
  const all = await api.GET("/api/reports/sankey", { params: { query: WINDOW } });
  const allFlows = (all.data?.links ?? []).reduce((sum, l) => sum + l.value_minor, 0);
  expect(flows).toBeGreaterThan(0);
  expect(flows).toBeLessThan(allFlows);
});

/**
 * Net worth filters *accounts*, not transactions: a balance belongs to whoever owns the pot,
 * and a per-transaction override says who a movement was for. So Sam's claim on the joint
 * card moves their spending but not a cent of anyone's net worth.
 */
test("net worth is filtered by account owner, and ignores transaction overrides", async ({
  api,
}) => {
  const { alex, sam } = await household(api);
  const latest = async (attributed_to?: string) => {
    const { data } = await api.GET("/api/reports/net-worth", {
      params: { query: { ...WINDOW, interval: "month", attributed_to } },
    });
    return data!.points[data!.points.length - 1].net_worth_minor;
  };

  // Opening balances only: -1000 / -500 / +3000.
  expect(await latest(String(alex.id))).toBe(-1100_00);
  expect(await latest(String(sam.id))).toBe(-700_00);
  expect(await latest("joint")).toBe(2650_00);
  expect(await latest()).toBe(850_00);
  // ...and the parts still sum to the whole.
  expect(-1100_00 + -700_00 + 2650_00).toBe(850_00);
});

test("balances carry each account's owner, so the client can group by person", async ({
  api,
}) => {
  const { alex } = await household(api);
  const { data } = await api.GET("/api/reports/balances", {});
  const alexs = data!.accounts.find((a) => a.name === "Alex's card");
  const joint = data!.accounts.find((a) => a.name === "Joint");
  expect(alexs?.ownership).toEqual({ kind: "person", person_id: alex.id });
  expect(joint?.ownership).toEqual({ kind: "joint" });
});

test("an unparseable attribution is a 400 on every report", async ({ api }) => {
  for (const path of ["/api/reports/category-breakdown", "/api/reports/sankey"] as const) {
    const { response } = await api.GET(path, {
      params: { query: { attributed_to: "everyone" } },
    });
    expect(response.status, path).toBe(400);
  }
  const nw = await api.GET("/api/reports/net-worth", {
    params: { query: { attributed_to: "everyone" } },
  });
  expect(nw.response.status).toBe(400);
});
