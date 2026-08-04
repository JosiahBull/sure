import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount, createCategory, createTransaction } from "../helpers";

const valuation = (api: SureClient, id: number, as_of: string, value_minor: number) =>
  api.POST("/api/accounts/{id}/valuations", { params: { path: { id } }, body: { as_of, value_minor } });

test("net worth combines cash flows and valuations", async ({ api }) => {
  const everyday = await createAccount(api, "Everyday", "bank");
  const house = await createAccount(api, "House", "real_estate");
  const mortgage = await createAccount(api, "Mortgage", "mortgage");

  await createTransaction(api, { account_id: everyday.id, posted_at: "2026-01-05", amount_minor: 500_000 });
  await createTransaction(api, { account_id: everyday.id, posted_at: "2026-01-10", amount_minor: -20_000 });
  await createTransaction(api, { account_id: everyday.id, posted_at: "2026-01-15", amount_minor: -150_000 });
  await valuation(api, house.id, "2026-01-01", 80_000_000);
  await valuation(api, mortgage.id, "2026-01-01", -50_000_000);

  const series = await api.GET("/api/reports/net-worth", {
    params: { query: { from: "2026-01-01", to: "2026-01-31", interval: "month" } },
  });
  expect(series.data?.currency).toBe("NZD");
  const last = series.data!.points.at(-1)!;
  expect(last.net_worth_minor).toBe(30_330_000);
  expect(last.assets_minor).toBe(80_330_000);
  expect(last.liabilities_minor).toBe(-50_000_000);
});

test("an unrecognised net-worth interval is rejected, not silently defaulted", async ({ api }) => {
  const { response } = await api.GET("/api/reports/net-worth", {
    params: { query: { interval: "fortnightly" } },
  });
  expect(response.status).toBe(400);
});

// A US$600 holding, with and without a rate to reach NZD by. No public fx-rate endpoint
// yet, so the rate is seeded through config import — which also exercises the snapshot
// restore path.
const usdHoldingSnapshot = (rates: { base_code: string; quote_code: string; as_of: string; rate: string }[]) => {
  const ts = "2026-01-01T00:00:00.000Z";
  return {
    version: 1,
    base_currency_code: "NZD",
    currencies: [
      { code: "NZD", name: "NZ Dollar", symbol: "$", decimal_places: 2, created_at: ts },
      { code: "USD", name: "US Dollar", symbol: "$", decimal_places: 2, created_at: ts },
    ],
    exchange_rates: rates,
    categories: [],
    merchants: [],
    accounts: [
      { id: 1, name: "US Shares", kind: "shares_us", currency_code: "USD", institution: null, metadata: "{}", archived: false, sort_order: 0, created_at: ts, updated_at: ts },
    ],
    transactions: [],
    valuations: [
      { id: 1, account_id: 1, as_of: "2026-01-01", value_minor: 60_000, currency_code: "USD", source: "manual", note: null, created_at: ts },
    ],
    rules: [],
    crons: [],
    providers: [],
    equity_grants: [],
    equity_exercises: [],
  };
};

test("net worth converts foreign-currency holdings (seeded via import)", async ({ api }) => {
  // 1 NZD = 0.6 USD => $600 USD = $1000 NZD.
  const snapshot = usdHoldingSnapshot([
    { base_code: "NZD", quote_code: "USD", as_of: "2026-01-01", rate: "0.6" },
  ]);
  const imported = await api.POST("/api/config/import", { body: snapshot as never });
  expect(imported.response.status).toBe(200);

  const series = await api.GET("/api/reports/net-worth", { params: { query: { from: "2026-01-01", to: "2026-01-31" } } });
  expect(series.data!.points.at(-1)!.net_worth_minor).toBe(100_000);
  // Nothing withheld, and the rate's own date is reported so a dead feed is visible.
  expect(series.data!.unconverted).toEqual([]);
  expect(series.data!.rates_as_of).toBe("2026-01-01");
});

test("a currency with no rate is reported as unconverted, never counted at parity", async ({ api }) => {
  // The identical holding with the rate removed. The failure this pins: for years an empty
  // rate table made every foreign amount convert at 1.0, so this US$600 read as NZ$600 of
  // net worth. It must now be absent from the total and named instead.
  const imported = await api.POST("/api/config/import", { body: usdHoldingSnapshot([]) as never });
  expect(imported.response.status).toBe(200);

  const series = await api.GET("/api/reports/net-worth", { params: { query: { from: "2026-01-01", to: "2026-01-31" } } });
  expect(series.data!.unconverted).toEqual(["USD"]);
  expect(series.data!.rates_as_of).toBeNull();
  const last = series.data!.points.at(-1)!;
  expect(last.net_worth_minor).toBe(0); // not 60_000 — that would be the parity bug
  expect(last.assets_minor).toBe(0);

  // Balances still lists the account at its true own-currency value; only the NZD roll-up
  // leaves it out, and says which currency it left out.
  const balances = await api.GET("/api/reports/balances", { params: { query: { to: "2026-01-31" } } });
  expect(balances.data!.unconverted).toEqual(["USD"]);
  expect(balances.data!.total_minor).toBe(0);
  const usAccount = balances.data!.accounts.find((a) => a.name === "US Shares")!;
  expect(usAccount.currency_code).toBe("USD");
  expect(usAccount.value_minor).toBe(60_000);
});

test("category breakdown splits income and expense", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const groceries = await createCategory(api, "Groceries");
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-05", amount_minor: 500_000 });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-10", amount_minor: -20_000, category_id: groceries.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-15", amount_minor: -150_000 });

  const b = await api.GET("/api/reports/category-breakdown", { params: { query: { from: "2026-01-01", to: "2026-01-31" } } });
  expect(b.data?.income.length).toBe(1);
  expect(b.data?.income[0].total_minor).toBe(500_000);
  expect(b.data?.income[0].category_id).toBeNull();

  const expense = b.data!.expense;
  expect(expense.length).toBe(2);
  expect(expense[0].total_minor).toBe(150_000); // uncategorised rent, sorted first
  const groc = expense.find((c) => c.category_id === groceries.id)!;
  expect(groc.total_minor).toBe(20_000);
});

test("sankey routes income through the centre to savings", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-05", amount_minor: 500_000 });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-15", amount_minor: -170_000 });

  const g = await api.GET("/api/reports/sankey", { params: { query: { from: "2026-01-01", to: "2026-01-31" } } });
  expect(g.data?.nodes.some((n) => n.id === "center")).toBe(true);
  expect(g.data?.nodes.some((n) => n.kind === "savings")).toBe(true);
  const savings = g.data!.links.find((l) => l.target === "savings")!;
  expect(savings.value_minor).toBe(330_000);
});

// ---- sankey category hierarchy ---------------------------------------------
// The graph fans the category tree out from the hub, up to MAX_CATEGORY_DEPTH levels per
// side, so a leaf's spend is visible at every level above it as well as its own.

const SANKEY_WINDOW = { from: "2026-01-01", to: "2026-01-31" } as const;
const getSankey = (api: SureClient) => api.GET("/api/reports/sankey", { params: { query: SANKEY_WINDOW } });

type Graph = NonNullable<Awaited<ReturnType<typeof getSankey>>["data"]>;

const node = (g: Graph, id: string) => g.nodes.find((n) => n.id === id);
/** The value flowing between a node and whatever sits on its hub-ward side. */
const linkInto = (g: Graph, id: string) =>
  g.links.find((l) => (id.startsWith("in:") ? l.source === id : l.target === id))?.value_minor;

test("sankey fans a category chain out into one node per level", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const income = await createCategory(api, "Income", "income");
  const employment = await createCategory(api, "Employment", "income", income.id);
  const partly = await createCategory(api, "Partly Group", "income", employment.id);
  await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-01-05",
    amount_minor: 500_000,
    category_id: partly.id,
  });

  const g = (await getSankey(api)).data!;
  // One node per level, each tagged with where it sits and which branch it belongs to.
  expect(node(g, `in:${income.id}`)).toMatchObject({ depth: 0, category_id: income.id, root_id: income.id });
  expect(node(g, `in:${employment.id}`)).toMatchObject({ depth: 1, root_id: income.id });
  expect(node(g, `in:${partly.id}`)).toMatchObject({ depth: 2, root_id: income.id, label: "Partly Group" });

  // ...chained leaf -> parent -> root -> hub, every hop carrying the leaf's full amount.
  expect(g.links).toContainEqual(
    expect.objectContaining({ source: `in:${partly.id}`, target: `in:${employment.id}`, value_minor: 500_000 })
  );
  expect(g.links).toContainEqual(
    expect.objectContaining({ source: `in:${employment.id}`, target: `in:${income.id}`, value_minor: 500_000 })
  );
  expect(g.links).toContainEqual(
    expect.objectContaining({ source: `in:${income.id}`, target: "center", value_minor: 500_000 })
  );
});

test("a category nested deeper than the cap rolls up into its deepest drawn ancestor", async ({ api }) => {
  // CRUD refuses a 4th level, so seed the over-deep tree through the snapshot restore —
  // which deliberately bypasses `validate`, and is exactly why the report can't assume the
  // cap holds.
  const ts = "2026-01-01T00:00:00.000Z";
  const cat = (id: number, name: string, parent_id: number | null) => ({
    id,
    name,
    parent_id,
    kind: "expense",
    color: null,
    icon: null,
    sort_order: 0,
    created_at: ts,
  });
  const snapshot = {
    version: 1,
    base_currency_code: "NZD",
    currencies: [{ code: "NZD", name: "NZ Dollar", symbol: "$", decimal_places: 2, created_at: ts }],
    exchange_rates: [],
    categories: [cat(1, "Housing", null), cat(2, "Utilities", 1), cat(3, "Power", 2), cat(4, "Off-peak", 3)],
    merchants: [],
    accounts: [
      { id: 1, name: "Everyday", kind: "bank", currency_code: "NZD", institution: "ASB", metadata: "{}", archived: false, sort_order: 0, created_at: ts, updated_at: ts },
    ],
    transactions: [
      { id: 1, account_id: 1, posted_at: "2026-01-10", amount_minor: -40_000, currency_code: "NZD", description: "Power", merchant: null, merchant_id: null, notes: null, category_id: 4, is_one_off: false, linked_transaction_id: null, provider: null, external_id: null, categorized_by_rule_id: null, attributed_to: null, created_at: ts, updated_at: ts },
    ],
    valuations: [],
    rules: [],
    crons: [],
    providers: [],
    equity_grants: [],
    equity_exercises: [],
  };
  expect((await api.POST("/api/config/import", { body: snapshot as never })).response.status).toBe(200);

  const g = (await getSankey(api)).data!;
  expect(node(g, "out:4")).toBeUndefined(); // the 4th level has no column to sit in
  expect(node(g, "out:3")).toMatchObject({ depth: 2, label: "Power" });
  expect(linkInto(g, "out:3")).toBe(40_000); // its spend surfaces here instead of vanishing
  expect(linkInto(g, "out:1")).toBe(40_000);
});

test("a parent's own transactions widen its link beyond its children's", async ({ api }) => {
  // Money booked straight onto a parent has nothing further out to flow from, so the
  // parent's link is wider than its children's by exactly that amount — the blank band the
  // chart draws on the node's inner face.
  const acc = await createAccount(api, "Everyday", "bank");
  const income = await createCategory(api, "Income", "income");
  const employment = await createCategory(api, "Employment", "income", income.id);
  const tx = (amount_minor: number, category_id: number) =>
    createTransaction(api, { account_id: acc.id, posted_at: "2026-01-05", amount_minor, category_id });
  await tx(700_000, employment.id);
  await tx(100_000, income.id);

  const g = (await getSankey(api)).data!;
  expect(linkInto(g, `in:${income.id}`)).toBe(800_000);
  expect(linkInto(g, `in:${employment.id}`)).toBe(700_000);
});

test("no parent link is ever narrower than the children feeding it", async ({ api }) => {
  // Awkward thirds plus a foreign-currency row: each node's total is rounded to minor units
  // independently, so children can round up past a naively-rounded parent.
  const ts = "2026-01-01T00:00:00.000Z";
  const cat = (id: number, name: string, parent_id: number | null) => ({
    id, name, parent_id, kind: "expense", color: null, icon: null, sort_order: 0, created_at: ts,
  });
  const txn = (id: number, amount_minor: number, currency_code: string, category_id: number) => ({
    id, account_id: 1, posted_at: "2026-01-10", amount_minor, currency_code, description: "x", merchant: null,
    merchant_id: null, notes: null, category_id, is_one_off: false, linked_transaction_id: null, provider: null,
    external_id: null, categorized_by_rule_id: null, attributed_to: null, created_at: ts, updated_at: ts,
  });
  const snapshot = {
    version: 1,
    base_currency_code: "NZD",
    currencies: [
      { code: "NZD", name: "NZ Dollar", symbol: "$", decimal_places: 2, created_at: ts },
      { code: "USD", name: "US Dollar", symbol: "$", decimal_places: 2, created_at: ts },
    ],
    exchange_rates: [{ base_code: "NZD", quote_code: "USD", as_of: "2026-01-01", rate: "0.6" }],
    categories: [cat(1, "Home", null), cat(2, "Utilities", 1), cat(3, "Power", 2), cat(4, "Water", 2), cat(5, "Rent", 1)],
    merchants: [],
    accounts: [
      { id: 1, name: "Everyday", kind: "bank", currency_code: "NZD", institution: "ASB", metadata: "{}", archived: false, sort_order: 0, created_at: ts, updated_at: ts },
    ],
    transactions: [
      txn(1, -33_333, "NZD", 3),
      txn(2, -33_333, "NZD", 4),
      txn(3, -33_334, "NZD", 2),
      txn(4, -20_000, "USD", 5),
      txn(5, 250_000, "NZD", 1),
    ],
    valuations: [], rules: [], crons: [], providers: [], equity_grants: [], equity_exercises: [],
  };
  expect((await api.POST("/api/config/import", { body: snapshot as never })).response.status).toBe(200);

  const g = (await getSankey(api)).data!;
  const outgoing = new Map<string, number>();
  const incoming = new Map<string, number>();
  for (const l of g.links) {
    outgoing.set(l.source, (outgoing.get(l.source) ?? 0) + l.value_minor);
    incoming.set(l.target, (incoming.get(l.target) ?? 0) + l.value_minor);
  }
  for (const n of g.nodes) {
    if (n.kind !== "income" && n.kind !== "expense") continue;
    // Income flows leaf -> hub, expense hub -> leaf, so a node's own link is on the hub side.
    const own = (n.kind === "income" ? outgoing : incoming).get(n.id) ?? 0;
    const children = (n.kind === "income" ? incoming : outgoing).get(n.id) ?? 0;
    expect(children, `children of ${n.label} exceed its own link`).toBeLessThanOrEqual(own);
  }
  // The hub balances: everything in comes back out, savings included.
  expect(outgoing.get("center")).toBe(incoming.get("center"));
});

test("the sankey is byte-identical across identical requests", async ({ api }) => {
  // Node order seeds d3-sankey's vertical layout, so a HashMap-ordered response would make
  // the chart jump between refreshes.
  const acc = await createAccount(api, "Everyday", "bank");
  const income = await createCategory(api, "Income", "income");
  const housing = await createCategory(api, "Housing");
  for (const [name, parent] of [["Rent", housing], ["Power", housing], ["Water", housing]] as const) {
    const child = await createCategory(api, name, "expense", parent.id);
    await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-10", amount_minor: -10_000, category_id: child.id });
  }
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-05", amount_minor: 500_000, category_id: income.id });

  const [a, b] = [(await getSankey(api)).data, (await getSankey(api)).data];
  expect(JSON.stringify(a)).toBe(JSON.stringify(b));
});
