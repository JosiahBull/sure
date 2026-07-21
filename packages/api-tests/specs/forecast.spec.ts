import { test, expect } from "../fixtures";
import type { Schemas } from "../../client/src/index";
import { createAccount, createCategory, createTransaction } from "../helpers";

function findAssumption(
  assumptions: Schemas["ResolvedAssumption"][],
  targetType: Schemas["ForecastTargetType"],
  targetId: number
) {
  return assumptions.find((a) => a.target_type === targetType && a.target_id === targetId);
}

test("an account with a single valuation is flagged insufficient history, not a confident 0%", async ({
  api,
}) => {
  const house = await createAccount(api, "House", "real_estate");
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: house.id } },
    body: { as_of: "2026-01-01", value_minor: 770_000_00 },
  });

  const { data } = await api.GET("/api/forecast/assumptions", {});
  const a = findAssumption(data!, "account", house.id);
  expect(a?.source).toBe("insufficient_history");
  expect(a?.annual_growth_bps).toBe(0);
});

test("an account with a real valuation trend derives a growth rate", async ({ api }) => {
  const shares = await createAccount(api, "Shares", "shares_nz");
  const monthly = Math.pow(1.12, 1 / 12);
  let value = 50_000_00;
  // Anchored to *today* (the server uses the real wall clock, not a fixed test clock) —
  // the last valuation must land on the most recent month, or the resampled series
  // flat-carries from an older date to today and dilutes the derived rate downward.
  const today = new Date();
  for (let i = 14; i >= 0; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: shares.id } },
      body: { as_of: d.toISOString().slice(0, 10), value_minor: Math.round(value) },
    });
    value *= monthly;
  }

  const { data } = await api.GET("/api/forecast/assumptions", {});
  const a = findAssumption(data!, "account", shares.id);
  expect(a?.source).toBe("derived");
  expect(a!.annual_growth_bps).toBeGreaterThan(1000); // ~12%/yr, allow noise
  expect(a!.annual_growth_bps).toBeLessThan(1400);
});

test("a mortgage with complete amortisation metadata is deterministic, not a derived rate", async ({
  api,
}) => {
  const mortgage = await createAccount(api, "Home Loan", "mortgage", "NZD", {
    metadata: {
      profile: "mortgage",
      original_amount_minor: 500_000_00,
      interest_rate_bps: 549,
      term_months: 360,
      start_date: "2024-01-01",
    },
  });

  const { data } = await api.GET("/api/forecast/assumptions", {});
  const a = findAssumption(data!, "account", mortgage.id);
  expect(a?.source).toBe("deterministic");
});

test("an assumption override round-trips and clears back to the derived default", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");

  const put = await api.PUT("/api/forecast/assumptions", {
    body: {
      target_type: "account",
      target_id: house.id,
      annual_growth_bps: 500,
      annual_volatility_bps: 200,
    },
  });
  expect(put.response.status).toBe(200);

  const afterSet = await api.GET("/api/forecast/assumptions", {});
  const a = findAssumption(afterSet.data!, "account", house.id);
  expect(a?.source).toBe("override");
  expect(a?.annual_growth_bps).toBe(500);
  expect(a?.annual_volatility_bps).toBe(200);

  const del = await api.DELETE("/api/forecast/assumptions/{target_type}/{target_id}", {
    params: { path: { target_type: "account", target_id: house.id } },
  });
  expect(del.response.status).toBe(204);

  const afterClear = await api.GET("/api/forecast/assumptions", {});
  const cleared = findAssumption(afterClear.data!, "account", house.id);
  expect(cleared?.source).toBe("insufficient_history");
});

test("forecast events CRUD, and deleting a nonexistent one 404s", async ({ api }) => {
  const cat = await createCategory(api, "Salary", "income");
  const created = await api.POST("/api/forecast/events", {
    body: {
      target_type: "category",
      target_id: cat.id,
      kind: "step_change",
      effective_date: "2027-01-01",
      amount_minor: 750_000,
      label: "Promotion",
    },
  });
  expect(created.response.status).toBe(201);

  const list = await api.GET("/api/forecast/events", {});
  expect(list.data?.some((e) => e.id === created.data!.id)).toBe(true);

  const del = await api.DELETE("/api/forecast/events/{id}", {
    params: { path: { id: created.data!.id } },
  });
  expect(del.response.status).toBe(204);

  const missing = await api.DELETE("/api/forecast/events/{id}", {
    params: { path: { id: created.data!.id } },
  });
  expect(missing.response.status).toBe(404);
});

test("GET /api/forecast returns ordered percentile bands for every requested month", async ({ api }) => {
  const shares = await createAccount(api, "Shares", "shares_nz");
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: shares.id } },
    body: { as_of: "2025-01-01", value_minor: 100_000_00 },
  });
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: shares.id } },
    body: { as_of: "2025-08-01", value_minor: 120_000_00 },
  });

  const { data, response } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 6, simulations: 300, seed: 1 } },
  });
  expect(response.status).toBe(200);
  expect(data?.months.length).toBe(6);
  for (const m of data!.months) {
    expect(m.net_worth.p10_minor).toBeLessThanOrEqual(m.net_worth.median_minor);
    expect(m.net_worth.median_minor).toBeLessThanOrEqual(m.net_worth.p90_minor);
    expect(m.net_worth.p25_minor).toBeLessThanOrEqual(m.net_worth.p75_minor);
  }
});

test("a promotion step-change event raises the projected net worth from its month on", async ({ api }) => {
  const bank = await createAccount(api, "Everyday", "bank");
  const salary = await createCategory(api, "Salary", "income");
  for (let i = 0; i < 12; i++) {
    const d = new Date(2025, i, 15);
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: 500_000,
      category_id: salary.id,
    });
  }

  const params = { horizon_months: 3, simulations: 500, seed: 42 };
  const before = await api.GET("/api/forecast", { params: { query: params } });

  await api.POST("/api/forecast/events", {
    body: {
      target_type: "category",
      target_id: salary.id,
      kind: "step_change",
      effective_date: new Date().toISOString().slice(0, 10),
      amount_minor: 1_000_000, // double the ~$5,000/mo baseline
      label: "Promotion",
    },
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  for (let i = 0; i < 3; i++) {
    expect(after.data!.months[i].net_worth.median_minor).toBeGreaterThan(
      before.data!.months[i].net_worth.median_minor
    );
  }
});
