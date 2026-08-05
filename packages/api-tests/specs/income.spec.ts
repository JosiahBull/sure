// Per-person income streams: the wire contract and the guards that only the HTTP edge can show.
// The arithmetic — pay frequencies, tax brackets, take-home resolution — is unit-tested in Rust.
//
// Every figure, name and employer here is invented. A salary is personal data, and
// `scripts/pii-scan.mjs` matches account-number and IRD shapes, not amounts, so nothing stops a
// real one landing in a fixture except not putting it there (CLAUDE.md rule 3).
import { test, expect } from "../fixtures";
import { createCategory, createIncomeStream, createPerson } from "../helpers";

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
