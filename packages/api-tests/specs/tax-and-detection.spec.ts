// Editable tax rates, fund fees, and reading a salary out of the ledger.
//
// The arithmetic is unit-tested in Rust. These cover what only the whole stack shows: that editing a
// rate actually changes a projection, that a fee actually drags on one, and that the detector tells
// twice-a-month apart from every-fourteen-days using real transactions.
//
// Invented figures and employers throughout (CLAUDE.md rule 3).
import { test, expect } from "../fixtures";
import { createAccount, createCategory, createIncomeStream, createPerson, createTransaction } from "../helpers";

const params = { horizon_months: 24, simulations: 200, seed: 6 };

function firstOfNextMonth(): string {
  const d = new Date();
  d.setMonth(d.getMonth() + 1, 1);
  return d.toISOString().slice(0, 10);
}

test("the built-in tax rates are seeded on first run, with their sources recorded", async ({
  api,
}) => {
  const { data } = await api.GET("/api/tax-scales", {});
  expect(data!.length).toBeGreaterThanOrEqual(2);
  // The top band has to be open-ended, or income above it would be untaxed.
  expect(data![0].brackets.at(-1)![0]).toBeNull();
  // Where the figures came from is recorded, because that is what makes them checkable.
  expect(data![0].source_note).toContain("ird.govt.nz");
  // Budget 2025 halved the government contribution mid-tax-year, so there is a scale on that date.
  const july = data!.find((s) => s.effective_from === "2025-07-01");
  expect(july!.kiwisaver_govt_match_bps).toBe(2_500);
  expect(july!.kiwisaver_govt_income_cap_minor).toBe(180_000_00);
});

test("editing a tax rate changes what a salary takes home", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 100_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
  });
  const before = await api.GET("/api/forecast", { params: { query: params } });

  // Put every band at 50%. The point is that a stored rate is what the projection reads — with the
  // figures as constants this edit would have changed nothing at all.
  const scales = await api.GET("/api/tax-scales", {});
  const latest = scales.data!.at(-1)!;
  const put = await api.PUT("/api/tax-scales/{id}", {
    params: { path: { id: latest.id } },
    body: {
      ...latest,
      brackets: [[null, 5_000]],
      source_note: "hand-edited",
    },
  });
  expect(put.response.status, JSON.stringify(put.error)).toBe(200);

  const after = await api.GET("/api/forecast", { params: { query: params } });
  expect(after.data!.income_net[1].median_minor).toBeLessThan(
    before.data!.income_net[1].median_minor
  );
});

test("an unusable set of rates is refused with every problem named", async ({ api }) => {
  const { response, error } = await api.POST("/api/tax-scales", {
    body: {
      effective_from: "2030-04-01",
      // Descending, and closed at the top.
      brackets: [
        [50_000_00, 1_050],
        [10_000_00, 1_750],
      ],
      esct_brackets: [[null, 3_300]],
      acc_levy_bps: 50_000,
      acc_income_cap_minor: 1,
      student_loan_threshold_minor: 1,
      student_loan_rate_bps: 1_200,
      kiwisaver_govt_match_bps: 2_500,
      kiwisaver_govt_max_minor: 260_72,
      kiwisaver_govt_income_cap_minor: null,
    },
  });
  expect(response.status).toBe(422);
  const msg = JSON.stringify(error);
  expect(msg).toContain("ascending");
  expect(msg).toContain("open-ended");
});

test("restoring puts the built-in figures back", async ({ api }) => {
  const scales = await api.GET("/api/tax-scales", {});
  const first = scales.data![0];
  await api.PUT("/api/tax-scales/{id}", {
    params: { path: { id: first.id } },
    body: { ...first, acc_levy_bps: 1 },
  });
  const restored = await api.POST("/api/tax-scales/restore", {});
  expect(restored.data![0].acc_levy_bps).not.toBe(1);
  expect(restored.data![0].source_note).toContain("ird.govt.nz");
});

test("the last set of rates cannot be deleted", async ({ api }) => {
  const { data } = await api.GET("/api/tax-scales", {});
  for (const s of data!.slice(0, -1)) {
    await api.DELETE("/api/tax-scales/{id}", { params: { path: { id: s.id } } });
  }
  const left = await api.GET("/api/tax-scales", {});
  expect(left.data).toHaveLength(1);
  // An empty table would tax every gross salary at nothing, which reads as a windfall.
  const { response } = await api.DELETE("/api/tax-scales/{id}", {
    params: { path: { id: left.data![0].id } },
  });
  expect(response.status).toBe(409);
});

test("a fund fee drags on the account it is charged against", async ({ api }) => {
  const fund = await createAccount(api, "Managed fund", "brokerage");
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: fund.id } },
    body: { as_of: new Date().toISOString().slice(0, 10), value_minor: 100_000_00 },
  });
  // A fixed expected return on both runs, so the fee is the only thing that differs.
  const base = {
    target_type: "account" as const,
    target_id: fund.id,
    annual_growth_bps: 600,
    annual_volatility_bps: 0,
  };
  await api.PUT("/api/forecast/assumptions", { body: base });
  const before = await api.GET("/api/forecast", { params: { query: params } });

  await api.PUT("/api/forecast/assumptions", {
    body: { ...base, annual_fee_bps: 105, annual_fixed_fee_minor: 30_00 },
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  expect(after.data!.months[23].assets.median_minor).toBeLessThan(
    before.data!.months[23].assets.median_minor
  );
  // Two years of 1.05% on ~$100k plus $60 of flat fees is roughly $2,200 — enough to see, and not
  // so much that a typo in the units would pass.
  const drag =
    before.data!.months[23].assets.median_minor - after.data!.months[23].assets.median_minor;
  expect(drag).toBeGreaterThan(1_500_00);
  expect(drag).toBeLessThan(3_500_00);

  const a = after.data!.assumptions.find((x) => x.target_id === fund.id);
  expect(a!.annual_fee_bps).toBe(105);
});

/** A salary paid on two fixed days a month — what people call "fortnightly on the 14th and 28th". */
async function seedTwiceMonthly(api: Parameters<typeof createPerson>[0], perPayment: number) {
  const bank = await createAccount(api, "Everyday", "bank");
  const salary = await createCategory(api, "Salary", "income");
  const today = new Date();
  for (let i = 8; i >= 1; i--) {
    for (const day of [14, 28]) {
      // Built as a string rather than via `Date.toISOString()`: a local-time Date converted to UTC
      // shifts back a day at UTC+12, which would seed the 13th and 27th and quietly test something
      // else entirely.
      const m = new Date(today.getFullYear(), today.getMonth() - i, 1);
      const posted_at = `${m.getFullYear()}-${String(m.getMonth() + 1).padStart(2, "0")}-${day}`;
      await createTransaction(api, {
        account_id: bank.id,
        posted_at,
        amount_minor: perPayment,
        description: "KAIMAHI PAYROLL",
        category_id: salary.id,
      });
    }
  }
  return { bank, salary };
}

test("a salary paid on the 14th and 28th is detected as twice a month, not fortnightly", async ({
  api,
}) => {
  // The distinction the detector exists for. Both average about a fortnight, but one is 24 payments
  // a year and the other 26 — an 8% difference in the annual figure.
  await seedTwiceMonthly(api, 5_625_00);
  const { data } = await api.GET("/api/income-streams/detect", { params: { query: {} } });
  expect(data!.length).toBeGreaterThanOrEqual(1);
  const found = data![0];
  expect(found.pay_frequency).toBe("semi_monthly");
  expect(found.days_of_month).toEqual([14, 28]);
  // 24 x $5,625 = $135,000 net — not the $146,250 that calling it fortnightly would imply.
  expect(found.annual_net_minor).toBe(5_625_00 * 24);
  expect(found.label).toBe("KAIMAHI PAYROLL");
  // The anchor is the *next* payment, so recording it does not re-credit one already in the ledger.
  expect(found.next_payment_on >= new Date().toISOString().slice(0, 10)).toBe(true);
  expect(found.next_payment_on > found.last_paid_on).toBe(true);
  // Steady amounts, so nothing to warn about.
  expect(found.variability_bps).toBe(0);
});

test("a genuinely fortnightly salary is detected as fortnightly", async ({ api }) => {
  const bank = await createAccount(api, "Everyday", "bank");
  const today = new Date();
  // Every 14 days, walking through the calendar rather than landing on fixed days.
  for (let i = 16; i >= 1; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i * 14);
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: 5_192_00,
      description: "KAIMAHI PAYROLL",
    });
  }
  const { data } = await api.GET("/api/income-streams/detect", { params: { query: {} } });
  expect(data![0].pay_frequency).toBe("fortnightly");
  expect(data![0].annual_net_minor).toBe(5_192_00 * 26);
});

test("a detected salary can be recorded as-is and reaches the projection", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const { salary } = await seedTwiceMonthly(api, 5_625_00);
  const detected = await api.GET("/api/income-streams/detect", { params: { query: {} } });
  const d = detected.data![0];

  // Exactly what the UI does with a detected row: net, because the figures are what landed.
  const stream = await createIncomeStream(api, p.id, {
    label: d.label,
    basis: "net",
    annual_amount_minor: d.annual_net_minor,
    pay_frequency: d.pay_frequency,
    first_payment_on: d.next_payment_on,
    starts_on: d.next_payment_on,
    linked_category_id: salary.id,
  });
  expect(stream.pay_frequency).toBe("semi_monthly");

  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 12, simulations: 200, seed: 6 } },
  });
  // Twice a month, every month — so each month carries two payments' worth.
  const monthly = data!.income_net.find((b) => b.median_minor > 0)!;
  expect(monthly.median_minor).toBeGreaterThan(11_000_00);
  expect(monthly.median_minor).toBeLessThan(11_500_00);
});

test("irregular credits are not offered as a salary", async ({ api }) => {
  const bank = await createAccount(api, "Everyday", "bank");
  for (const day of ["2026-01-03", "2026-02-19", "2026-05-02", "2026-05-30"]) {
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: day,
      amount_minor: 500_00,
      description: "SOME REFUND",
    });
  }
  const { data } = await api.GET("/api/income-streams/detect", { params: { query: {} } });
  expect(data).toHaveLength(0);
});
