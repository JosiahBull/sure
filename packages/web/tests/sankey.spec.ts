import { type APIRequestContext, type Page } from "@playwright/test";

import { test, expect } from "./fixtures";

/**
 * Crossings in the money-flow chart, counted off the rendered SVG.
 *
 * A Sankey earns its keep by being readable at a glance, and a ribbon cutting across the
 * diagram to reach a node on the far side undoes that faster than anything else in the chart.
 * `outwardOrder` in Sankey.svelte exists to make crossings impossible for the category tree,
 * and it succeeds — but the payslip layer is not part of that tree, and it was drawn as a
 * ribbon from a deduction sink near the top of the graph to an account parked at the bottom of
 * the hub's own column, cutting through every income flow on the way.
 *
 * The seeded demo data has no payslip layer at all (no income stream, so no gross pay and no
 * deductions), which is why this builds its own — the bug lived in the one part of the graph
 * the suite could not see.
 */

/** Invented throughout, per CLAUDE.md rule 3 — none of this is anybody's real pay. */
const GROSS_ANNUAL_MINOR = 82_151_29;
const PAY_MEMO = "KAIMAHI LTD PAYROLL";

type Created = { streamId: number; accountIds: number[]; txIds: number[] };

async function api<T>(request: APIRequestContext, method: "get" | "post" | "put" | "delete", path: string, body?: unknown): Promise<T> {
  const res = await request[method](path, body === undefined ? {} : { data: body });
  expect(res.ok(), `${method.toUpperCase()} ${path} -> ${res.status()} ${await res.text()}`).toBe(true);
  return res.status() === 204 ? (null as T) : ((await res.json()) as T);
}

/**
 * Give the household a salary whose deductions are routed onward into real accounts, which is
 * what makes the chart draw `gross → deduction → destination` at all.
 *
 * The deposits are the point: a payslip is *reconstructed from a matched deposit*, so a stream
 * on its own draws nothing. Each expected pay is deposited at exactly its expected net, then
 * the matcher is run.
 */
async function seedPayslips(request: APIRequestContext): Promise<Created> {
  const people = await api<{ id: number }[]>(request, "get", "/api/people");
  const person = people[0];
  const accounts = await api<{ id: number; kind: string }[]>(request, "get", "/api/accounts");
  const everyday = accounts.find((a) => a.kind === "bank")!;

  const loan = await api<{ id: number }>(request, "post", "/api/accounts", {
    name: "Student loan",
    kind: "student_loan",
    currency_code: "NZD",
    ownership: { kind: "person", person_id: person.id },
    opening_balance_minor: -18_400_00,
    opening_balance_date: "2025-09-01",
    metadata: { profile: "student_loan", lender: "StudyLink", interest_rate_bps: 0 },
  });
  // A second routed deduction, because one destination cannot show whether two of them keep
  // their parents' order.
  const kiwisaver = await api<{ id: number }>(request, "post", "/api/accounts", {
    name: "KiwiSaver",
    kind: "savings",
    currency_code: "NZD",
    institution: "Provider",
    ownership: { kind: "person", person_id: person.id },
    opening_balance_minor: 24_000_00,
    opening_balance_date: "2025-09-01",
  });

  const stream = await api<{ id: number }>(request, "post", `/api/people/${person.id}/income-streams`, {
    label: "Salary",
    currency_code: "NZD",
    ownership: { kind: "person", person_id: person.id },
    basis: "gross_nz_paye",
    annual_amount_minor: GROSS_ANNUAL_MINOR,
    pay_frequency: "semi_monthly",
    first_payment_on: "2025-09-15",
    starts_on: "2025-09-01",
    kiwisaver_bps: 350,
    student_loan: true,
    student_loan_account_id: loan.id,
    kiwisaver_account_id: kiwisaver.id,
    match_account_id: everyday.id,
    match_pattern: "KAIMAHI",
  });

  await api(request, "post", "/api/income-payments/rematch");
  const pays = await api<{ due_on: string; expected_net_minor: number }[]>(
    request,
    "get",
    "/api/income-payments?from=2025-09-01&to=2026-09-15",
  );
  expect(pays.length, "the stream generated no expected pays").toBeGreaterThan(0);

  const txIds: number[] = [];
  for (const pay of pays) {
    const tx = await api<{ id: number }>(request, "post", "/api/transactions", {
      account_id: everyday.id,
      posted_at: pay.due_on,
      amount_minor: pay.expected_net_minor,
      description: PAY_MEMO,
      currency_code: "NZD",
    });
    txIds.push(tx.id);
  }
  const matched = await api<{ matched: number }>(request, "post", "/api/income-payments/rematch");
  expect(matched.matched, "no deposit was matched, so no payslip was reconstructed").toBeGreaterThan(0);

  return { streamId: stream.id, accountIds: [loan.id, kiwisaver.id], txIds };
}

/** Put the database back, so a later spec sees the data it was seeded with. */
async function cleanUp(request: APIRequestContext, made: Created) {
  await api(request, "delete", `/api/income-streams/${made.streamId}`);
  for (const id of made.txIds) await api(request, "delete", `/api/transactions/${id}`);
  for (const id of made.accountIds) await api(request, "delete", `/api/accounts/${id}`);
}

/**
 * Count crossings between the drawn ribbons, by sampling them.
 *
 * Sampled rather than compared at the endpoints, because the textbook "same gap, endpoints
 * inverted" test cannot see a link that spans more than one column — and until this fix the
 * worst ribbon in the chart was exactly that.
 *
 * Every pair counts. It did not always: a ribbon reaching across a whole column used to be
 * outside what the ordering could promise, so those pairs were excluded to keep the assertion
 * honest. Routing every such link through a waypoint removed the category — `sankey-layout.spec`
 * asserts no link reaches more than one column — so the exclusion would now exclude nothing, and
 * counting everything is simply the stronger statement.
 */
async function crossings(page: Page, selector: string): Promise<number> {
  return page.evaluate((sel) => {
    const svg = document.querySelector(`${sel} svg`);
    if (!svg) throw new Error(`no sankey rendered at ${sel}`);
    const links = [...svg.querySelectorAll("path.link")].map((el) => {
      const len = (el as SVGPathElement).getTotalLength();
      const pts = Array.from({ length: 41 }, (_, i) => (el as SVGPathElement).getPointAtLength((len * i) / 40));
      return { pts, x0: pts[0].x, x1: pts[pts.length - 1].x };
    });
    type L = (typeof links)[number];
    const yAt = (l: L, x: number): number | null => {
      for (let i = 1; i < l.pts.length; i++) {
        const a = l.pts[i - 1];
        const c = l.pts[i];
        if ((x >= a.x && x <= c.x) || (x >= c.x && x <= a.x)) {
          const t = c.x === a.x ? 0 : (x - a.x) / (c.x - a.x);
          return a.y + t * (c.y - a.y);
        }
      }
      return null;
    };
    let found = 0;
    for (let i = 0; i < links.length; i++) {
      for (let j = i + 1; j < links.length; j++) {
        const A = links[i];
        const B = links[j];
        const lo = Math.max(Math.min(A.x0, A.x1), Math.min(B.x0, B.x1));
        const hi = Math.min(Math.max(A.x0, A.x1), Math.max(B.x0, B.x1));
        if (hi - lo < 2) continue;
        let prev: number | null = null;
        for (let s = 0; s <= 24; s++) {
          const x = lo + ((hi - lo) * s) / 24;
          const ya = yAt(A, x);
          const yb = yAt(B, x);
          if (ya == null || yb == null) continue;
          const sign = Math.sign(ya - yb);
          if (sign !== 0 && prev !== null && sign !== prev) found++;
          if (sign !== 0) prev = sign;
        }
      }
    }
    return found;
  }, selector);
}

/** Node id → its drawn box, so placement can be asserted rather than eyeballed. */
async function boxes(page: Page): Promise<Record<string, { x: number; y0: number; y1: number }>> {
  return page.evaluate(() => {
    const out: Record<string, { x: number; y0: number; y1: number }> = {};
    for (const g of document.querySelectorAll(".sankey-wrap g.node")) {
      const path = g.querySelector("path");
      const id = (g as HTMLElement).dataset.nodeId;
      if (path && id) {
        const bb = (path as SVGPathElement).getBBox();
        out[id] = { x: Math.round(bb.x), y0: Math.round(bb.y), y1: Math.round(bb.y + bb.height) };
      }
    }
    return out;
  });
}

test.describe("money flow", () => {
  // Wide enough that `fitToWidth` keeps the full graph — this is the shape the chart is
  // designed around, and the one the reported screenshot was taken at.
  test.use({ viewport: { width: 1440, height: 1000 }, isMobile: false, hasTouch: false });

  let made: Created;

  test.beforeAll(async ({ playwright, baseURL }) => {
    const request = await playwright.request.newContext({ baseURL });
    made = await seedPayslips(request);
    await request.dispose();
  });

  test.afterAll(async ({ playwright, baseURL }) => {
    const request = await playwright.request.newContext({ baseURL });
    await cleanUp(request, made);
    await request.dispose();
  });

  test("a routed deduction lands beside the sink it came from, not across the chart", async ({ page }) => {
    await page.goto("/#/");
    await page.waitForLoadState("networkidle");
    await expect(page.getByRole("heading", { name: "Money flow" })).toBeVisible();
    await expect(page.locator(".sankey-wrap g.node").first()).toBeVisible();

    const box = await boxes(page);
    const deduction = box["ded:sl"];
    const destination = box["dest:" + made.accountIds[0]];
    expect(deduction, "the student-loan deduction was not drawn").toBeTruthy();
    expect(destination, "the student-loan account was not drawn").toBeTruthy();

    // One column to the right, not on the hub's spine — the difference between a short hop and
    // a ribbon across the whole income fan.
    const columns = [...new Set(Object.values(box).map((b) => b.x))].sort((a, b) => a - b);
    const columnOf = (b: { x: number }) => columns.indexOf(b.x);
    expect(columnOf(destination) - columnOf(deduction), "the destination is not one column past its deduction").toBe(1);

    // And at its parent's height, so the ribbon between them is a short horizontal band.
    const centre = (b: { y0: number; y1: number }) => (b.y0 + b.y1) / 2;
    expect(Math.abs(centre(destination) - centre(deduction))).toBeLessThan(40);

    // Two routed deductions keep their sinks' order.
    const other = box["dest:" + made.accountIds[1]];
    if (other) {
      expect(centre(destination), "the destinations are not in their deductions' order").toBeLessThan(centre(other));
    }
  });

  test("no ribbon crosses another, inline or expanded", async ({ page }) => {
    await page.goto("/#/");
    await page.waitForLoadState("networkidle");
    await expect(page.locator(".sankey-wrap g.node").first()).toBeVisible();

    expect(await crossings(page, ".sankey-wrap"), "the inline chart has crossing ribbons").toBe(0);

    // The expand view lays the same graph out at full depth, where there is most to cross.
    await page.getByRole("button", { name: "Expand" }).click();
    await expect(page.locator(".overlay .sankey-wrap g.node").first()).toBeVisible();
    expect(await crossings(page, ".overlay .sankey-wrap"), "the expanded chart has crossing ribbons").toBe(0);
  });

  test("the two same-named payslip nodes do not draw their labels on top of each other", async ({ page }) => {
    await page.goto("/#/");
    await page.waitForLoadState("networkidle");
    await expect(page.locator(".sankey-wrap g.node").first()).toBeVisible();

    // A deduction and the account it feeds carry the same name and the same figure, and sit in
    // adjacent columns — so labelling the account leftwards drew it back over its own sink's,
    // rendering "Student loan" as "Student Stadent loan".
    const rects = await page.evaluate(() => {
      const wanted = ["ded:sl", "dest:"];
      return [...document.querySelectorAll(".sankey-wrap g.node")]
        .filter((g) => wanted.some((w) => ((g as HTMLElement).dataset.nodeId ?? "").startsWith(w)))
        .map((g) => {
          const t = g.querySelector("text");
          const r = t?.getBoundingClientRect();
          // A label the chart has already suppressed for crowding still has a box; it is just
          // invisible, and an invisible label cannot collide with anything a reader sees.
          const shown = t ? parseFloat(getComputedStyle(t).opacity) > 0.01 : false;
          return t && r && r.width > 0 && shown
            ? { id: (g as HTMLElement).dataset.nodeId, left: r.left, right: r.right, top: r.top, bottom: r.bottom }
            : null;
        })
        .filter(Boolean) as { id: string; left: number; right: number; top: number; bottom: number }[];
    });

    for (let i = 0; i < rects.length; i++) {
      for (let j = i + 1; j < rects.length; j++) {
        const a = rects[i];
        const b = rects[j];
        const overlaps =
          a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
        expect(overlaps, `${a.id} and ${b.id} draw their labels over each other`).toBe(false);
      }
    }
  });
});
