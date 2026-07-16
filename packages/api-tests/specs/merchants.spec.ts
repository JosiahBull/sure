import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount, createMerchant, createTransaction, getTransaction } from "../helpers";

const merchantOf = async (api: SureClient, id: number) => (await getTransaction(api, id)).merchant_id;

test("merchant CRUD is unique by name (case-insensitive)", async ({ api }) => {
  const created = await api.POST("/api/merchants", { body: { name: "Countdown" } });
  expect(created.response.status).toBe(201);

  const dup = await api.POST("/api/merchants", { body: { name: "countdown" } });
  expect(dup.response.status).toBe(409);

  const list = await api.GET("/api/merchants", {});
  expect(list.data?.length).toBe(1);
});

test("a transaction can be assigned a merchant", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const m = await createMerchant(api, "New World");
  const t = await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -5000, description: "NW Metro" });
  expect(await merchantOf(api, t.id)).toBeNull();

  await api.PUT("/api/transactions/{id}", {
    params: { path: { id: t.id } },
    body: { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -5000, description: "NW Metro", merchant_id: m.id },
  });
  expect(await merchantOf(api, t.id)).toBe(m.id);
});

test("a rule assigns a merchant, and undo reverts it", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const m = await createMerchant(api, "Countdown");
  const t = await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -5000, description: "Countdown Supermarket" });

  const rule = await api.POST("/api/rules", {
    body: {
      name: "Tag Countdown",
      expression: "contains(lower(description),'countdown')",
      set_merchant_id: m.id,
      overwrite_manual: false,
      stop_on_match: false,
      priority: 0,
      enabled: true,
    },
  });
  const rid = rule.data!.id;

  const run = await api.POST("/api/rules/{id}/run", { params: { path: { id: rid } } });
  expect(run.data?.matched).toBe(1);
  expect(run.data?.changed).toBe(1);
  expect(await merchantOf(api, t.id)).toBe(m.id);

  const undo = await api.POST("/api/rules/runs/{run_id}/undo", { params: { path: { run_id: run.data!.run_id } } });
  expect(undo.data?.changed).toBe(1);
  expect(await merchantOf(api, t.id)).toBeNull();
});
