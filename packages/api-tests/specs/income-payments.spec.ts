// The income matcher end to end: expected pays generated from a stream's schedule, deposits
// claimed by memo + date + amount, and each match carrying a payslip reconstructed from the
// observed net (gross − PAYE − ACC − KiwiSaver − student loan == what landed, to the cent).
// The arithmetic itself is unit-tested in sure-core; this suite is the wire contract and the
// lifecycle — matching, coalescing, unlinking, manual linking, pruning after a schedule edit.
//
// Every figure, name and employer is invented (CLAUDE.md rule 3). The worked example is the
// sure-core test salary: $96k semi-monthly, KiwiSaver 3.5%, student loan — one payslip nets
// $2,532.41; its quarterly $10k bonus nets $1,243.75 on top.
import { test, expect } from "../fixtures";
import {
  createAccount,
  createIncomeStream,
  createPerson,
  createTransaction,
} from "../helpers";
import type { Schemas, SureClient } from "../../client/src/index";

const SALARY_NET = 2_532_41;
const BONUS_NET = 1_243_75;
const PATTERN = "KAIMAHI";

async function seedSalaryStream(
  api: SureClient,
  over: Record<string, unknown> = {}
): Promise<{ personId: number; accountId: number; streamId: number }> {
  const person = await createPerson(api, "Rua");
  const account = await createAccount(api, "Everyday", "bank");
  const stream = await createIncomeStream(api, person.id, {
    label: "Salary",
    basis: "gross_nz_paye",
    annual_amount_minor: 96_000_00,
    pay_frequency: "semi_monthly",
    first_payment_on: "2026-05-14",
    starts_on: "2026-05-01",
    kiwisaver_bps: 350,
    student_loan: true,
    match_account_id: account.id,
    match_pattern: PATTERN,
    ...over,
  });
  return { personId: person.id, accountId: account.id, streamId: stream.id };
}

async function rematch(api: SureClient) {
  const { data, response } = await api.POST("/api/income-payments/rematch", {});
  expect(response.status, "rematch").toBe(200);
  return data!;
}

async function payments(
  api: SureClient,
  query: { from?: string; to?: string; person_id?: number; status?: string } = {}
): Promise<Schemas["IncomePayment"][]> {
  const { data, response } = await api.GET("/api/income-payments", {
    params: { query },
  });
  expect(response.status, "list payments").toBe(200);
  return data!;
}

function reconciles(p: {
  gross_minor?: number | null;
  income_tax_minor?: number | null;
  acc_levy_minor?: number | null;
  kiwisaver_minor?: number | null;
  student_loan_minor?: number | null;
  observed_net_minor?: number | null;
}) {
  expect(
    p.gross_minor! -
      p.income_tax_minor! -
      p.acc_levy_minor! -
      p.kiwisaver_minor! -
      p.student_loan_minor!,
    "the reconstructed lines must sum to what landed"
  ).toBe(p.observed_net_minor!);
}

test("the matcher claims salary deposits and reconstructs each payslip", async ({ api }) => {
  const { accountId } = await seedSalaryStream(api);
  // On the day; three days early (a long weekend); and a same-amount deposit whose memo says
  // it is something else entirely.
  await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: SALARY_NET,
    description: "KAIMAHI COLLECTIVE SALARY 041",
  });
  await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-25",
    amount_minor: SALARY_NET,
    description: "KAIMAHI COLLECTIVE SALARY 042",
  });
  const noise = await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: SALARY_NET,
    description: "TRADE ME REFUND",
  });

  const first = await rematch(api);
  expect(first.matched).toBe(2);

  const matched = await payments(api, { status: "matched" });
  expect(matched.length).toBe(2);
  for (const p of matched) {
    expect(p.matched_by).toBe("auto");
    expect(p.observed_net_minor).toBe(SALARY_NET);
    // The payslip behind a $96k/24 pay: gross $4,000 within rounding-plateau cents.
    expect(Math.abs(p.gross_minor! - 4_000_00)).toBeLessThanOrEqual(5);
    expect(p.transaction_id).not.toBe(noise.id);
    reconciles(p);
  }
  // The 28th's deposit (posted the 25th, inside the early window) claimed the 28th's row.
  expect(matched.map((p) => p.due_on).sort()).toEqual(["2026-05-14", "2026-05-28"]);

  // Later paydays stay expected — including everything between then and today.
  const expected = await payments(api, { status: "expected" });
  expect(expected.length).toBeGreaterThan(0);
  for (const p of expected) expect(p.transaction_id).toBeNull();

  // Idempotent: running it again matches nothing new and disturbs nothing old.
  const second = await rematch(api);
  expect(second.matched).toBe(0);
  expect((await payments(api, { status: "matched" })).length).toBe(2);
});

test("salary and bonus in one deposit split into two reconstructed payslips", async ({
  api,
}) => {
  const { personId, accountId } = await seedSalaryStream(api);
  await createIncomeStream(api, personId, {
    label: "Bonus",
    basis: "gross_nz_paye",
    annual_amount_minor: 10_000_00,
    pay_frequency: "quarterly",
    pay_treatment: "extra_pay",
    first_payment_on: "2026-06-14",
    starts_on: "2026-06-01",
    kiwisaver_bps: 350,
    student_loan: true,
    match_account_id: accountId,
    match_pattern: PATTERN,
  });
  // Two ordinary pays first, so the matcher has an observed base to anchor the split on.
  for (const posted of ["2026-05-14", "2026-05-28"]) {
    await createTransaction(api, {
      account_id: accountId,
      posted_at: posted,
      amount_minor: SALARY_NET,
      description: "KAIMAHI COLLECTIVE SALARY",
    });
  }
  // The quarter's pay run: salary and bonus land as one deposit.
  const combined = await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-06-14",
    amount_minor: SALARY_NET + BONUS_NET,
    description: "KAIMAHI COLLECTIVE SALARY",
  });

  await rematch(api);
  const june = (await payments(api, { status: "matched" })).filter(
    (p) => p.due_on === "2026-06-14"
  );
  expect(june.length, "both streams matched against the one deposit").toBe(2);
  expect(june.every((p) => p.transaction_id === combined.id)).toBe(true);
  const total = june.reduce((sum, p) => sum + p.observed_net_minor!, 0);
  expect(total, "the two slices cover the deposit exactly").toBe(SALARY_NET + BONUS_NET);
  for (const p of june) reconciles(p);

  // The bonus slice is an extra pay: student loan takes 12% of its whole gross, no threshold.
  const bonus = june.find((p) => p.observed_net_minor === BONUS_NET)!;
  expect(bonus.student_loan_minor).toBe(Math.round(bonus.gross_minor! * 0.12));
});

test("a match can be undone and relinked by hand, in either order of a shared deposit", async ({
  api,
}) => {
  const { accountId } = await seedSalaryStream(api);
  const deposit = await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: SALARY_NET,
    description: "KAIMAHI COLLECTIVE SALARY",
  });
  await rematch(api);
  const [matched] = await payments(api, { status: "matched" });

  // Undo: back to expected, decomposition gone, deposit released.
  const unlink = await api.DELETE("/api/income-payments/{id}/link", {
    params: { path: { id: matched.id } },
  });
  expect(unlink.response.status).toBe(200);
  expect(unlink.data!.status).toBe("expected");
  expect(unlink.data!.gross_minor).toBeNull();

  // Relink by hand: confirmed outright, with the decomposition rebuilt.
  const link = await api.POST("/api/income-payments/{id}/link", {
    params: { path: { id: matched.id } },
    body: { transaction_id: deposit.id },
  });
  expect(link.response.status, JSON.stringify(link.error)).toBe(200);
  expect(link.data!.status).toBe("confirmed");
  expect(link.data!.matched_by).toBe("manual");
  reconciles(link.data!);

  // A second payment cannot claim what this deposit no longer has.
  const other = (await payments(api, { status: "expected" }))[0];
  const overclaim = await api.POST("/api/income-payments/{id}/link", {
    params: { path: { id: other.id } },
    body: { transaction_id: deposit.id },
  });
  expect(overclaim.response.status).toBe(409);
});

test("editing the schedule prunes stray expected rows and never settled ones", async ({
  api,
}) => {
  const { personId, accountId, streamId } = await seedSalaryStream(api);
  await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: SALARY_NET,
    description: "KAIMAHI COLLECTIVE SALARY",
  });
  await rematch(api);
  expect((await payments(api, { status: "matched" })).length).toBe(1);
  const before = await payments(api, { status: "expected", person_id: personId });
  expect(before.some((p) => p.due_on.endsWith("-28"))).toBe(true);

  // The employer moves paydays to the 10th/24th. The old unsettled dates are strays now.
  const { response } = await api.PUT("/api/income-streams/{id}", {
    params: { path: { id: streamId } },
    body: {
      label: "Salary",
      currency_code: "NZD",
      basis: "gross_nz_paye",
      annual_amount_minor: 96_000_00,
      pay_frequency: "semi_monthly",
      first_payment_on: "2026-05-10",
      starts_on: "2026-05-01",
      kiwisaver_bps: 350,
      student_loan: true,
      match_account_id: accountId,
      match_pattern: PATTERN,
    },
  });
  expect(response.status).toBe(200);

  const summary = await rematch(api);
  expect(summary.pruned).toBeGreaterThan(0);
  const after = await payments(api, { person_id: personId });
  // Every unsettled row now sits on a 10th or a 24th…
  for (const p of after.filter((p) => p.status === "expected")) {
    const day = Number(p.due_on.slice(8));
    expect([10, 24]).toContain(day);
  }
  // …and the settled 14th survives untouched.
  const settled = after.find((p) => p.status === "matched");
  expect(settled?.due_on).toBe("2026-05-14");
});

test("the sankey grows a reconstructed payslip behind a matched deposit", async ({ api }) => {
  const { personId, accountId } = await seedSalaryStream(api);
  await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: SALARY_NET,
    description: "KAIMAHI COLLECTIVE SALARY",
  });
  // An unmatched deposit alongside it, to prove the layer is additive and scoped.
  await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: 100_00,
    description: "TRADE ME REFUND",
  });
  await rematch(api);

  const { data } = await api.GET("/api/reports/sankey", { params: { query: {} } });
  const g = data!;
  const gross = g.nodes.find((n) => n.id === `gross:${personId}`);
  expect(gross, "one gross node per earner").toBeDefined();
  expect(gross!.kind).toBe("gross");
  expect(g.nodes.filter((n) => n.kind === "deduction").map((n) => n.id).sort()).toEqual([
    "ded:acc",
    "ded:kiwisaver",
    "ded:paye",
    "ded:sl",
  ]);

  const from = (source: string) => g.links.filter((l) => l.source === source);
  const outOfGross = from(`gross:${personId}`).reduce((t, l) => t + l.value_minor, 0);
  // The whole payslip: net + PAYE + ACC + KiwiSaver + student loan (within reconstruction's
  // rounding-plateau cents).
  expect(Math.abs(outOfGross - 4_000_00)).toBeLessThanOrEqual(5);
  expect(
    g.links.find((l) => l.source === `gross:${personId}` && l.target === "ded:paye")
  ).toBeDefined();

  // Additive: the income side still carries BOTH deposits into the hub, and the hub balances.
  const intoHub = g.links
    .filter((l) => l.target === "center")
    .reduce((t, l) => t + l.value_minor, 0);
  const outOfHub = g.links
    .filter((l) => l.source === "center")
    .reduce((t, l) => t + l.value_minor, 0);
  expect(intoHub).toBe(SALARY_NET + 100_00);
  expect(outOfHub).toBe(intoHub); // all of it flows on to savings here
});

test("payment statuses move only along the human-owned edges", async ({ api }) => {
  const { accountId } = await seedSalaryStream(api);
  await createTransaction(api, {
    account_id: accountId,
    posted_at: "2026-05-14",
    amount_minor: SALARY_NET,
    description: "KAIMAHI COLLECTIVE SALARY",
  });
  await rematch(api);
  const [matched] = await payments(api, { status: "matched" });
  const [expected] = await payments(api, { status: "expected" });

  const set = (id: number, status: "expected" | "matched" | "confirmed" | "dismissed") =>
    api.PATCH("/api/income-payments/{id}", {
      params: { path: { id } },
      body: { status },
    });

  // matched → confirmed: agreeing with the matcher.
  expect((await set(matched.id, "confirmed")).response.status).toBe(200);
  // expected → dismissed → expected: a payday that isn't real, then a change of mind.
  expect((await set(expected.id, "dismissed")).response.status).toBe(200);
  // A dismissed row survives a rematch instead of being resurrected…
  await rematch(api);
  const held = await payments(api, { status: "dismissed" });
  expect(held.map((p) => p.id)).toContain(expected.id);
  expect((await set(expected.id, "expected")).response.status).toBe(200);
  // …and the illegal edges are refused by name.
  expect((await set(expected.id, "matched")).response.status).toBe(409);
  expect((await set(matched.id, "dismissed")).response.status).toBe(409);
});
