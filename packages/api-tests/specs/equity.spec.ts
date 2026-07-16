import { test, expect } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

const sharesAccount = (api: SureClient) => createAccount(api, "Startco Options", "shares_private", "USD");

function grant(api: SureClient, accountId: number, company: string) {
  return api.POST("/api/accounts/{id}/equity-grants", {
    params: { path: { id: accountId } },
    body: {
      company,
      grant_date: "2024-01-01",
      quantity: 4800,
      strike_minor: 100,
      unit_value_minor: 500,
      vest_months: 48,
      cliff_months: 12,
    },
  });
}
const vesting = (api: SureClient, id: number, asOf: string) =>
  api.GET("/api/equity-grants/{id}/vesting", { params: { path: { id }, query: { as_of: asOf } } });

test("cliff + linear vesting", async ({ api }) => {
  const acc = await sharesAccount(api);
  const g = (await grant(api, acc.id, "Startco")).data!;

  const before = (await vesting(api, g.id, "2024-06-01")).data!;
  expect(before.vested).toBe(0);
  expect(before.unvested).toBe(4800);

  const atCliff = (await vesting(api, g.id, "2025-01-01")).data!;
  expect(atCliff.vested).toBe(1200); // 12/48
  expect(atCliff.unvested).toBe(3600);
  expect(atCliff.intrinsic_value_minor).toBe(480_000); // 1200 × ($5 − $1)

  const done = (await vesting(api, g.id, "2028-06-01")).data!;
  expect(done.vested).toBe(4800);
  expect(done.unvested).toBe(0);
});

test("exercises reduce what's available and are bounded", async ({ api }) => {
  const acc = await sharesAccount(api);
  const g = (await grant(api, acc.id, "Startco")).data!;

  const v = (await vesting(api, g.id, "2025-06-01")).data!;
  expect(v.vested).toBe(1700); // 17/48
  expect(v.vested_unexercised).toBe(1700);

  const ex = await api.POST("/api/equity-grants/{id}/exercises", {
    params: { path: { id: g.id } },
    body: { exercise_date: "2025-06-01", quantity: 500, price_minor: 100 },
  });
  expect(ex.response.status).toBe(201);

  const after = (await vesting(api, g.id, "2025-06-01")).data!;
  expect(after.exercised).toBe(500);
  expect(after.vested_unexercised).toBe(1200);

  const tooMuch = await api.POST("/api/equity-grants/{id}/exercises", {
    params: { path: { id: g.id } },
    body: { exercise_date: "2025-06-01", quantity: 5000, price_minor: 0 },
  });
  expect(tooMuch.response.status).toBe(422);
});

test("account equity sums grants and revalues into net worth", async ({ api }) => {
  const acc = await sharesAccount(api);
  await grant(api, acc.id, "Startco");
  await grant(api, acc.id, "Otherco");

  const equity = await api.GET("/api/accounts/{id}/equity", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-01" } },
  });
  expect(equity.data?.grants.length).toBe(2);
  // 30 months => 3000 vested per grant, intrinsic 3000×400 = 1,200,000 each.
  expect(equity.data?.total_intrinsic_minor).toBe(2_400_000);

  const revalue = await api.POST("/api/accounts/{id}/equity/revalue", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-01" } },
  });
  expect(revalue.response.status).toBe(200);
  const vals = await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: acc.id } } });
  expect(vals.data![0].value_minor).toBe(2_400_000);
  expect(vals.data![0].source).toBe("equity");
});
