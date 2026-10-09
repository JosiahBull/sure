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

// A code that isn't in `currencies` has no minor-unit scale and no exchange rate (a rate row's
// currency is an FK into that table), so every report used to answer 200 with every account
// named in `unconverted` and every total zero — indistinguishable from an empty ledger. Same
// treatment as `interval` above: 400, naming the code.
test("an unknown ?currency= is rejected on every report, not answered with an empty one", async ({ api }) => {
  await createAccount(api, "Everyday", "bank");

  for (const path of ["/api/reports/net-worth", "/api/reports/category-breakdown", "/api/reports/sankey", "/api/reports/balances"] as const) {
    const { response, error } = await api.GET(path, { params: { query: { currency: "ZZZ" } } });
    expect(response.status, path).toBe(400);
    expect(JSON.stringify(error), path).toContain("ZZZ");
  }

  // The configured base is still fine when named explicitly, and so is omitting it.
  const explicit = await api.GET("/api/reports/balances", { params: { query: { currency: "NZD" } } });
  expect(explicit.response.status).toBe(200);
  expect(explicit.data?.currency).toBe("NZD");
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

// ---- sankey: the cash basis ------------------------------------------------
// The graph answers one of two questions and says which. On `cash` every movement of the
// household's liquid money counts, including the ones a transfer rule and a link would
// otherwise hide; on `spending` none of them do.

test("the cash basis draws a mortgage payment as interest plus a labelled crossing", async ({ api }) => {
  // The shape ASB actually posts: interest charged to the facility the payment is drawn from,
  // and the principal moved from that facility into the loan. Two rows, not one — so nothing
  // here has to derive the split.
  const jam = await createAccount(api, "The Jam", "revolving_credit");
  const mortgage = await createAccount(api, "Home Mortgage", "mortgage");
  const interestCat = await createCategory(api, "Interest charged", "expense");
  const transferCat = await createCategory(api, "Transfer", "transfer");

  await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -95_202,
    description: "LOAN REPAYMENT 006INTEREST",
    category_id: interestCat.id,
  });
  const out = await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -44_530,
    description: "LOAN REPAYMENT 006PRINCIPAL",
    category_id: transferCat.id,
  });
  const inn = await createTransaction(api, {
    account_id: mortgage.id,
    posted_at: "2026-01-10",
    amount_minor: 44_530,
    description: "Principal payment",
    category_id: transferCat.id,
  });
  const linked = await api.POST("/api/transactions/{id}/link", {
    params: { path: { id: out.id } },
    body: { linked_transaction_id: inn.id },
  });
  expect(linked.response.status).toBe(200);

  const cash = (
    await api.GET("/api/reports/sankey", { params: { query: { ...SANKEY_WINDOW, basis: "cash" } } })
  ).data!;
  // The principal: one node named after the account it reached, never after the "Transfer"
  // category both legs carry.
  expect(node(cash, `acct:${mortgage.id}`)).toMatchObject({
    kind: "crossing",
    label: "Home Mortgage",
    account_id: mortgage.id,
    side: "expense",
    depth: 0,
    category_id: null,
  });
  expect(cash.links).toContainEqual(
    expect.objectContaining({ source: "center", target: `acct:${mortgage.id}`, value_minor: 44_530 })
  );
  // …counted once. The loan's own row is the far leg, outside the perimeter.
  expect(cash.links.filter((l) => l.target === `acct:${mortgage.id}`)).toHaveLength(1);
  // The interest stays the ordinary expense category it always was.
  expect(cash.links).toContainEqual(
    expect.objectContaining({ source: "center", target: `out:${interestCat.id}`, value_minor: 95_202 })
  );

  // On the spending basis the principal is gone and the interest is untouched.
  const spending = (
    await api.GET("/api/reports/sankey", { params: { query: { ...SANKEY_WINDOW, basis: "spending" } } })
  ).data!;
  expect(spending.nodes.some((n) => n.kind === "crossing")).toBe(false);
  expect(spending.links).toContainEqual(
    expect.objectContaining({ source: "center", target: `out:${interestCat.id}`, value_minor: 95_202 })
  );
});

test("naming the loan on its interest rows splits the crossing into interest and principal", async ({
  api,
}) => {
  // The question the basis exists for: what does the mortgage cost? The bank charges interest to
  // the facility rather than to the loan, so that row has nothing to link to — naming the
  // counterparty is the only way to say the two belong together.
  const jam = await createAccount(api, "The Jam", "revolving_credit");
  const mortgage = await createAccount(api, "Home Mortgage", "mortgage");
  const interestCat = await createCategory(api, "Interest charged", "expense");
  const transferCat = await createCategory(api, "Transfer", "transfer");

  await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -95_202,
    description: "LOAN REPAYMENT 006INTEREST",
    category_id: interestCat.id,
    counterparty_account_id: mortgage.id,
  });
  const out = await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -44_530,
    description: "LOAN REPAYMENT 006PRINCIPAL",
    category_id: transferCat.id,
  });
  const inn = await createTransaction(api, {
    account_id: mortgage.id,
    posted_at: "2026-01-10",
    amount_minor: 44_530,
    description: "Principal payment",
    category_id: transferCat.id,
  });
  await api.POST("/api/transactions/{id}/link", {
    params: { path: { id: out.id } },
    body: { linked_transaction_id: inn.id },
  });

  const g = (
    await api.GET("/api/reports/sankey", { params: { query: { ...SANKEY_WINDOW, basis: "cash" } } })
  ).data!;
  // The whole payment crosses…
  expect(g.links).toContainEqual(
    expect.objectContaining({ source: "center", target: `acct:${mortgage.id}`, value_minor: 139_732 })
  );
  // …and splits by what the loan's own ledger says the balance did. Nothing read a rate or a memo.
  expect(node(g, `acct:${mortgage.id}:cost`)).toMatchObject({ label: "Interest", depth: 1 });
  expect(node(g, `acct:${mortgage.id}:rest`)).toMatchObject({ label: "Principal", depth: 1 });
  expect(g.links).toContainEqual(
    expect.objectContaining({
      source: `acct:${mortgage.id}`,
      target: `acct:${mortgage.id}:cost`,
      value_minor: 95_202,
    })
  );
});

test("the net-worth basis keeps the interest and drops the principal", async ({ api }) => {
  const jam = await createAccount(api, "The Jam", "revolving_credit");
  const mortgage = await createAccount(api, "Home Mortgage", "mortgage");
  const interestCat = await createCategory(api, "Interest charged", "expense");
  const transferCat = await createCategory(api, "Transfer", "transfer");

  await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -95_202,
    description: "LOAN REPAYMENT 006INTEREST",
    category_id: interestCat.id,
    counterparty_account_id: mortgage.id,
  });
  const out = await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -44_530,
    description: "LOAN REPAYMENT 006PRINCIPAL",
    category_id: transferCat.id,
  });
  const inn = await createTransaction(api, {
    account_id: mortgage.id,
    posted_at: "2026-01-10",
    amount_minor: 44_530,
    description: "Principal payment",
    category_id: transferCat.id,
  });
  await api.POST("/api/transactions/{id}/link", {
    params: { path: { id: out.id } },
    body: { linked_transaction_id: inn.id },
  });

  const g = (
    await api.GET("/api/reports/sankey", {
      params: { query: { ...SANKEY_WINDOW, basis: "net_worth" } },
    })
  ).data!;
  // Repaying a loan is a net zero change: only the interest survives, and it says so.
  expect(g.links).toContainEqual(
    expect.objectContaining({ source: "center", target: `acct:${mortgage.id}`, value_minor: 95_202 })
  );
  expect(node(g, `acct:${mortgage.id}`)?.label).toBe("Interest — Home Mortgage");
  expect(g.nodes.some((n) => n.id.endsWith(":rest")), "the principal is not drawn").toBe(false);
  // The spine and the leftover name the question, so the two bases cannot be confused.
  expect(node(g, "center")?.label).toBe("Net worth");
});

test("a rule can name the counterparty, and the run is undoable", async ({ api }) => {
  const jam = await createAccount(api, "The Jam", "revolving_credit");
  const mortgage = await createAccount(api, "Home Mortgage", "mortgage");
  const tx = await createTransaction(api, {
    account_id: jam.id,
    posted_at: "2026-01-10",
    amount_minor: -95_202,
    description: "LOAN REPAYMENT 006INTEREST",
  });

  const rule = await api.POST("/api/rules", {
    body: {
      name: "Loan interest → Home Mortgage",
      expression: "contains(lower(description), '006interest')",
      set_counterparty_account_id: mortgage.id,
      overwrite_manual: false,
      stop_on_match: false,
      priority: 0,
      enabled: true,
    },
  });
  expect(rule.response.status).toBe(201);
  const run = await api.POST("/api/rules/{id}/run", { params: { path: { id: rule.data!.id } } });
  expect(run.data?.changed).toBe(1);

  const after = await api.GET("/api/transactions/{id}", { params: { path: { id: tx.id } } });
  expect(after.data?.counterparty_account_id).toBe(mortgage.id);

  // Undoable like every other rule action — otherwise a mis-aimed rule is a manual repair job.
  const undone = await api.POST("/api/rules/runs/{run_id}/undo", {
    params: { path: { run_id: run.data!.run_id } },
  });
  expect(undone.data?.changed).toBe(1);
  const reverted = await api.GET("/api/transactions/{id}", { params: { path: { id: tx.id } } });
  expect(reverted.data?.counterparty_account_id).toBeNull();
});

test("a transfer between two cash accounts is internal on every basis", async ({ api }) => {
  const everyday = await createAccount(api, "Everyday", "bank");
  const savings = await createAccount(api, "Savings", "savings");
  const transferred = await api.POST("/api/transfers", {
    body: {
      from_account_id: everyday.id,
      to_account_id: savings.id,
      posted_at: "2026-01-10",
      from_amount_minor: 100_000,
      description: "To savings",
    },
  });
  expect(transferred.response.status).toBe(201);
  await createTransaction(api, { account_id: everyday.id, posted_at: "2026-01-05", amount_minor: 500_000 });

  for (const basis of ["cash", "spending"] as const) {
    const g = (await api.GET("/api/reports/sankey", { params: { query: { ...SANKEY_WINDOW, basis } } })).data!;
    expect(g.nodes.some((n) => n.kind === "crossing"), basis).toBe(false);
    // Only the deposit survives, so the hub carries it and nothing else.
    expect(g.links.filter((l) => l.target === "center").map((l) => l.value_minor), basis).toEqual([500_000]);
  }
});

test("an unlinked transfer-category row is cash but not spending", async ({ api }) => {
  // A student-loan living-cost drawdown: real money arriving, unlinked because the loan is
  // balance-only and has no opposite row to pair with.
  const acc = await createAccount(api, "Everyday", "bank");
  const drawdowns = await createCategory(api, "Student loan drawdowns", "transfer");
  await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-01-08",
    amount_minor: 33_348,
    description: "STUDYLINK (MSD) LC PAYMENT",
    category_id: drawdowns.id,
  });

  const cash = (
    await api.GET("/api/reports/sankey", { params: { query: { ...SANKEY_WINDOW, basis: "cash" } } })
  ).data!;
  expect(linkInto(cash, `in:${drawdowns.id}`)).toBe(33_348);

  const spending = (
    await api.GET("/api/reports/sankey", { params: { query: { ...SANKEY_WINDOW, basis: "spending" } } })
  ).data!;
  expect(node(spending, `in:${drawdowns.id}`)).toBeUndefined();
});

test("a month that spent more than it earned draws a deficit into the hub", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-05", amount_minor: 100_000 });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-15", amount_minor: -120_000 });

  const g = (await getSankey(api)).data!;
  expect(node(g, "deficit")).toMatchObject({ kind: "deficit", side: null });
  // A source *into* the hub, not a sink out of it: the other way round draws a ribbon
  // backwards through the centre.
  expect(g.links).toContainEqual(
    expect.objectContaining({ source: "deficit", target: "center", value_minor: 20_000 })
  );
  expect(g.nodes.some((n) => n.kind === "savings")).toBe(false);
});

test("an unrecognised basis is a bad request", async ({ api }) => {
  const r = await api.GET("/api/reports/sankey", {
    // The generated client types `basis` as the legal strings, which is the point — this is the
    // untyped caller the 400 exists for.
    params: { query: { ...SANKEY_WINDOW, basis: "networth" as "cash" } },
  });
  expect(r.response.status).toBe(400);
  expect(r.error?.error.message).toContain("networth");
});

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
