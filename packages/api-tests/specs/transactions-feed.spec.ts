import { test, expect } from "../fixtures";
import { createAccount, createCategory, createMerchant, createTransaction } from "../helpers";

/**
 * `/api/transactions/feed` and `/api/transactions/summary`: the infinitely-scrolling list and its
 * stat bar. The keyset itself is covered row-for-row in `sure_dal::transactions`'s tests; these
 * pin the wire contract — query parameters, the page shape, the 400s — that the SPA codes against.
 */

test("following next_cursor walks every row once, in order, with full-day totals", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  // Three rows a day for five days.
  for (let day = 1; day <= 5; day++) {
    for (const amount of [-1_000, -250, 4_000]) {
      await createTransaction(api, { account_id: acc.id, posted_at: `2026-02-0${day}`, amount_minor: amount });
    }
  }

  const seen: number[] = [];
  let cursor: string | undefined;
  let pages = 0;
  do {
    const { data, response } = await api.GET("/api/transactions/feed", {
      params: { query: { limit: 4, ...(cursor ? { cursor } : {}) } },
    });
    expect(response.status).toBe(200);
    pages++;
    seen.push(...data!.items.map((t) => t.id));
    // Each day on the page is reported whole, even when the page holds only part of it.
    for (const d of data!.days) expect(d).toEqual({ date: d.date, count: 3, net_minor: 2_750 });
    cursor = data!.next_cursor ?? undefined;
  } while (cursor);

  expect(pages).toBe(4);
  expect(new Set(seen).size).toBe(15);
  const { data: whole } = await api.GET("/api/transactions/feed", { params: { query: { limit: 500 } } });
  expect(whole!.items.map((t) => t.id)).toEqual(seen);
  expect(whole!.next_cursor ?? null).toBeNull();
});

test("sort, flow, subcategory and search filters are applied by the server", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const food = await createCategory(api, "Food");
  const groceries = await createCategory(api, "Groceries", "expense", food.id);
  const shop = await createMerchant(api, "Corner Dairy");
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-03-01", amount_minor: -500, category_id: food.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-03-02", amount_minor: -900, category_id: groceries.id });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-03-03", amount_minor: 7_000, description: "Salary" });
  await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-03-04",
    amount_minor: -300,
    description: "",
    merchant_id: shop.id,
  });

  const amounts = async (query: Record<string, unknown>) => {
    const { data, response } = await api.GET("/api/transactions/feed", { params: { query } });
    expect(response.status).toBe(200);
    return data!.items.map((t) => t.amount_minor);
  };
  expect(await amounts({ sort: "amount", dir: "asc" })).toEqual([-900, -500, -300, 7_000]);
  expect(await amounts({ flow: "income" })).toEqual([7_000]);
  // A parent category takes in its children, as the reports roll it up.
  expect(await amounts({ category_id: food.id, sort: "amount", dir: "desc" })).toEqual([-500, -900]);
  expect(await amounts({ uncategorized: true, sort: "amount", dir: "asc" })).toEqual([-300, 7_000]);
  // A row with no description is found by the name of the merchant it is linked to.
  expect(await amounts({ search: "dairy" })).toEqual([-300]);

  const { data: sum } = await api.GET("/api/transactions/summary", { params: { query: { flow: "expense" } } });
  expect(sum).toEqual({ count: 3, income_minor: 0, expenses_minor: -1_700 });
  const { data: all } = await api.GET("/api/transactions/summary", { params: { query: { category_id: food.id } } });
  expect(all).toEqual({ count: 2, income_minor: 0, expenses_minor: -1_400 });
});

test("a cursor is only good for the order it came from, and limit is bounded", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  for (let i = 1; i <= 3; i++) {
    await createTransaction(api, { account_id: acc.id, posted_at: `2026-04-0${i}`, amount_minor: -i });
  }
  const { data } = await api.GET("/api/transactions/feed", { params: { query: { limit: 1 } } });
  const cursor = data!.next_cursor!;
  expect(cursor).toBeTruthy();

  const other = await api.GET("/api/transactions/feed", { params: { query: { cursor, sort: "amount" } } });
  expect(other.response.status).toBe(400);
  const junk = await api.GET("/api/transactions/feed", { params: { query: { cursor: "not-a-cursor" } } });
  expect(junk.response.status).toBe(400);
  for (const limit of [0, 501]) {
    const r = await api.GET("/api/transactions/feed", { params: { query: { limit } } });
    expect(r.response.status, `limit=${limit}`).toBe(400);
  }
  const badSort = await api.GET("/api/transactions/feed", {
    // @ts-expect-error -- deliberately outside the enum, to prove the server refuses it
    params: { query: { sort: "colour" } },
  });
  expect(badSort.response.status).toBe(400);
});
