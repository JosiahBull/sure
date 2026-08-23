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
  // Exercising converts options into shares; it does not dispose of them. The 500 units are
  // now worth the full $5 mark rather than the $4 spread, so the grant's total goes *up* by
  // the strike now sunk into them — it must never fall by their market value.
  expect(after.owned).toBe(500);
  expect(after.owned_value_minor).toBe(500 * 500);
  expect(after.intrinsic_value_minor).toBe(1200 * 400);
  expect(after.total_value_minor).toBe(1200 * 400 + 500 * 500);
  expect(after.total_value_minor).toBeGreaterThan(v.total_value_minor);

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
  // Nothing exercised on either grant, so the whole position is still options.
  expect(equity.data?.total_owned_minor).toBe(0);
  expect(equity.data?.total_value_minor).toBe(2_400_000);

  const revalue = await api.POST("/api/accounts/{id}/equity/revalue", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-01" } },
  });
  expect(revalue.response.status).toBe(200);
  const vals = await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: acc.id } } });
  expect(vals.data![0].value_minor).toBe(2_400_000);
  expect(vals.data![0].source).toBe("equity");
});

test("a past date is priced at the mark that applied then", async ({ api }) => {
  const acc = await sharesAccount(api);
  // No unit value on the grant: the mark ledger is the only price.
  const g = (
    await api.POST("/api/accounts/{id}/equity-grants", {
      params: { path: { id: acc.id } },
      body: { company: "Startco", grant_date: "2024-01-01", quantity: 4800, strike_minor: 100 },
    })
  ).data!;
  const mark = (asOf: string, unit: number) =>
    api.POST("/api/accounts/{id}/equity-marks", {
      params: { path: { id: acc.id } },
      body: { as_of: asOf, unit_value_minor: unit },
    });
  expect((await mark("2024-01-01", 200)).response.status).toBe(201);
  await mark("2026-01-01", 500);

  // 2025 is still on the old mark even though a newer one exists — the whole point of a price
  // ledger. Before it, valuing a past date used whatever the grant currently said.
  const then = (await vesting(api, g.id, "2025-01-01")).data!;
  expect(then.vested).toBe(1200);
  expect(then.unit_value_minor).toBe(200);
  expect(then.intrinsic_value_minor).toBe(1200 * 100);

  const now = (await vesting(api, g.id, "2026-01-01")).data!;
  expect(now.unit_value_minor).toBe(500);
  expect(now.intrinsic_value_minor).toBe(2400 * 400);

  // Correcting a figure replaces the mark on that date rather than stacking a second one.
  await mark("2024-01-01", 250);
  const marks = (
    await api.GET("/api/accounts/{id}/equity-marks", { params: { path: { id: acc.id } } })
  ).data!;
  expect(marks.length).toBe(2);
});

test("rebuilding writes the whole history and is safe to re-run", async ({ api }) => {
  const acc = await sharesAccount(api);
  await grant(api, acc.id, "Startco");

  const first = await api.POST("/api/accounts/{id}/equity/rebuild", {
    params: { path: { id: acc.id }, query: { as_of: "2026-01-01" } },
  });
  expect(first.response.status).toBe(200);
  expect(first.data!.written).toBeGreaterThan(1);
  expect(first.data!.from).toBe("2024-01-01");
  expect(first.data!.to).toBe("2026-01-01");

  const count = async () =>
    (
      await api.GET("/api/accounts/{id}/valuations", {
        params: { path: { id: acc.id }, query: { source: "equity", limit: 500 } },
      })
    ).data!.length;
  const after = await count();
  expect(after).toBe(first.data!.written);

  // Idempotent per date — what makes correcting a mark and rebuilding safe.
  await api.POST("/api/accounts/{id}/equity/rebuild", {
    params: { path: { id: acc.id }, query: { as_of: "2026-01-01" } },
  });
  expect(await count()).toBe(after);
});

test("the activity ledger reports vesting nobody entered", async ({ api }) => {
  const acc = await sharesAccount(api);
  const g = (await grant(api, acc.id, "Startco")).data!;
  await api.POST("/api/equity-grants/{id}/exercises", {
    params: { path: { id: g.id } },
    body: { exercise_date: "2025-06-01", quantity: 100, price_minor: 100 },
  });

  const events = (
    await api.GET("/api/accounts/{id}/equity-events", {
      params: { path: { id: acc.id }, query: { as_of: "2025-06-01" } },
    })
  ).data!;
  // The 12-month cliff is its own kind: one date where a year of tranches lands at once.
  const cliff = events.filter((e) => e.kind === "cliff");
  expect(cliff.length).toBe(1);
  expect(cliff[0].date).toBe("2025-01-01");
  expect(cliff[0].quantity).toBe(1200);
  // Tranches must total what the status reports, or ledger and summary disagree.
  const vested = events
    .filter((e) => e.kind !== "exercise")
    .reduce((sum, e) => sum + e.quantity, 0);
  expect(vested).toBe((await vesting(api, g.id, "2025-06-01")).data!.vested);
  expect(events.filter((e) => e.kind === "exercise").length).toBe(1);
});

test("a revaluation counts shares already exercised, not just the options left", async ({ api }) => {
  const acc = await sharesAccount(api);
  const g = (await grant(api, acc.id, "Startco")).data!;
  // 30 months in: 3,000 of 4,800 vested. Exercise all of it, leaving no options at all.
  await api.POST("/api/equity-grants/{id}/exercises", {
    params: { path: { id: g.id } },
    body: { exercise_date: "2026-07-01", quantity: 3000, price_minor: 100 },
  });

  const equity = (
    await api.GET("/api/accounts/{id}/equity", {
      params: { path: { id: acc.id }, query: { as_of: "2026-07-01" } },
    })
  ).data!;
  expect(equity.total_intrinsic_minor).toBe(0);
  expect(equity.total_owned_minor).toBe(3000 * 500);
  expect(equity.total_value_minor).toBe(3000 * 500);

  await api.POST("/api/accounts/{id}/equity/revalue", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-01" } },
  });
  const vals = await api.GET("/api/accounts/{id}/valuations", { params: { path: { id: acc.id } } });
  // The whole position. While only the intrinsic figure was persisted, a fully-exercised
  // account like this one revalued to zero and vanished from net worth.
  expect(vals.data![0].value_minor).toBe(3000 * 500);
});
