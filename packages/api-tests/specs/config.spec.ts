import { test, expect } from "../fixtures";
import {
  createAccount,
  createCategory,
  createMerchant,
  createTransaction,
  getTransaction,
  postOversized,
} from "../helpers";

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

/**
 * FX rates and the currencies they reference are both wiped and both restored by an import,
 * so a snapshot may legitimately drop a currency the database currently holds a rate for.
 *
 * Background: the poller used to write a second, latest-only `exchange_rate_cache` table which
 * was *not* in the import wipe list while `currencies` was, and because import defers
 * foreign-key checks to COMMIT, any such import died there with a bare `FOREIGN KEY constraint
 * failed` naming no table. Folding the cache into `exchange_rates` removed that dangling
 * reference.
 *
 * Note what this test can and cannot do: it seeds the rate through `/api/config/import`, which
 * is the only route that ever wrote a rate over HTTP — the old cache was written *solely* by
 * the background poller, so this test would have passed before the fix too. It pins the
 * property that matters going forward (dropping a currency is safe for every table that
 * references it), not the historical cache bug, which is now unreachable by construction: the
 * table is gone and `exchange_rates` is in the wipe list.
 */
test("a snapshot that drops a currency the database has a rate for still imports", async ({
  api,
}) => {
  const exported = (await api.GET("/api/config/export", {})).data as Record<string, unknown>;
  const currencies = exported.currencies as { code: string }[];
  expect(currencies.map((c) => c.code)).toContain("USD");

  // Give the database a NZD/USD rate, the shape a completed poll leaves behind.
  const seeded = await api.POST("/api/config/import", {
    body: {
      ...exported,
      exchange_rates: [
        { base_code: "NZD", quote_code: "USD", as_of: "2026-01-01", rate: "0.6" },
      ],
    } as never,
  });
  expect(seeded.response.status).toBe(200);

  // Now an older/leaner snapshot that knows nothing about USD or any rate.
  const withoutUsd = await api.POST("/api/config/import", {
    body: {
      ...exported,
      currencies: currencies.filter((c) => c.code !== "USD"),
      exchange_rates: [],
    } as never,
  });
  expect(withoutUsd.response.status).toBe(200);

  // And the server is still serving, i.e. the transaction committed rather than poisoning it.
  expect((await api.GET("/api/health", {})).response.status).toBe(200);
});

test("import rejects garbage", async ({ api }) => {
  const res = await api.POST("/api/config/import", { body: { not: "a snapshot" } as never });
  expect(res.response.status).toBe(422);
});

// ---- malformed and hostile snapshots ----------------------------------------------------

/**
 * `POST /api/config/import` is the most destructive endpoint in the API: it clears every
 * table and re-inserts from the body. So the bar is higher than "fails cleanly" — a bad
 * snapshot must leave the existing data **exactly as it was**, which is what the DAL's
 * single transaction is for. These cases check that, and that a hostile body can't take the
 * process down instead.
 */
async function postRaw(baseURL: string, body: string, contentType = "application/json") {
  return fetch(`${baseURL}/api/config/import`, {
    method: "POST",
    headers: { "Content-Type": contentType },
    body,
  });
}

test("a body that isn't JSON at all fails cleanly", async ({ server }) => {
  for (const body of ["", "not json", "{", "[1,2,3", '{"accounts":']) {
    const res = await postRaw(server.baseURL, body);
    expect([400, 422]).toContain(res.status);
  }
});

test("a JSON body of the wrong shape fails cleanly", async ({ server }) => {
  for (const body of [
    "null",
    "[]",
    "42",
    '"a string"',
    '{"accounts": "not an array"}',
    '{"accounts": [{"id": "not a number"}]}',
    // Right keys, wrong element types.
    '{"currencies": [1, 2, 3]}',
  ]) {
    const res = await postRaw(server.baseURL, body);
    expect([400, 422]).toContain(res.status);
  }
});

test("a rejected snapshot leaves the existing data untouched", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const tx = await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-02-10",
    amount_minor: -5000,
    description: "Before the bad import",
  });

  // A snapshot that starts out plausible and turns invalid part-way through: a transaction
  // pointing at an account the snapshot never defines. The whole thing has to roll back.
  const good = (await api.GET("/api/config/export", {})).data as Record<string, unknown>;
  const poisoned = JSON.stringify({
    ...good,
    accounts: [],
    transactions: [
      {
        id: 9001,
        account_id: 424242,
        posted_at: "2026-01-01T12:00:00+00:00",
        amount_minor: -100,
        currency_code: "NZD",
        description: "Orphan",
        is_one_off: false,
        created_at: "2026-01-01T00:00:00Z",
        updated_at: "2026-01-01T00:00:00Z",
      },
    ],
  });
  const res = await postRaw(server.baseURL, poisoned);
  expect([400, 422, 500]).toContain(res.status);

  // The database is exactly as it was.
  const accounts = await api.GET("/api/accounts", {});
  expect(accounts.data?.map((a) => a.id)).toEqual([acc.id]);
  const restored = await getTransaction(api, tx.id);
  expect(restored.description).toBe("Before the bad import");
});

test("deeply nested JSON is refused rather than exhausting the stack", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  // serde_json bounds recursion; this must come back as an error, not a crash.
  const deep = "[".repeat(20_000) + "]".repeat(20_000);
  const res = await postRaw(server.baseURL, `{"accounts": ${deep}}`);
  expect([400, 413, 422]).toContain(res.status);

  // Still serving, and the account survived.
  const accounts = await api.GET("/api/accounts", {});
  expect(accounts.data?.map((a) => a.id)).toEqual([acc.id]);
});

test("a snapshot body over the size limit is rejected by the server", async ({ server }) => {
  // The route carries its own 32 MB ceiling, well above any real snapshot. Probed over a raw
  // socket rather than `fetch`: the cap is enforced part-way through the upload, so the close
  // is an RST and `undici` discards the 413 it was already sent — see `postOversized`.
  const res = await postOversized(server.baseURL, 33 * 1024 * 1024, {
    path: "/api/config/import",
  });
  expect(res.status).toBe(413);
});

test("the server survives a burst of malformed snapshots", async ({ api, server }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  await Promise.all(
    Array.from({ length: 20 }, (_, i) =>
      postRaw(server.baseURL, i % 2 ? "{" : '{"accounts": [{"id": "x"}]}')
    )
  );
  const health = await api.GET("/api/health", {});
  expect(health.response.status).toBe(200);
  const accounts = await api.GET("/api/accounts", {});
  expect(accounts.data?.map((a) => a.id)).toEqual([acc.id]);
});
