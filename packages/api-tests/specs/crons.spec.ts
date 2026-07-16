import { test, expect } from "../fixtures";
import type { Schemas, SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

function mkCron(api: SureClient, cron: Partial<Schemas["SaveCron"]> & { name: string; account_id: number; kind: string; start_date: string }) {
  return api.POST("/api/crons", { body: { enabled: true, ...cron } });
}

test("an appreciation cron compounds and is idempotent", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: house.id } },
    body: { as_of: "2026-01-01", value_minor: 100_000_000 },
  });

  const { data: cron } = await mkCron(api, {
    name: "House appreciation",
    account_id: house.id,
    kind: "appreciation",
    rate_bps: 100,
    start_date: "2026-02-01",
    day_of_month: 1,
  });

  const run = await api.POST("/api/crons/{id}/run", { params: { path: { id: cron!.id }, query: { to: "2026-04-01" } } });
  expect(run.data?.applied).toBe(3); // Feb, Mar, Apr

  const vals = await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: house.id } } });
  const newest = vals.data![0].value_minor; // ~ 1,000,000 * 1.01^(3/12)
  expect(newest).toBeGreaterThanOrEqual(100_240_000);
  expect(newest).toBeLessThanOrEqual(100_260_000);

  const again = await api.POST("/api/crons/{id}/run", { params: { path: { id: cron!.id }, query: { to: "2026-04-01" } } });
  expect(again.data?.applied).toBe(0);

  const runs = await api.GET("/api/crons/{id}/runs", { params: { path: { id: cron!.id } } });
  expect(runs.data?.length).toBe(3);
  const lastRun = runs.data![0].id;
  const undo = await api.POST("/api/crons/runs/{run_id}/undo", { params: { path: { run_id: lastRun } } });
  expect(undo.response.status).toBe(204);
  const runsAfter = await api.GET("/api/crons/{id}/runs", { params: { path: { id: cron!.id } } });
  expect(runsAfter.data?.length).toBe(2);
  const rerun = await api.POST("/api/crons/{id}/run", { params: { path: { id: cron!.id }, query: { to: "2026-04-01" } } });
  expect(rerun.data?.applied).toBe(1);
});

test("a fixed-transaction cron posts monthly", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { data: cron } = await mkCron(api, {
    name: "Netflix",
    account_id: acc.id,
    kind: "fixed_transaction",
    amount_minor: -1999,
    start_date: "2026-01-15",
    day_of_month: 15,
  });

  const run = await api.POST("/api/crons/{id}/run", { params: { path: { id: cron!.id }, query: { to: "2026-03-15" } } });
  expect(run.data?.applied).toBe(3);

  const txns = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
  expect(txns.data?.length).toBe(3);
  expect(txns.data?.every((t) => t.amount_minor === -1999)).toBe(true);
  expect(txns.data?.every((t) => t.description === "Netflix")).toBe(true);
});

test("valuation crons require a rate", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");
  const { response } = await mkCron(api, { name: "bad", account_id: house.id, kind: "appreciation", start_date: "2026-01-01" });
  expect(response.status).toBe(422);
});
