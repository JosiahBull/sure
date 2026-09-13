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
  // The compulsory employer contribution stepped 3% -> 3.5% on the tax-year boundary, and each
  // scale carries the figure that really applied to it rather than today's.
  expect(july!.kiwisaver_employer_min_bps).toBe(300);
  expect(
    data!.find((s) => s.effective_from === "2026-04-01")!.kiwisaver_employer_min_bps
  ).toBe(350);
  // The 2028 step ships too, and says in its own note that only its KiwiSaver figure is a 2028
  // fact — the rest is this year's scale carried forward.
  const projected = data!.find((s) => s.effective_from === "2028-04-01");
  expect(projected!.kiwisaver_employer_min_bps).toBe(400);
  expect(projected!.source_note).toContain("carried forward");
});

test("the compulsory employer contribution can be changed for a future year", async ({ api }) => {
  // The reason it is a setting at all: the rate has moved twice since 2025 and will again, and
  // nobody should need a new binary to record the next one. The date and figure here are
  // hypothetical — the two real steps ship as built-ins.
  const scales = await api.GET("/api/tax-scales", {});
  const latest = scales.data!.at(-1)!;
  const { response, data } = await api.POST("/api/tax-scales", {
    body: {
      ...latest,
      effective_from: "2031-04-01",
      kiwisaver_employer_min_bps: 450,
      source_note: "invented for this test",
    },
  });
  expect(response.status, JSON.stringify(data)).toBe(201);
  expect(data!.kiwisaver_employer_min_bps).toBe(450);

  // …and it survives the round trip rather than only echoing back off the write.
  const after = await api.GET("/api/tax-scales", {});
  const added = after.data!.find((s) => s.effective_from === "2031-04-01");
  expect(added!.kiwisaver_employer_min_bps).toBe(450);
  // Every scale before it is untouched — that is what dating them is for.
  expect(
    after.data!.find((s) => s.effective_from === "2026-04-01")!.kiwisaver_employer_min_bps
  ).toBe(350);
  expect(
    after.data!.find((s) => s.effective_from === "2028-04-01")!.kiwisaver_employer_min_bps
  ).toBe(400);
});

test("an employer minimum above 100% is refused", async ({ api }) => {
  const scales = await api.GET("/api/tax-scales", {});
  const { response, error } = await api.PUT("/api/tax-scales/{id}", {
    params: { path: { id: scales.data!.at(-1)!.id } },
    body: { ...scales.data!.at(-1)!, kiwisaver_employer_min_bps: 20_000 },
  });
  expect(response.status).toBe(422);
  expect(JSON.stringify(error)).toContain("kiwisaver_employer_min_bps");
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
      kiwisaver_employer_min_bps: 350,
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
