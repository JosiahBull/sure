// Probabilistic forecast events: the wire contract, and the guarantees the chart is drawn from.
//
// The sampling arithmetic — uniform windows, RNG independence, relation clamping — is unit-tested in
// Rust. These cover what only the whole stack can show: that a certainty still behaves exactly as it
// did before probability existed, that uncertainty widens the band, and that the timing the chart is
// fed is the *realised* one rather than what was typed.
//
// All names and figures are invented (CLAUDE.md rule 3).
import { test, expect } from "../fixtures";
import type { Schemas } from "../../client/src/index";
import {
  createAccount,
  createCategory,
  createIncomeStream,
  createPerson,
  createTransaction,
} from "../helpers";

const params = { horizon_months: 60, simulations: 400, seed: 5 };

async function addEvent(
  api: Parameters<typeof createPerson>[0],
  over: Partial<Schemas["SaveForecastEvent"]> = {}
) {
  const body: Schemas["SaveForecastEvent"] = {
    label: "Something",
    kind: "custom",
    expected_on: monthsOut(12),
    ...over,
  };
  const { data, response, error } = await api.POST("/api/forecast/events", { body });
  expect(response.status, JSON.stringify(error)).toBe(201);
  return data!;
}

function monthsOut(n: number): string {
  const d = new Date();
  d.setMonth(d.getMonth() + n, 1);
  return d.toISOString().slice(0, 10);
}

/** A category with a real fitted baseline, so an event has something to act on. */
async function seedSpending(api: Parameters<typeof createPerson>[0]) {
  const bank = await createAccount(api, "Everyday", "bank", "NZD", {
    opening_balance_minor: 200_000_00,
    opening_balance_date: "2024-01-01",
  });
  const cat = await createCategory(api, "Living", "expense");
  const today = new Date();
  for (let i = 12; i >= 1; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: -2_000_00,
      description: "Living",
      category_id: cat.id,
    });
  }
  return cat;
}

test("an event round-trips with its effects and timing rules", async ({ api }) => {
  const cat = await seedSpending(api);
  const first = await addEvent(api, { label: "Move house", kind: "custom" });
  const child = await addEvent(api, {
    label: "First child",
    kind: "child",
    probability_bps: 8_000,
    timing_spread_months: 24,
    effects: [
      {
        kind: "recurring_delta",
        category_id: cat.id,
        amount_minor: 1_200_00,
        delay_months: 12,
        ramp_months: 3,
        duration_months: 48,
      },
    ],
    relations: [{ depends_on_event_id: first.id, kind: "after", min_gap_months: 6 }],
  });

  expect(child.probability_bps).toBe(8_000);
  expect(child.timing_spread_months).toBe(24);
  expect(child.effects).toHaveLength(1);
  expect(child.relations).toHaveLength(1);
  expect(child.relations[0].min_gap_months).toBe(6);

  const got = await api.GET("/api/forecast/events/{id}", { params: { path: { id: child.id } } });
  expect(got.data!.effects[0]).toMatchObject({ kind: "recurring_delta", ramp_months: 3 });
});

test("a certainty behaves exactly as a dated change always did", async ({ api }) => {
  const cat = await seedSpending(api);
  const before = await api.GET("/api/forecast", { params: { query: params } });

  // 100% likely, no spread — which is all a "known future change" ever was.
  await addEvent(api, {
    label: "Rates rise",
    kind: "adjustment",
    probability_bps: 10_000,
    timing_spread_months: 0,
    expected_on: monthsOut(6),
    effects: [
      {
        kind: "recurring_delta",
        category_id: cat.id,
        amount_minor: 500_00,
        delay_months: 0,
        ramp_months: 0,
        duration_months: null,
      },
    ],
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  // Certain, so every path agrees on when it lands and how much it costs.
  const o = after.data!.events[0];
  expect(o.occurrence_rate_bps).toBe(10_000);
  expect(o.month_p10).toBe(o.month_p90);
  expect(o.constrained_rate_bps).toBe(0);
  // …and it makes the household poorer from then on.
  expect(after.data!.months[59].net_worth.median_minor).toBeLessThan(
    before.data!.months[59].net_worth.median_minor
  );
});

test("a zero-probability event changes nothing at all", async ({ api }) => {
  const cat = await seedSpending(api);
  const before = await api.GET("/api/forecast", { params: { query: params } });
  await addEvent(api, {
    label: "Probably not",
    probability_bps: 0,
    effects: [
      {
        kind: "recurring_delta",
        category_id: cat.id,
        amount_minor: 5_000_00,
        delay_months: 0,
        ramp_months: 0,
        duration_months: null,
      },
    ],
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });
  expect(after.data!.events[0].occurrence_rate_bps).toBe(0);
  // Same seed, and the event draws from its own RNG stream, so the projection is untouched.
  expect(after.data!.months.map((m) => m.net_worth.median_minor)).toEqual(
    before.data!.months.map((m) => m.net_worth.median_minor)
  );
});

test("an uncertain event widens the band that a certain one leaves tight", async ({ api }) => {
  const cat = await seedSpending(api);
  const certain = await addEvent(api, {
    label: "Certain cost",
    probability_bps: 10_000,
    timing_spread_months: 0,
    expected_on: monthsOut(24),
    effects: [
      {
        kind: "recurring_delta",
        category_id: cat.id,
        amount_minor: 2_000_00,
        delay_months: 0,
        ramp_months: 0,
        duration_months: null,
      },
    ],
  });
  const tight = await api.GET("/api/forecast", { params: { query: params } });
  const tightSpread =
    tight.data!.months[59].net_worth.p90_minor - tight.data!.months[59].net_worth.p10_minor;

  // Same cost, same expected date — but a coin flip on whether it happens at all.
  await api.PUT("/api/forecast/events/{id}", {
    params: { path: { id: certain.id } },
    body: {
      label: "Uncertain cost",
      kind: "custom",
      expected_on: monthsOut(24),
      probability_bps: 5_000,
      timing_spread_months: 0,
      effects: [
        {
          kind: "recurring_delta",
          category_id: cat.id,
          amount_minor: 2_000_00,
          delay_months: 0,
          ramp_months: 0,
          duration_months: null,
        },
      ],
    },
  });
  const wide = await api.GET("/api/forecast", { params: { query: params } });
  const wideSpread =
    wide.data!.months[59].net_worth.p90_minor - wide.data!.months[59].net_worth.p10_minor;

  // This is the whole point of the feature: the band widens because the future is genuinely unsure.
  expect(wideSpread).toBeGreaterThan(tightSpread);
  const o = wide.data!.events[0];
  expect(o.occurrence_rate_bps).toBeGreaterThan(4_000);
  expect(o.occurrence_rate_bps).toBeLessThan(6_000);
});

test("the timing the chart is fed is the realised one, not what was typed", async ({ api }) => {
  const promotion = await addEvent(api, {
    label: "Promotion",
    kind: "promotion",
    expected_on: monthsOut(24),
    probability_bps: 10_000,
    timing_spread_months: 0,
  });
  // Configured for month 6 — but forced to wait until three months after the promotion.
  await addEvent(api, {
    label: "First child",
    kind: "child",
    expected_on: monthsOut(6),
    probability_bps: 10_000,
    timing_spread_months: 0,
    relations: [{ depends_on_event_id: promotion.id, kind: "after", min_gap_months: 3 }],
  });

  const { data } = await api.GET("/api/forecast", { params: { query: params } });
  const child = data!.events.find((e) => e.label === "First child")!;
  const promo = data!.events.find((e) => e.label === "Promotion")!;
  // A chart drawn from `expected_on` would put the child before the promotion. It lands after.
  expect(child.month_median!).toBeGreaterThan(promo.month_median!);
  expect(child.month_median!).toBe(promo.month_median! + 3);
  // …and it says the rule is what moved it, on every run.
  expect(child.constrained_rate_bps).toBe(10_000);
});

test("only_if stops a child event when its parent does not happen", async ({ api }) => {
  const parent = await addEvent(api, { label: "Buy a house", probability_bps: 0 });
  await addEvent(api, {
    label: "Renovate",
    relations: [{ depends_on_event_id: parent.id, kind: "only_if", min_gap_months: 0 }],
  });
  const { data } = await api.GET("/api/forecast", { params: { query: params } });
  const renovate = data!.events.find((e) => e.label === "Renovate")!;
  // Configured as a certainty, but conditional on something that never happens.
  expect(renovate.probability_bps).toBe(10_000);
  expect(renovate.occurrence_rate_bps).toBe(0);
});

test("closing a loop is refused, and the message names what waits for what", async ({ api }) => {
  const a = await addEvent(api, { label: "Alpha" });
  const b = await addEvent(api, {
    label: "Beta",
    relations: [{ depends_on_event_id: a.id, kind: "after", min_gap_months: 0 }],
  });
  const cycle = await api.PUT("/api/forecast/events/{id}", {
    params: { path: { id: a.id } },
    body: {
      label: "Alpha",
      kind: "custom",
      expected_on: monthsOut(12),
      relations: [{ depends_on_event_id: b.id, kind: "after", min_gap_months: 0 }],
    },
  });
  expect(cycle.response.status).toBe(409);
  expect(JSON.stringify(cycle.error)).toContain("waits for");
});

test("deleting an event drops ordering rules but is refused while something depends on it", async ({
  api,
}) => {
  const parent = await addEvent(api, { label: "Parent" });
  const ordered = await addEvent(api, {
    label: "Ordered after",
    relations: [{ depends_on_event_id: parent.id, kind: "after", min_gap_months: 0 }],
  });
  const conditional = await addEvent(api, {
    label: "Only if",
    relations: [{ depends_on_event_id: parent.id, kind: "only_if", min_gap_months: 0 }],
  });

  // An `only_if` would silently become certain, which is a change of meaning with no trace.
  const blocked = await api.DELETE("/api/forecast/events/{id}", {
    params: { path: { id: parent.id } },
  });
  expect(blocked.response.status).toBe(409);
  expect(JSON.stringify(blocked.error)).toContain("Only if");

  // Remove the conditional dependant, and the pure ordering edge is dropped for you.
  await api.DELETE("/api/forecast/events/{id}", { params: { path: { id: conditional.id } } });
  const ok = await api.DELETE("/api/forecast/events/{id}", {
    params: { path: { id: parent.id } },
  });
  expect(ok.response.status).toBe(204);

  const survivor = await api.GET("/api/forecast/events/{id}", {
    params: { path: { id: ordered.id } },
  });
  expect(survivor.data!.relations).toHaveLength(0);
});

test("a career break pauses every one of that person's incomes", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Main job",
    basis: "net",
    annual_amount_minor: 60_000_00,
    pay_frequency: "monthly",
    first_payment_on: monthsOut(1),
    starts_on: monthsOut(1),
  });
  await createIncomeStream(api, p.id, {
    label: "Side work",
    basis: "net",
    annual_amount_minor: 12_000_00,
    pay_frequency: "monthly",
    first_payment_on: monthsOut(1),
    starts_on: monthsOut(1),
  });
  const before = await api.GET("/api/forecast", { params: { query: params } });

  await addEvent(api, {
    label: "Parental leave",
    kind: "career_break",
    person_id: p.id,
    expected_on: monthsOut(12),
    probability_bps: 10_000,
    timing_spread_months: 0,
    // Unpaid, so the drop is unambiguous.
    effects: [{ kind: "income_pause", person_id: p.id, months: 6, replacement_rate_bps: 0 }],
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  // Month 12 is inside the break: both incomes stop, not just one.
  expect(after.data!.income_net[11].median_minor).toBe(0);
  // Before it, nothing changed; after it, pay resumes.
  expect(after.data!.income_net[5].median_minor).toBe(before.data!.income_net[5].median_minor);
  expect(after.data!.income_net[23].median_minor).toBe(before.data!.income_net[23].median_minor);
});

test("a promotion raises pay from its month on, at the marginal tax rate", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const stream = await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 90_000_00,
    pay_frequency: "monthly",
    first_payment_on: monthsOut(1),
    starts_on: monthsOut(1),
  });
  await addEvent(api, {
    label: "Promotion",
    kind: "promotion",
    person_id: p.id,
    expected_on: monthsOut(12),
    probability_bps: 10_000,
    timing_spread_months: 0,
    effects: [
      {
        kind: "income_step",
        income_stream_id: stream.id,
        amount: { basis: "percent", rate_bps: 2_000 },
      },
    ],
  });
  const { data } = await api.GET("/api/forecast", { params: { query: params } });
  const beforeRise = data!.income_net[5].median_minor;
  const afterRise = data!.income_net[23].median_minor;
  expect(afterRise).toBeGreaterThan(beforeRise);
  // A 20% gross rise keeps less than 20% more take-home, because the increment is taxed at the
  // marginal rate rather than at the average one. That distinction is why the tax engine exists.
  expect(afterRise).toBeLessThan(beforeRise * 1.2);
});

test("an event whose window runs past the horizon is flagged, not silently dropped", async ({
  api,
}) => {
  await addEvent(api, {
    label: "Some day",
    expected_on: monthsOut(58),
    probability_bps: 10_000,
    timing_spread_months: 24,
  });
  const { data } = await api.GET("/api/forecast", { params: { query: params } });
  const o = data!.events[0];
  expect(o.occurrence_rate_bps).toBe(10_000);
  // It always happens; it does not always happen *by then*, and those are different facts.
  expect(o.in_window_rate_bps).toBeLessThan(10_000);
  expect(o.truncated).toBe(true);
});

test("every reported window is ordered p10 <= median <= p90", async ({ api }) => {
  await addEvent(api, { label: "A", probability_bps: 6_000, timing_spread_months: 18 });
  await addEvent(api, { label: "B", probability_bps: 9_500, timing_spread_months: 3 });
  const { data } = await api.GET("/api/forecast", { params: { query: params } });
  for (const o of data!.events) {
    expect(o.month_p10!).toBeLessThanOrEqual(o.month_median!);
    expect(o.month_median!).toBeLessThanOrEqual(o.month_p90!);
    expect(o.occurrence_rate_bps).toBeGreaterThanOrEqual(o.in_window_rate_bps);
  }
});
