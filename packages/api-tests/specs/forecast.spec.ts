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
      lender: "ASB",
      original_amount_minor: 500_000_00,
      interest_rate_bps: 549,
      rate_type: "floating",
      term_months: 360,
      start_date: "2024-01-01",
    },
  });

  const { data } = await api.GET("/api/forecast/assumptions", {});
  const a = findAssumption(data!, "account", mortgage.id);
  expect(a?.source).toBe("deterministic");
  // …and it shows its working, rather than an unexplained "deterministic".
  expect(a?.currency_code).toBe("NZD");
  expect(a?.schedule?.current_rate_bps).toBe(549);
  expect(a?.schedule?.monthly_payment_minor).toBeGreaterThan(0);
  expect(a?.schedule?.remaining_term_months).toBeLessThan(360);
  // Floating: no roll-off to model.
  expect(a?.schedule?.refix_in_months ?? null).toBeNull();
});

test("a fixed-rate mortgage's refix uncertainty widens the projection only after it rolls off", async ({
  api,
}) => {
  // Fixed for another three months, then refixed at 5.12% ± 1.5% (one standard deviation).
  // Until the roll-off the balance is genuinely certain and every simulated path must agree;
  // after it, the drawn rate is what gives the forecast an honest band.
  const fixedUntil = new Date();
  fixedUntil.setMonth(fixedUntil.getMonth() + 3);

  const mortgage = await createAccount(api, "Prime Housing Lending", "mortgage", "NZD", {
    metadata: {
      profile: "mortgage",
      lender: "ASB",
      original_amount_minor: 485_000_00,
      interest_rate_bps: 512,
      rate_type: "fixed",
      fixed_until: fixedUntil.toISOString().slice(0, 10),
      refix_rate_bps: 512,
      refix_rate_uncertainty_bps: 150,
      term_months: 324,
      start_date: "2025-12-11",
    },
  });
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: mortgage.id } },
    body: { as_of: new Date().toISOString().slice(0, 10), value_minor: -478_940_17 },
  });

  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 24, simulations: 1000, seed: 11 } },
  });

  const first = data!.months[0].liabilities;
  expect(first.p90_minor - first.p10_minor).toBe(0);
  const last = data!.months[23].liabilities;
  expect(last.p90_minor - last.p10_minor).toBeGreaterThan(0);

  const a = findAssumption(data!.assumptions, "account", mortgage.id);
  expect(a?.schedule?.refix_in_months).toBe(3);
  expect(a?.schedule?.refix_rate_uncertainty_bps).toBe(150);
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

// The permanent-500 case. Volatility is the standard deviation of a lognormal monthly draw, so
// it sets the exponent the simulation raises `e` to; `exp()` saturates past ±745, and a σ in the
// millions of bps makes both an underflow to 0.0 and an overflow to inf routine inside one
// path. 0.0 * inf is NaN, which the percentile sort used to panic on — a 500 on GET /api/forecast
// that could only be cleared through a control on the page the 500 broke. Refused on the way in,
// and the projection stays healthy either way.
test("an out-of-range volatility override is refused, and GET /api/forecast keeps working", async ({ api }) => {
  const house = await createAccount(api, "House", "real_estate");
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: house.id } },
    body: { as_of: "2026-01-01", value_minor: 770_000_00 },
  });

  const absurd = await api.PUT("/api/forecast/assumptions", {
    body: { target_type: "account", target_id: house.id, annual_volatility_bps: 1_000_000_000_000_00 },
  });
  expect(absurd.response.status).toBe(422);
  expect(JSON.stringify(absurd.error)).toContain("annual_volatility_bps");

  const negative = await api.PUT("/api/forecast/assumptions", {
    body: { target_type: "account", target_id: house.id, annual_volatility_bps: -1 },
  });
  expect(negative.response.status).toBe(422);

  // Nothing was stored, so the account is still on its derived default…
  const assumptions = await api.GET("/api/forecast/assumptions", {});
  expect(findAssumption(assumptions.data!, "account", house.id)?.source).toBe("insufficient_history");

  // …and the endpoint the bad value would have taken down still answers.
  const forecast = await api.GET("/api/forecast", { params: { query: { horizon_months: 6, simulations: 200, seed: 42 } } });
  expect(forecast.response.status).toBe(200);
  for (const m of forecast.data!.months) {
    expect(m.net_worth.p10_minor).toBeLessThanOrEqual(m.net_worth.median_minor);
    expect(m.net_worth.median_minor).toBeLessThanOrEqual(m.net_worth.p90_minor);
  }

  // A usable volatility — up to and including the 300%/yr a genuinely lumpy series measures —
  // is still accepted.
  for (const annual_volatility_bps of [0, 2_000, 30_000]) {
    const ok = await api.PUT("/api/forecast/assumptions", {
      body: { target_type: "account", target_id: house.id, annual_volatility_bps },
    });
    expect(ok.response.status, `${annual_volatility_bps}`).toBe(200);
    expect(ok.data?.annual_volatility_bps).toBe(annual_volatility_bps);
  }
});

// Same door, same answer as the reports: a projection denominated in a currency with no
// `currencies` row has no scale and no rate, so every account falls out of it. That was a 200
// describing nothing; it is a 400 naming the code.
test("an unknown ?currency= on the forecast is a 400", async ({ api }) => {
  await createAccount(api, "Everyday", "bank");

  const bad = await api.GET("/api/forecast", { params: { query: { currency: "ZZZ", horizon_months: 3, simulations: 100 } } });
  expect(bad.response.status).toBe(400);
  expect(JSON.stringify(bad.error)).toContain("ZZZ");

  const good = await api.GET("/api/forecast", { params: { query: { currency: "NZD", horizon_months: 3, simulations: 100 } } });
  expect(good.response.status).toBe(200);
  expect(good.data?.currency).toBe("NZD");
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
