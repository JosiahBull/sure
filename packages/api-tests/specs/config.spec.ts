import { test, expect } from "../fixtures";
import { createAccount, createCategory, createMerchant, createTransaction, getTransaction } from "../helpers";

test("export then import restores the exact state", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const cat = await createCategory(api, "Groceries");
  const merch = await createMerchant(api, "Countdown");
  const tx = await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-02-10",
    amount_minor: -5000,
    description: "Countdown",
    category_id: cat.id,
    merchant_id: merch.id,
  });

  const snapshot = await api.GET("/api/config/export", {});
  expect(snapshot.response.status).toBe(200);
  const snap = snapshot.data as { accounts: unknown[]; transactions: unknown[] };
  expect(snap.accounts.length).toBe(1);
  expect(snap.transactions.length).toBe(1);

  // Mutate: add a second account that should disappear on import.
  await createAccount(api, "Extra", "savings");

  const result = await api.POST("/api/config/import", { body: snapshot.data as never });
  expect(result.response.status).toBe(200);
  expect((result.data as { counts: { accounts: number } }).counts.accounts).toBe(1);

  // Only the original account survives, id preserved.
  const accounts = await api.GET("/api/accounts", {});
  expect(accounts.data?.length).toBe(1);
  expect(accounts.data![0].id).toBe(acc.id);

  // The transaction is back with its links.
  const restored = await getTransaction(api, tx.id);
  expect(restored.category_id).toBe(cat.id);
  expect(restored.merchant_id).toBe(merch.id);
  expect(restored.description).toBe("Countdown");
});

test("import rejects garbage", async ({ api }) => {
  const res = await api.POST("/api/config/import", { body: { not: "a snapshot" } as never });
  expect(res.response.status).toBe(422);
});
