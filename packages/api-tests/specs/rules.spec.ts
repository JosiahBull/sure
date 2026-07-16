import { test, expect } from "../fixtures";
import type { Schemas, SureClient } from "../../client/src/index";
import { createAccount, createCategory, createTransaction, getTransaction } from "../helpers";

function mkRule(api: SureClient, rule: Partial<Schemas["SaveRule"]> & { name: string; expression: string }) {
  return api.POST("/api/rules", {
    body: { overwrite_manual: false, stop_on_match: false, priority: 0, enabled: true, ...rule },
  });
}
const categoryOf = async (api: SureClient, id: number) => (await getTransaction(api, id)).category_id;

test("an invalid expression is rejected", async ({ api }) => {
  const { response } = await mkRule(api, { name: "bad", expression: "this is not (valid" });
  expect(response.status).toBe(422);
});

test("running a rule classifies, and undo reverts", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const groceries = await createCategory(api, "Groceries");
  const countdown = await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -5000, description: "Countdown Supermarket" });
  const newworld = await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -3000, description: "New World Metro" });
  const salary = await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: 250000, description: "ACME Payroll" });

  const { data: rule } = await mkRule(api, {
    name: "Groceries",
    expression: "is_expense and (contains(lower(description),'countdown') or contains(lower(description),'new world'))",
    set_category_id: groceries.id,
  });

  const run = await api.POST("/api/rules/{id}/run", { params: { path: { id: rule!.id } } });
  expect(run.data?.matched).toBe(2);
  expect(run.data?.changed).toBe(2);
  const runId = run.data!.run_id;

  expect(await categoryOf(api, countdown.id)).toBe(groceries.id);
  expect(await categoryOf(api, newworld.id)).toBe(groceries.id);
  expect(await categoryOf(api, salary.id)).toBeNull();

  const runs = await api.GET("/api/rules/runs", {});
  expect(runs.data?.length).toBe(1);
  const apps = await api.GET("/api/rules/runs/{run_id}", { params: { path: { run_id: runId } } });
  expect(apps.data?.length).toBe(2);

  // Re-running is idempotent.
  const rerun = await api.POST("/api/rules/{id}/run", { params: { path: { id: rule!.id } } });
  expect(rerun.data?.changed).toBe(0);

  const undo = await api.POST("/api/rules/runs/{run_id}/undo", { params: { path: { run_id: runId } } });
  expect(undo.data?.changed).toBe(2);
  expect(await categoryOf(api, countdown.id)).toBeNull();
  expect(await categoryOf(api, newworld.id)).toBeNull();
});

test("preview does not mutate", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const a = await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-01", amount_minor: -5000, description: "Countdown" });
  await createTransaction(api, { account_id: acc.id, posted_at: "2026-01-01", amount_minor: -200000, description: "Rent" });

  const preview = await api.POST("/api/rules/preview", { body: { expression: "contains(lower(description),'countdown')" } });
  expect(preview.data?.matched).toBe(1);
  expect(preview.data?.sample[0].transaction_id).toBe(a.id);
  expect(await categoryOf(api, a.id)).toBeNull();
});

test("manual categorisation is protected unless overwrite is set", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const groceries = await createCategory(api, "Groceries");
  const dining = await createCategory(api, "Dining");
  const t = await createTransaction(api, { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -5000, description: "Countdown" });

  // Manually file under Dining.
  await api.PUT("/api/transactions/{id}", {
    params: { path: { id: t.id } },
    body: { account_id: acc.id, posted_at: "2026-02-10", amount_minor: -5000, description: "Countdown", category_id: dining.id },
  });

  const { data: rule } = await mkRule(api, {
    name: "G",
    expression: "contains(lower(description),'countdown')",
    set_category_id: groceries.id,
  });
  const run = await api.POST("/api/rules/{id}/run", { params: { path: { id: rule!.id } } });
  expect(run.data?.changed).toBe(0);
  expect(await categoryOf(api, t.id)).toBe(dining.id);

  // With overwrite enabled it wins.
  await api.PUT("/api/rules/{id}", {
    params: { path: { id: rule!.id } },
    body: { name: "G", expression: "contains(lower(description),'countdown')", set_category_id: groceries.id, overwrite_manual: true, stop_on_match: false, priority: 0, enabled: true },
  });
  const run2 = await api.POST("/api/rules/{id}/run", { params: { path: { id: rule!.id } } });
  expect(run2.data?.changed).toBe(1);
  expect(await categoryOf(api, t.id)).toBe(groceries.id);
});
