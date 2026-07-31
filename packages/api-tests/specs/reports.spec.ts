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

test("net worth converts foreign-currency holdings (seeded via import)", async ({ api }) => {
  // No public fx-rate endpoint yet, so seed a rate + USD holding through config import —
  // which also exercises the snapshot restore path. 1 NZD = 0.6 USD => $600 USD = $1000 NZD.
  const ts = "2026-01-01T00:00:00.000Z";
  const snapshot = {
    version: 1,
    base_currency_code: "NZD",
    currencies: [
      { code: "NZD", name: "NZ Dollar", symbol: "$", decimal_places: 2, created_at: ts },
      { code: "USD", name: "US Dollar", symbol: "$", decimal_places: 2, created_at: ts },
    ],
    exchange_rates: [{ base_code: "NZD", quote_code: "USD", as_of: "2026-01-01", rate: "0.6" }],
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
  const imported = await api.POST("/api/config/import", { body: snapshot as never });
  expect(imported.response.status).toBe(200);

  const series = await api.GET("/api/reports/net-worth", { params: { query: { from: "2026-01-01", to: "2026-01-31" } } });
  expect(series.data!.points.at(-1)!.net_worth_minor).toBe(100_000);
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
