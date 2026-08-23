import { test, expect } from "../fixtures";
import type { Schemas } from "../../client/src/index";
import { createAccount, createCategory, createPerson, createTransaction } from "../helpers";

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
  // The unified shape: identity and timing on the event, what it does in its effects. A dated,
  // exact change is this model with probability 100% and no spread — which is all it ever was.
  const created = await api.POST("/api/forecast/events", {
    body: {
      label: "Promotion",
      kind: "adjustment",
      expected_on: "2027-01-01",
      effects: [
        {
          kind: "set_baseline",
          target: { kind: "category", category_id: cat.id },
          amount_minor: 750_000,
        },
      ],
    },
  });
  expect(created.response.status, JSON.stringify(created.error)).toBe(201);
  expect(created.data!.probability_bps).toBe(10_000);
  expect(created.data!.timing_spread_months).toBe(0);
  expect(created.data!.effects).toHaveLength(1);

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
      label: "Promotion",
      kind: "promotion",
      expected_on: new Date().toISOString().slice(0, 10),
      effects: [
        {
          kind: "set_baseline",
          target: { kind: "category", category_id: salary.id },
          amount_minor: 1_000_000, // double the ~$5,000/mo baseline
        },
      ],
    },
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  for (let i = 0; i < 3; i++) {
    expect(after.data!.months[i].net_worth.median_minor).toBeGreaterThan(
      before.data!.months[i].net_worth.median_minor
    );
  }
});

// ---- long horizon ------------------------------------------------------------------
// The Rust unit tests own the decay arithmetic (that a fitted rate is untouched inside the
// window it was fitted over, and what it compounds to beyond it). These cover the parts only
// the wire can answer: that the ceiling moved, that the path budget is reported rather than
// applied silently, and that raising the ceiling did not reintroduce the non-finite band that
// used to take this endpoint down permanently.

test("a thirty-year horizon returns thirty years of months", async ({ api }) => {
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 360, simulations: 100, seed: 7 } },
  });
  expect(data!.months).toHaveLength(360);
  expect(data!.horizon_months).toBe(360);
});

test("a horizon past the ceiling is clamped and says so, rather than being refused", async ({
  api,
}) => {
  const { data, response } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 5_000, simulations: 100, seed: 7 } },
  });
  expect(response.status).toBe(200);
  expect(data!.horizon_months).toBe(360);
  expect(data!.months).toHaveLength(360);
});

test("a long horizon trades paths for months, and reports the count it actually ran", async ({
  api,
}) => {
  // Asking for the maximum path count over the maximum horizon exceeds the path-month budget,
  // so the run is cut back. Without `simulations` on the response there was no way to tell.
  const long = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 360, simulations: 5_000, seed: 7 } },
  });
  expect(long.data!.simulations).toBe(2_000);

  // At any horizon that was legal before the ceiling moved, the budget cannot bind — otherwise
  // raising it would have silently coarsened every projection anyone had already looked at.
  const short = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 60, simulations: 5_000, seed: 7 } },
  });
  expect(short.data!.simulations).toBe(5_000);
});

test("thirty years of compounding still produces finite bands, ordered p10 <= median <= p90", async ({
  api,
}) => {
  // The guard this pins: `exp()` saturates past ±745, and a saturated draw makes `0.0 * inf`
  // = NaN, which used to reach the percentile sort and 500 the endpoint until the offending
  // row was deleted. Six times the horizon is six times the exponent, so the arithmetic that
  // was comfortably safe at five years had to be re-checked at thirty.
  const shares = await createAccount(api, "Volatile fund", "shares_nz");
  const today = new Date();
  for (let i = 24; i >= 0; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: shares.id } },
      body: {
        as_of: d.toISOString().slice(0, 10),
        // Alternating hard up/down, so the fitted volatility is as large as a real series can
        // make it rather than as large as an override could assert.
        value_minor: i % 2 === 0 ? 20_000_00 : 60_000_00,
      },
    });
  }

  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 360, simulations: 500, seed: 3 } },
  });
  for (const m of data!.months) {
    for (const band of [m.net_worth, m.assets, m.liabilities]) {
      for (const v of Object.values(band)) {
        expect(Number.isFinite(v)).toBe(true);
      }
      expect(band.p10_minor).toBeLessThanOrEqual(band.median_minor);
      expect(band.median_minor).toBeLessThanOrEqual(band.p90_minor);
    }
  }
});

test("the share of paths that go cash-negative is reported per month", async ({ api }) => {
  // A band around net worth cannot answer "could we actually afford this": a path that ends
  // rich having been thousands overdrawn on the way looks identical to one that never was.
  // Enough to cover the 13 months of history below and leave ~10 months of runway on top, so
  // the rate starts at zero and climbs — which is the shape that makes this figure worth
  // reporting. Without the opening balance the pool is already under water in month 1 and the
  // series is a flat 10 000, which proves only that the field exists.
  const bank = await createAccount(api, "Everyday", "bank", "NZD", {
    opening_balance_minor: 69_000_00,
    opening_balance_date: "2024-01-01",
  });
  const rent = await createCategory(api, "Rent", "expense");
  const today = new Date();
  for (let i = 13; i >= 1; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: -3_000_00,
      description: "Rent",
      category_id: rent.id,
    });
  }

  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 60, simulations: 300, seed: 5 } },
  });
  expect(data!.negative_cash_rate_bps).toHaveLength(data!.months.length);
  for (const bps of data!.negative_cash_rate_bps) {
    expect(bps).toBeGreaterThanOrEqual(0);
    expect(bps).toBeLessThanOrEqual(10_000);
  }
  // Solvent at the start…
  expect(data!.negative_cash_rate_bps[0]).toBe(0);
  // …and rent with no income drains it, so by five years out every path is under water.
  expect(data!.negative_cash_rate_bps.at(-1)).toBeGreaterThan(9_000);
});

test("an account assumption names its owner; a category assumption names nobody", async ({
  api,
}) => {
  // Two people's student loans are both called "Student loan", so the label alone cannot tell
  // the assumptions list which rate belongs to whom. A category is the household's, not
  // anyone's, and says so by carrying no owner at all.
  const person = await createPerson(api, "Alex Doe");
  const shares = await createAccount(api, "Shares", "shares_nz", "NZD", {
    ownership: { kind: "person", person_id: person.id },
  });
  const joint = await createAccount(api, "Family home", "real_estate", "NZD", {
    ownership: { kind: "joint" },
  });
  const groceries = await createCategory(api, "Groceries", "expense");

  const { data } = await api.GET("/api/forecast/assumptions", {});
  expect(findAssumption(data!, "account", shares.id)?.ownership).toEqual({
    kind: "person",
    person_id: person.id,
  });
  expect(findAssumption(data!, "account", joint.id)?.ownership).toEqual({ kind: "joint" });
  expect(findAssumption(data!, "category", groceries.id)?.ownership).toBeNull();
});

// ---- GET /api/forecast/stream ------------------------------------------------------------
//
// Driven with raw `fetch` rather than the typed client: `openapi-fetch` resolves a whole body,
// so it has nowhere to put an answer that arrives in pieces. The payload *type* is still the
// generated one, which is what keeps this honest about the wire shape.

interface SseEvent {
  event: string;
  data: string;
}

/** Read an SSE response to completion, returning every frame in order. */
async function readEvents(res: Response): Promise<SseEvent[]> {
  expect(res.status).toBe(200);
  const reader = res.body!.getReader();
  const decoder = new TextDecoder();
  const out: SseEvent[] = [];
  let buffered = "";
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    buffered += decoder.decode(value, { stream: true });
    let at: number;
    while ((at = buffered.indexOf("\n\n")) !== -1) {
      const frame = buffered.slice(0, at);
      buffered = buffered.slice(at + 2);
      let event = "message";
      const data: string[] = [];
      for (const line of frame.split("\n")) {
        // A leading colon is a comment, which is what axum's keep-alive sends.
        if (line === "" || line.startsWith(":")) continue;
        const colon = line.indexOf(":");
        const field = colon === -1 ? line : line.slice(0, colon);
        let v = colon === -1 ? "" : line.slice(colon + 1);
        if (v.startsWith(" ")) v = v.slice(1);
        if (field === "event") event = v;
        else if (field === "data") data.push(v);
      }
      if (data.length) out.push({ event, data: data.join("\n") });
    }
  }
  return out;
}

test("the forecast stream delivers the projection in stages and ends with the whole thing", async ({
  api,
  server,
}) => {
  // Something with a shape to project, so the bands are not all zero.
  const acct = await createAccount(api, "Shares", "shares_nz", "NZD");
  for (let i = 24; i >= 0; i--) {
    const d = new Date();
    d.setMonth(d.getMonth() - i);
    await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: acct.id } },
      body: { as_of: d.toISOString().slice(0, 10), value_minor: 100_000_00 + (24 - i) * 1_000_00 },
    });
  }

  const query = "horizon_months=6&simulations=300&seed=1";
  const res = await fetch(`${server.baseURL}/api/forecast/stream?${query}`, {
    headers: { Accept: "text/event-stream" },
  });
  expect(res.headers.get("content-type")).toContain("text/event-stream");
  // A stream is not a representation of a resource: there is nothing for a cache to hold, and
  // this API has no authentication, so a shared cache must never keep it. `Sse::into_response`
  // sets `no-cache` on its own, which is weaker — this asserts the policy table won.
  expect(res.headers.get("cache-control")).toBe("no-store");

  const events = await readEvents(res);
  expect(events.at(-1)?.event).toBe("done");
  expect(events.some((e) => e.event === "error")).toBe(false);

  const snapshots = events
    .filter((e) => e.event === "snapshot")
    .map((e) => JSON.parse(e.data) as Schemas["ForecastProgress"]);
  const ticks = events
    .filter((e) => e.event === "tick")
    .map((e) => JSON.parse(e.data) as Schemas["ForecastProgress"]);

  // 300 paths, so: 10, 100, then the end. The point is that the first useful picture arrives
  // long before the last path does.
  expect(snapshots.map((s) => s.completed)).toEqual([10, 100, 300]);
  for (const s of snapshots) {
    expect(s.total).toBe(300);
    // A snapshot reports the paths it was taken over, not the run's eventual total, so a
    // caller reading a partial answer can tell how partial it is.
    expect(s.result!.simulations).toBe(s.completed);
    expect(s.result!.months.length).toBe(6);
  }
  // Ticks carry counters and no projection — that is the whole reason they are cheap.
  expect(ticks.length).toBeGreaterThan(0);
  for (const t of ticks) expect(t.result ?? null).toBeNull();
});

test("the stream's final snapshot is exactly what the JSON route returns", async ({
  api,
  server,
}) => {
  // The acceptance criterion for having two transports. If a future change reorders the RNG,
  // or a snapshot's aggregation drifts from the one-shot one, this is what fails.
  const acct = await createAccount(api, "Shares", "shares_nz", "NZD");
  for (let i = 24; i >= 0; i--) {
    const d = new Date();
    d.setMonth(d.getMonth() - i);
    await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: acct.id } },
      body: { as_of: d.toISOString().slice(0, 10), value_minor: 250_000_00 + (24 - i) * 2_000_00 },
    });
  }
  await createCategory(api, "Groceries", "expense");

  const q = { horizon_months: 12, simulations: 500, seed: 7 } as const;
  const { data: json } = await api.GET("/api/forecast", { params: { query: q } });
  const res = await fetch(
    `${server.baseURL}/api/forecast/stream?horizon_months=12&simulations=500&seed=7`,
    { headers: { Accept: "text/event-stream" } }
  );
  const events = await readEvents(res);
  const final = [...events]
    .reverse()
    .find((e) => e.event === "snapshot")!;
  const streamed = (JSON.parse(final.data) as Schemas["ForecastProgress"]).result!;

  // Guards the comparison against passing on two empty projections.
  expect(json!.months.length).toBe(12);
  expect(json!.simulations).toBe(500);
  expect(streamed).toEqual(json);
});

test("a bad query is refused before anything streams", async ({ server }) => {
  // The head is produced after `simulate_inputs` and before the first path, so a refusal is an
  // ordinary JSON error response — not an `error` event a client would have to parse the stream
  // to find.
  const res = await fetch(`${server.baseURL}/api/forecast/stream?currency=ZZZ`, {
    headers: { Accept: "text/event-stream" },
  });
  expect(res.status).toBe(400);
  expect(res.headers.get("content-type")).toContain("application/json");
  const body = (await res.json()) as { error: { code: string; message: string } };
  expect(body.error.code).toBeTruthy();
});

test("a seeded forecast reproduces across requests", async ({ api }) => {
  // It did not, until the category order was made deterministic: `Categories` kept its tree in
  // a `HashMap`, and that order *is* the order `category_sims` draws its randomness in, so two
  // identically-seeded requests differed in every band. Three categories, because one cannot
  // be permuted.
  await createCategory(api, "Groceries", "expense");
  await createCategory(api, "Transport", "expense");
  await createCategory(api, "Salary", "income");
  const acct = await createAccount(api, "Bank", "bank", "NZD");
  for (let i = 24; i >= 0; i--) {
    const d = new Date();
    d.setMonth(d.getMonth() - i);
    await createTransaction(api, {
      account_id: acct.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: 4_000_00,
      description: "Pay",
    });
  }

  const q = { horizon_months: 12, simulations: 400, seed: 99 } as const;
  const runs = [];
  for (let i = 0; i < 3; i++) {
    const { data } = await api.GET("/api/forecast", { params: { query: q } });
    runs.push(JSON.stringify(data));
  }
  expect(runs[1]).toBe(runs[0]);
  expect(runs[2]).toBe(runs[0]);

  // …and the assumptions come back in a stable order, which is also what lets the ETag on
  // `/api/forecast/assumptions` ever match.
  const orders = [];
  for (let i = 0; i < 3; i++) {
    const { data } = await api.GET("/api/forecast/assumptions", {});
    orders.push(data!.map((a) => `${a.target_type}:${a.target_id}`).join(","));
  }
  expect(orders[1]).toBe(orders[0]);
  expect(orders[2]).toBe(orders[0]);
});
