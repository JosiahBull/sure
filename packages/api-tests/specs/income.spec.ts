// Per-person income streams: the wire contract and the guards that only the HTTP edge can show.
// The arithmetic — pay frequencies, tax brackets, take-home resolution — is unit-tested in Rust.
//
// Every figure, name and employer here is invented. A salary is personal data, and
// `scripts/pii-scan.mjs` matches account-number and IRD shapes, not amounts, so nothing stops a
// real one landing in a fixture except not putting it there (CLAUDE.md rule 3).
import { test, expect } from "../fixtures";
import {
  createAccount,
  createCategory,
  createIncomeStream,
  createPerson,
  createTransaction,
} from "../helpers";

test("a stream is created under its person and listed with the rest of the household", async ({
  api,
}) => {
  const a = await createPerson(api, "Rua");
  const b = await createPerson(api, "Tane");
  await createIncomeStream(api, a.id, { label: "Teaching" });
  await createIncomeStream(api, b.id, { label: "Workshop", annual_amount_minor: 61_000_00 });

  const { data } = await api.GET("/api/income-streams", {});
  expect(data!.map((s) => s.label).sort()).toEqual(["Teaching", "Workshop"]);
  expect(data!.find((s) => s.label === "Teaching")!.person_id).toBe(a.id);
});

test("creating a stream for a person who doesn't exist is refused, not left dangling", async ({
  api,
}) => {
  const { response } = await api.POST("/api/people/{person_id}/income-streams", {
    params: { path: { person_id: 9_999 } },
    body: {
      label: "Ghost",
      currency_code: "NZD",
      annual_amount_minor: 50_000_00,
      basis: "net",
      pay_frequency: "monthly",
      first_payment_on: "2026-04-03",
      starts_on: "2026-04-01",
    },
  });
  expect(response.status).toBe(422);
});

test("pay-scale steps come back in date order however they were entered", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const s = await createIncomeStream(api, p.id, {
    label: "Teaching",
    steps: [
      { effective_on: "2029-04-01", annual_amount_minor: 100_000_00, label: "Step 7" },
      { effective_on: "2027-04-01", annual_amount_minor: 92_000_00, label: "Step 5" },
      { effective_on: "2028-04-01", annual_amount_minor: 96_000_00, label: "Step 6" },
    ],
  });
  expect(s.steps.map((x) => x.effective_on)).toEqual([
    "2027-04-01",
    "2028-04-01",
    "2029-04-01",
  ]);
  expect(s.steps.map((x) => x.label)).toEqual(["Step 5", "Step 6", "Step 7"]);
});

test("two steps on the same date are refused with the date named", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const { response, error } = await api.POST("/api/people/{person_id}/income-streams", {
    params: { path: { person_id: p.id } },
    body: {
      label: "Teaching",
      currency_code: "NZD",
      annual_amount_minor: 88_000_00,
      basis: "gross_nz_paye",
      pay_frequency: "fortnightly",
      first_payment_on: "2026-04-03",
      starts_on: "2026-04-01",
      steps: [
        { effective_on: "2027-04-01", annual_amount_minor: 92_000_00 },
        { effective_on: "2027-04-01", annual_amount_minor: 93_000_00 },
      ],
    },
  });
  expect(response.status).toBe(422);
  // A scale can't be at two points at once, and the message has to say which date clashed —
  // "constraint failed" would leave the user hunting through their own schedule.
  expect(JSON.stringify(error)).toContain("2027-04-01");
});

test("an update replaces the whole schedule, so a step can actually be removed", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const s = await createIncomeStream(api, p.id, {
    steps: [
      { effective_on: "2027-04-01", annual_amount_minor: 92_000_00 },
      { effective_on: "2028-04-01", annual_amount_minor: 96_000_00 },
    ],
  });
  const { data } = await api.PUT("/api/income-streams/{id}", {
    params: { path: { id: s.id } },
    body: {
      label: "Teaching",
      currency_code: "NZD",
      annual_amount_minor: 88_000_00,
      basis: "gross_nz_paye",
      pay_frequency: "fortnightly",
      first_payment_on: "2026-04-03",
      starts_on: "2026-04-01",
      steps: [{ effective_on: "2027-04-01", annual_amount_minor: 92_000_00 }],
    },
  });
  expect(data!.steps).toHaveLength(1);
});

test("every problem in one response, not the first one found", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const { response, error } = await api.POST("/api/people/{person_id}/income-streams", {
    params: { path: { person_id: p.id } },
    body: {
      label: "   ",
      currency_code: "NZD",
      annual_amount_minor: 88_000_00,
      basis: "gross_nz_paye",
      pay_frequency: "fortnightly",
      first_payment_on: "2026-04-03",
      starts_on: "2026-04-01",
      ends_on: "2025-01-01",
      kiwisaver_bps: 90_000,
    },
  });
  expect(response.status).toBe(422);
  const msg = JSON.stringify(error);
  // Filling in a form should not be a game of whack-a-mole.
  expect(msg).toContain("label");
  expect(msg).toContain("kiwisaver_bps");
  expect(msg).toContain("ends_on");
});

test("a person who still earns cannot be deleted, and the message names the income", async ({
  api,
}) => {
  // Two people, because the household refuses to empty itself for an unrelated reason and that
  // would mask what this is testing.
  const p = await createPerson(api, "Rua");
  await createPerson(api, "Tane");
  await createIncomeStream(api, p.id, { label: "Teaching" });

  const blocked = await api.DELETE("/api/people/{id}", { params: { path: { id: p.id } } });
  expect(blocked.response.status).toBe(409);
  expect(JSON.stringify(blocked.error)).toContain("Teaching");

  // …and once the income is gone, so can they be.
  const streams = await api.GET("/api/income-streams", {});
  await api.DELETE("/api/income-streams/{id}", {
    params: { path: { id: streams.data![0].id } },
  });
  const ok = await api.DELETE("/api/people/{id}", { params: { path: { id: p.id } } });
  expect(ok.response.status).toBe(204);
});

test("a linked income category is remembered, so the forecast can net against it", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const salary = await createCategory(api, "Salary", "income");
  const s = await createIncomeStream(api, p.id, { linked_category_id: salary.id });
  expect(s.linked_category_id).toBe(salary.id);
});

test("a net stream and a gross stream are stored as the different things they are", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const gross = await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    student_loan: true,
    kiwisaver_bps: 350,
  });
  const net = await createIncomeStream(api, p.id, {
    label: "Board",
    basis: "net",
    annual_amount_minor: 9_100_00,
    pay_frequency: "quarterly",
  });
  expect(gross.basis).toBe("gross_nz_paye");
  expect(gross.student_loan).toBe(true);
  expect(gross.kiwisaver_bps).toBe(350);
  expect(net.basis).toBe("net");
  expect(net.pay_frequency).toBe("quarterly");
});

test("a snapshot round-trips income streams and their steps", async ({ api, server }) => {
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Teaching",
    steps: [{ effective_on: "2027-04-01", annual_amount_minor: 92_000_00, label: "Step 5" }],
  });

  const dump = await fetch(`${server.baseURL}/api/config/export`);
  const snapshot = await dump.json();
  expect(snapshot.income_streams).toHaveLength(1);
  expect(snapshot.income_stream_steps).toHaveLength(1);

  const restored = await fetch(`${server.baseURL}/api/config/import`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(snapshot),
  });
  expect(restored.status).toBe(200);

  const after = await api.GET("/api/income-streams", {});
  expect(after.data).toHaveLength(1);
  expect(after.data![0].steps).toHaveLength(1);
  expect(after.data![0].steps[0].label).toBe("Step 5");
});

// ---- income in the projection -------------------------------------------------------
// The arithmetic (pay calendars, brackets, netting) is unit-tested in Rust. These cover the
// end-to-end claims: that a stream reaches the projection at all, that it does not get counted
// twice against the category it lands in, and that the reconciliation says so when it would have.

test("a modelled salary raises the projection's cash flow", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const params = { horizon_months: 12, simulations: 200, seed: 9 };
  const before = await api.GET("/api/forecast", { params: { query: params } });

  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "net",
    annual_amount_minor: 60_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
  });

  const after = await api.GET("/api/forecast", { params: { query: params } });
  const first = after.data!.income_net[0];
  // $60k net a year, paid monthly, is $5k a month landing in the pool.
  expect(first.median_minor).toBeGreaterThan(4_900_00);
  expect(first.median_minor).toBeLessThan(5_100_00);
  expect(after.data!.months[11].net_worth.median_minor).toBeGreaterThan(
    before.data!.months[11].net_worth.median_minor
  );
});

test("a gross salary lands less in the pool than its headline figure", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 120_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    student_loan: true,
  });
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 6, simulations: 200, seed: 9 } },
  });
  const monthly = data!.income_net[0].median_minor;
  // $10,000/mo gross. PAYE + ACC + 3.5% KiwiSaver + 12% student loan takes a large bite; the
  // exact figure is pinned by the Rust bracket tests, so this asserts only that gross is not
  // being treated as take-home — the mistake that would make the whole feature lie.
  expect(monthly).toBeGreaterThan(5_000_00);
  expect(monthly).toBeLessThan(8_000_00);
});

test("a stream linked to its category nets against it instead of double-counting", async ({
  api,
}) => {
  // A year of observed salary, so the category has a real fitted baseline of ~$5,000/mo.
  const bank = await createAccount(api, "Everyday", "bank");
  const salary = await createCategory(api, "Salary", "income");
  const today = new Date();
  for (let i = 12; i >= 1; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: 5_000_00,
      description: "Salary",
      category_id: salary.id,
    });
  }
  const params = { horizon_months: 12, simulations: 200, seed: 4 };
  const fitted = await api.GET("/api/forecast", { params: { query: params } });
  const fittedEnd = fitted.data!.months[11].net_worth.median_minor;

  // Now model the *same* salary as a stream, linked to the same category.
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "net",
    annual_amount_minor: 60_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    linked_category_id: salary.id,
  });
  const netted = await api.GET("/api/forecast", { params: { query: params } });

  // The projection must be about the same, not about twice as good: the stream replaced the
  // fitted baseline rather than being added on top of it.
  const nettedEnd = netted.data!.months[11].net_worth.median_minor;
  expect(Math.abs(nettedEnd - fittedEnd)).toBeLessThan(Math.abs(fittedEnd) * 0.2);

  // …and the category now says where its figure came from, with the residual as the baseline.
  const a = netted.data!.assumptions.find(
    (x) => x.target_type === "category" && x.target_id === salary.id
  );
  expect(a?.source).toBe("modelled_from_income");

  const recon = netted.data!.reconciliations.find((r) => r.category_id === salary.id);
  expect(recon).toBeDefined();
  expect(recon!.person_id).toBe(p.id);
  // Modelled ~= observed, so coverage is ~100% and nothing is left for the trend to project.
  expect(recon!.coverage_bps).toBeGreaterThan(9_000);
  expect(recon!.coverage_bps).toBeLessThan(11_000);
});

test("a gross salary modelled as take-home shows up as coverage well over 100%", async ({
  api,
}) => {
  // The regression this whole readout exists for. Observed net is ~$5,000/mo; someone types their
  // $120k gross and marks it net, so the streams claim ~$10,000/mo against a category that only
  // ever saw half that.
  const bank = await createAccount(api, "Everyday", "bank");
  const salary = await createCategory(api, "Salary", "income");
  const today = new Date();
  for (let i = 12; i >= 1; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await createTransaction(api, {
      account_id: bank.id,
      posted_at: d.toISOString().slice(0, 10),
      amount_minor: 5_000_00,
      description: "Salary",
      category_id: salary.id,
    });
  }
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "net",
    annual_amount_minor: 120_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    linked_category_id: salary.id,
  });
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 6, simulations: 200, seed: 4 } },
  });
  const recon = data!.reconciliations.find((r) => r.category_id === salary.id);
  expect(recon!.coverage_bps).toBeGreaterThan(15_000);
  // The residual floors at zero rather than going negative — but the coverage figure above is
  // what tells the user, which is why it is not clamped too.
  expect(recon!.residual_minor).toBe(0);
});

test("a stream in a currency with no rate is left out and named, not counted at parity", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  // The currency has to exist — the FK sees to that — but nothing gives it a rate to the base
  // currency, which is the condition under test.
  await api.POST("/api/currencies", {
    body: { code: "JPY", name: "Japanese yen", symbol: "\u00a5", decimal_places: 0 },
  });
  await createIncomeStream(api, p.id, {
    label: "Overseas contract",
    currency_code: "JPY",
    basis: "net",
    annual_amount_minor: 6_000_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
  });
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 6, simulations: 200, seed: 4 } },
  });
  expect(data!.unmodelled_streams.join(" ")).toContain("Overseas contract");
  // Nothing was credited: an amount at the wrong rate is a wrong number that looks right.
  expect(data!.income_net[0].median_minor).toBe(0);
});

test("a stream that has already ended contributes nothing", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  await createIncomeStream(api, p.id, {
    label: "Old job",
    basis: "net",
    annual_amount_minor: 60_000_00,
    pay_frequency: "monthly",
    first_payment_on: "2024-01-01",
    starts_on: "2024-01-01",
    ends_on: "2025-01-01",
  });
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 6, simulations: 200, seed: 4 } },
  });
  expect(data!.income_net.every((b) => b.median_minor === 0)).toBe(true);
});

/** The first of next month, so a stream starts inside the projection rather than in history. */
function firstOfNextMonth(): string {
  const d = new Date();
  d.setMonth(d.getMonth() + 1, 1);
  return d.toISOString().slice(0, 10);
}

// ---- where the deductions go ---------------------------------------------------------
// KiwiSaver and student-loan deductions were computed correctly and then vanished. These cover the
// two claims that matter: the money now accumulates somewhere, and linking an account stops its own
// fitted rate being used — because that rate already contains the contributions.

/** An investment account with a rising valuation history, so it has a fitted growth rate to discard. */
async function kiwisaverAccount(api: Parameters<typeof createPerson>[0]) {
  const acct = await createAccount(api, "KiwiSaver", "brokerage");
  const today = new Date();
  let value = 40_000_00;
  for (let i = 14; i >= 0; i--) {
    const d = new Date(today);
    d.setMonth(d.getMonth() - i);
    await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: acct.id } },
      body: { as_of: d.toISOString().slice(0, 10), value_minor: Math.round(value) },
    });
    value *= 1.015; // ~20%/yr — market *and* contributions, indistinguishable after the fact
  }
  return acct;
}

test("KiwiSaver contributions accumulate in the linked account instead of vanishing", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const ks = await kiwisaverAccount(api);
  const params = { horizon_months: 60, simulations: 200, seed: 8 };

  // An explicit expected return on both runs, so growth is identical either way and the *only*
  // difference is whether the contributions land. Without it this compares two different things:
  // linking deliberately discards the fitted rate, so an unlinked run keeps a ~20%/yr figure that is
  // partly contributions already — and it wins on magnitude while being wrong.
  await api.PUT("/api/forecast/assumptions", {
    body: {
      target_type: "account",
      target_id: ks.id,
      annual_growth_bps: 500,
      annual_volatility_bps: 0,
    },
  });

  const stream = await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 100_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    employer_kiwisaver_bps: 350,
  });
  const before = await api.GET("/api/forecast", { params: { query: params } });

  await api.PUT("/api/income-streams/{id}", {
    params: { path: { id: stream.id } },
    body: {
      label: "Salary",
      currency_code: "NZD",
      annual_amount_minor: 100_000_00,
      basis: "gross_nz_paye",
      pay_frequency: "monthly",
      first_payment_on: firstOfNextMonth(),
      starts_on: firstOfNextMonth(),
      kiwisaver_bps: 350,
      employer_kiwisaver_bps: 350,
      kiwisaver_account_id: ks.id,
    },
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  // Five years of 3.5% + 3.5% less 33% ESCT on $100k is about $29k of contributions, before the
  // growth each one then earns.
  const assetsBefore = before.data!.months[59].assets.median_minor;
  const assetsAfter = after.data!.months[59].assets.median_minor;
  expect(assetsAfter - assetsBefore).toBeGreaterThan(28_000_00);
  expect(assetsAfter - assetsBefore).toBeLessThan(40_000_00);

  // Take-home is untouched: the employer's half was never spendable, and the employee's was already
  // deducted before this change.
  expect(after.data!.income_net[11].median_minor).toBe(before.data!.income_net[11].median_minor);
});

/// Linking can make a projection *smaller*, and that is the feature rather than a regression: a
/// fitted rate on an account that has been receiving contributions is flattering, because part of
/// what it measured was the money going in rather than growth.
test("linking replaces a flattering fitted rate, so the projection can honestly fall", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const ks = await kiwisaverAccount(api);
  const params = { horizon_months: 60, simulations: 200, seed: 8 };
  const stream = await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 100_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    employer_kiwisaver_bps: 350,
  });
  const before = await api.GET("/api/forecast", { params: { query: params } });

  await api.PUT("/api/income-streams/{id}", {
    params: { path: { id: stream.id } },
    body: {
      label: "Salary",
      currency_code: "NZD",
      annual_amount_minor: 100_000_00,
      basis: "gross_nz_paye",
      pay_frequency: "monthly",
      first_payment_on: firstOfNextMonth(),
      starts_on: firstOfNextMonth(),
      kiwisaver_bps: 350,
      employer_kiwisaver_bps: 350,
      kiwisaver_account_id: ks.id,
    },
  });
  const after = await api.GET("/api/forecast", { params: { query: params } });

  // ~20%/yr compounding on the starting balance beats five years of contributions on a flat one.
  expect(after.data!.months[59].assets.median_minor).toBeLessThan(
    before.data!.months[59].assets.median_minor
  );
  // And the user is told why, rather than left to wonder where the growth went.
  expect(after.data!.warnings.join(" ")).toContain("discarded");
});

test("linking an account discards its fitted rate, and says why", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const ks = await kiwisaverAccount(api);

  const unlinked = await api.GET("/api/forecast/assumptions", {});
  const derived = unlinked.data!.find((a) => a.target_type === "account" && a.target_id === ks.id);
  // It has a real fitted rate — which is exactly the problem: that ~20%/yr is market growth *and*
  // contributions, and nothing can separate them after the fact.
  expect(derived?.source).toBe("derived");
  expect(derived!.annual_growth_bps).toBeGreaterThan(1_000);

  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 100_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    employer_kiwisaver_bps: 350,
    kiwisaver_account_id: ks.id,
  });

  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 12, simulations: 200, seed: 8 } },
  });
  const a = data!.assumptions.find((x) => x.target_type === "account" && x.target_id === ks.id);
  expect(a?.source).toBe("contribution_driven");
  expect(a!.annual_growth_bps).toBe(0);
  // …and it does not do that silently.
  expect(data!.warnings.join(" ")).toContain("KiwiSaver");
});

test("an expected return set by hand is kept, because that is an assertion not a measurement", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const ks = await kiwisaverAccount(api);
  await api.PUT("/api/forecast/assumptions", {
    body: {
      target_type: "account",
      target_id: ks.id,
      annual_growth_bps: 600,
      annual_volatility_bps: 1_200,
    },
  });
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 100_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    kiwisaver_account_id: ks.id,
  });
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 12, simulations: 200, seed: 8 } },
  });
  const a = data!.assumptions.find((x) => x.target_type === "account" && x.target_id === ks.id);
  // An override survives: the user asserted 6%, which cannot contain contributions the way a
  // measured rate does. No warning either — there is nothing to warn about.
  expect(a?.source).toBe("override");
  expect(a!.annual_growth_bps).toBe(600);
  expect(data!.warnings.join(" ")).not.toContain("KiwiSaver");
});

test("student loan repayments pay the loan down, and it stops at zero", async ({ api }) => {
  const p = await createPerson(api, "Rua");
  const loan = await createAccount(api, "Student loan", "student_loan", "NZD", {
    metadata: { profile: "student_loan", lender: "Inland Revenue", interest_rate_bps: 0 },
  });
  // A small balance, so five years of 12%-over-threshold repayments clears it inside the horizon.
  await api.POST("/api/accounts/{id}/valuations", {
    params: { path: { id: loan.id } },
    body: { as_of: new Date().toISOString().slice(0, 10), value_minor: -6_000_00 },
  });
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 90_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    student_loan: true,
    student_loan_account_id: loan.id,
  });

  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 60, simulations: 200, seed: 8 } },
  });
  // 12% of ($90k - $24,128) is ~$7,900/yr, so a $6,000 loan is gone within the first year…
  expect(data!.months[11].liabilities.median_minor).toBe(0);
  // …and stays gone. A repaid debt must not run past zero into being an asset.
  expect(data!.months[59].liabilities.median_minor).toBe(0);
});

test("a contribution pointed at an account the projection does not model is reported", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  // A cash account is pooled rather than simulated, so it has no slot to credit.
  const bank = await createAccount(api, "Everyday", "bank");
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 90_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    kiwisaver_account_id: bank.id,
  });
  const { data } = await api.GET("/api/forecast", {
    params: { query: { horizon_months: 12, simulations: 200, seed: 8 } },
  });
  // Left where it was rather than credited to the wrong place, and named so it can be fixed.
  expect(data!.warnings.join(" ")).toContain("not being tracked");
});

test("deleting an account a stream contributes to is refused rather than silently unlinked", async ({
  api,
}) => {
  const p = await createPerson(api, "Rua");
  const ks = await kiwisaverAccount(api);
  await createIncomeStream(api, p.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 90_000_00,
    pay_frequency: "monthly",
    first_payment_on: firstOfNextMonth(),
    starts_on: firstOfNextMonth(),
    kiwisaver_bps: 350,
    kiwisaver_account_id: ks.id,
  });
  // Unlinking silently would put the account back on a rate that has contributions baked into it —
  // the exact double count the link exists to avoid, done invisibly.
  const { response } = await api.DELETE("/api/accounts/{id}", { params: { path: { id: ks.id } } });
  expect(response.status).toBeGreaterThanOrEqual(400);
});
