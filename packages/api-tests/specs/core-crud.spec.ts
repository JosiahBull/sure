import { test, expect } from "../fixtures";
import { createAccount, createCategory, createTransaction, getTransaction } from "../helpers";

test("currencies are seeded", async ({ api }) => {
  const { data } = await api.GET("/api/currencies", {});
  const codes = (data ?? []).map((c) => c.code);
  expect(codes).toContain("NZD");
  expect(codes).toContain("USD");
});

test("settings default to NZD and can be updated", async ({ api }) => {
  const { data } = await api.GET("/api/settings", {});
  expect(data?.base_currency_code).toBe("NZD");

  const updated = await api.PUT("/api/settings", { body: { base_currency_code: "usd" } });
  expect(updated.response.status).toBe(200);
  expect(updated.data?.base_currency_code).toBe("USD");

  const bad = await api.PUT("/api/settings", { body: { base_currency_code: "ZZZ" } });
  expect(bad.response.status).toBe(422);
});

test("account lifecycle and classes", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const fetched = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  expect(fetched.data?.kind).toBe("bank");
  expect(fetched.data?.class).toBe("cash");

  const shares = await createAccount(api, "Startco Options", "shares_private", "USD");
  const sharesBody = await api.GET("/api/accounts/{id}", { params: { path: { id: shares.id } } });
  expect(sharesBody.data?.class).toBe("investment");

  const bad = await api.POST("/api/accounts", {
    body: { name: "Bad", kind: "bank", currency_code: "ZZZ", archived: false, sort_order: 0 },
  });
  expect(bad.response.status).toBe(422);

  const del = await api.DELETE("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  expect(del.response.status).toBe(204);
  const gone = await api.GET("/api/accounts/{id}", { params: { path: { id: acc.id } } });
  expect(gone.response.status).toBe(404);
});

test("categories nest and cycles are rejected", async ({ api }) => {
  const parent = await createCategory(api, "Housing");
  const child = await createCategory(api, "Mortgage", "expense", parent.id);
  const grandchild = await createCategory(api, "Interest", "expense", child.id);

  const { data: tree } = await api.GET("/api/categories/tree", {});
  const housing = (tree ?? []).find((c) => c.category.id === parent.id)!;
  expect(housing.children[0].category.id).toBe(child.id);
  expect(housing.children[0].children[0].category.id).toBe(grandchild.id);

  // Nesting Housing under its own grandchild is a cycle.
  const cycle = await api.PUT("/api/categories/{id}", {
    params: { path: { id: parent.id } },
    body: { name: "Housing", kind: "expense", parent_id: grandchild.id, sort_order: 0 },
  });
  expect(cycle.response.status).toBe(422);
});

test("transaction filters and the one-off toggle", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const groceries = await createCategory(api, "Groceries");

  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-10", amount_minor: -5000, category_id: groceries.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -8000, category_id: groceries.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-20", amount_minor: -100000, is_one_off: true });

  const feb = await api.GET("/api/transactions", { params: { query: { from: "2026-02-01", to: "2026-02-28" } } });
  expect(feb.data?.length).toBe(2);

  const grocery = await api.GET("/api/transactions", { params: { query: { category_id: groceries.id } } });
  expect(grocery.data?.length).toBe(2);

  const withoutOneOff = await api.GET("/api/transactions", { params: { query: { include_one_off: false } } });
  expect(withoutOneOff.data?.length).toBe(2);
  const all = await api.GET("/api/transactions", {});
  expect(all.data?.length).toBe(3);
});

test("a transfer creates a reciprocally-linked pair", async ({ api }) => {
  const checking = await createAccount(api, "Checking", "bank");
  const savings = await createAccount(api, "Savings", "savings");

  const { data: pair, response } = await api.POST("/api/transfers", {
    body: {
      from_account_id: checking.id,
      to_account_id: savings.id,
      posted_at: "2026-03-01",
      from_amount_minor: 25000,
      description: "Move to savings",
    },
  });
  expect(response.status).toBe(201);
  const [out, inflow] = pair!;
  expect(out.amount_minor).toBe(-25000);
  expect(inflow.amount_minor).toBe(25000);
  expect(out.linked_transaction_id).toBe(inflow.id);
  expect(inflow.linked_transaction_id).toBe(out.id);

  const unlinked = await api.DELETE("/api/transactions/{id}/link", { params: { path: { id: out.id } } });
  expect(unlinked.data?.linked_transaction_id).toBeNull();
  expect((await getTransaction(api, out.id)).linked_transaction_id).toBeNull();
});
