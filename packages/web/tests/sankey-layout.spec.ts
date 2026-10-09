import { test, expect } from "@playwright/test";

import { layout, type Link, type Node } from "../src/lib/charts/sankeyLayout";

/**
 * Crossings in the money-flow layout, measured directly.
 *
 * No browser and no database: `layout()` is the whole pipeline, so a graph can be written down
 * in a few lines and the result measured exactly. That is what makes it affordable to check
 * hundreds of generated graphs rather than the one shape somebody happened to screenshot — and
 * every crossing found so far has been in a shape the seeded demo data does not contain.
 */

const BOX = { w: 1100, h: 520 };

// ---- measuring ---------------------------------------------------------------------------

/**
 * `sankeyLinkHorizontal` draws `M x0,y0 C xm,y0 xm,y1 x1,y1`, so both coordinates are cubics in
 * the same parameter. Evaluating them is how a link's height at a given x is known without a
 * DOM — and the x cubic has to be evaluated too, because x is not linear in t.
 */
function samples(l: { x0: number; y0: number; x1: number; y1: number }, n = 64) {
  const xm = (l.x0 + l.x1) / 2;
  const out: { x: number; y: number }[] = [];
  for (let i = 0; i <= n; i++) {
    const t = i / n;
    const u = 1 - t;
    out.push({
      x: u * u * u * l.x0 + 3 * u * u * t * xm + 3 * u * t * t * xm + t * t * t * l.x1,
      y: u * u * (1 + 2 * t) * l.y0 + t * t * (3 - 2 * t) * l.y1,
    });
  }
  return out;
}

type Ribbon = { id: string; pts: { x: number; y: number }[]; lo: number; hi: number; span: number };

function ribbons(laid: NonNullable<ReturnType<typeof layout>>): Ribbon[] {
  // Column x-positions, so a link's reach can be counted in columns rather than pixels.
  const xs = [...new Set(laid.nodes.map((n) => Math.round(n.x0)))].sort((a, b) => a - b);
  const col = (x: number) =>
    xs.reduce((best, c, i) => (Math.abs(c - x) < Math.abs(xs[best] - x) ? i : best), 0);
  return laid.links.map((l) => {
    const seg = { x0: l.source.x1, y0: l.y0, x1: l.target.x0, y1: l.y1 };
    return {
      id: `${l.source.id}→${l.target.id}`,
      pts: samples(seg),
      lo: Math.min(seg.x0, seg.x1),
      hi: Math.max(seg.x0, seg.x1),
      span: Math.abs(col(l.target.x0) - col(l.source.x0)),
    };
  });
}

/**
 * Every pair of ribbons whose vertical order flips somewhere along the x-range they share.
 *
 * Sampled rather than compared at the endpoints, because the endpoint test only works for two
 * links in the same gap — and the worst ribbons in this chart have been the ones reaching
 * across a column, which that test cannot see at all.
 */
function crossings(laid: NonNullable<ReturnType<typeof layout>>): string[] {
  const rs = ribbons(laid);
  const yAt = (r: Ribbon, x: number): number | null => {
    for (let i = 1; i < r.pts.length; i++) {
      const a = r.pts[i - 1];
      const b = r.pts[i];
      if ((x >= a.x && x <= b.x) || (x >= b.x && x <= a.x)) {
        const t = b.x === a.x ? 0 : (x - a.x) / (b.x - a.x);
        return a.y + t * (b.y - a.y);
      }
    }
    return null;
  };
  const found: string[] = [];
  for (let i = 0; i < rs.length; i++) {
    for (let j = i + 1; j < rs.length; j++) {
      const A = rs[i];
      const B = rs[j];
      const lo = Math.max(A.lo, B.lo);
      const hi = Math.min(A.hi, B.hi);
      if (hi - lo < 1) continue;
      let prev: number | null = null;
      let flipped = false;
      for (let s = 0; s <= 48; s++) {
        const x = lo + ((hi - lo) * s) / 48;
        const ya = yAt(A, x);
        const yb = yAt(B, x);
        if (ya == null || yb == null) continue;
        // A shared endpoint touches without crossing; only a real inversion counts.
        const sign = Math.abs(ya - yb) < 0.01 ? 0 : Math.sign(ya - yb);
        if (sign !== 0 && prev !== null && sign !== prev) flipped = true;
        if (sign !== 0) prev = sign;
      }
      if (flipped) found.push(`${A.id}  ×  ${B.id}`);
    }
  }
  return found;
}

// ---- graphs ------------------------------------------------------------------------------

const node = (
  id: string,
  kind: string,
  depth: number | null = null,
  label = id,
  side: "income" | "expense" | null = null,
): Node => ({
  id,
  label,
  kind,
  depth,
  category_id: null,
  root_id: null,
  root_color: null,
  // Only the kinds `SIDE_BY_KIND` cannot answer for need this — which is the crossings, and is
  // exactly why the wire states it.
  side,
  account_id: null,
});
const link = (source: string, target: string, value: number): Link => ({ source, target, value });

/**
 * The reported graph: one earner's payslip, two income branches plus an uncategorised leaf, and
 * eight expense roots with children of their own. Figures are invented (CLAUDE.md rule 3) but
 * the *shape* and the relative sizes are the ones in the screenshot, which is what decides the
 * ordering.
 */
function reportedGraph(): { nodes: Node[]; links: Link[] } {
  const nodes: Node[] = [
    node("center", "center", null, "Cash flow"),
    node("gross:1", "gross", null, "Kaimahi — gross pay"),
    node("ded:paye", "deduction", null, "PAYE"),
    node("ded:sl", "deduction", null, "Student loan"),
    node("ded:acc", "deduction", null, "ACC levy"),
    node("acct:9", "account", null, "Student loan"),
    node("in:salary", "income", 0, "Salary"),
    node("in:flat", "income", 0, "Flatmate income"),
    node("in:unc", "income", 0, "Uncategorised"),
    node("in:main", "income", 1, "Main salary"),
    node("in:tutor", "income", 1, "Tutoring"),
    node("in:flatA", "income", 1, "Flatmate A"),
    node("in:flatB", "income", 1, "Flatmate B"),
    node("savings", "savings", null, "Savings"),
    node("out:interest", "expense", 0, "Interest charged"),
    node("out:life", "expense", 0, "Lifestyle"),
    node("out:house", "expense", 0, "Household"),
    node("out:food", "expense", 0, "Food"),
    node("out:unc", "expense", 0, "Uncategorised"),
    node("out:prof", "expense", 0, "Professional services"),
    node("out:housing", "expense", 0, "Housing"),
    node("out:air", "expense", 1, "Air transport"),
    node("out:events", "expense", 1, "Events and tickets"),
    node("out:elec", "expense", 1, "Electronics"),
    node("out:ins", "expense", 1, "Insurance"),
    node("out:super", "expense", 1, "Supermarkets"),
    node("out:auto", "expense", 1, "Automotive"),
    node("out:power", "expense", 1, "Electricity"),
    node("out:fabric", "expense", 1, "Fabric and sewing"),
    node("out:council", "expense", 1, "Local government"),
  ];
  const links: Link[] = [
    link("gross:1", "ded:paye", 20_783_32),
    link("gross:1", "ded:sl", 8_048_56),
    link("gross:1", "ded:acc", 1_420_14),
    link("gross:1", "in:main", 51_899_27),
    link("ded:sl", "acct:9", 8_048_56),
    link("in:main", "in:salary", 77_498_36),
    link("in:tutor", "in:salary", 9_375_75),
    link("in:salary", "center", 89_000_06),
    link("in:flatA", "in:flat", 12_012_02),
    link("in:flatB", "in:flat", 8_234_76),
    link("in:flat", "center", 20_246_78),
    link("in:unc", "center", 3_827_16),
    link("center", "savings", 29_033_20),
    link("center", "out:interest", 19_447_89),
    link("center", "out:life", 15_771_50),
    link("center", "out:house", 12_602_81),
    link("center", "out:food", 8_837_09),
    link("center", "out:unc", 5_627_26),
    link("center", "out:prof", 3_671_91),
    link("center", "out:housing", 1_158_69),
    link("out:life", "out:air", 6_293_00),
    link("out:life", "out:events", 2_108_08),
    link("out:house", "out:elec", 4_299_28),
    link("out:house", "out:ins", 1_946_75),
    link("out:food", "out:super", 8_214_71),
    link("out:interest", "out:auto", 1_665_00),
    link("out:prof", "out:power", 5_159_38),
    link("out:prof", "out:fabric", 948_83),
    link("out:housing", "out:council", 1_158_69),
  ];
  return { nodes, links };
}

function laidOut(g: { nodes: Node[]; links: Link[] }, box = BOX) {
  const laid = layout(g.nodes, g.links, box.w, box.h);
  expect(laid, "the graph laid out to nothing").not.toBeNull();
  return laid!;
}

/** How wide a ribbon has to be before a crossing of it is something a reader can see. */
const VISIBLE_PX = 1;

/** Ribbons wide enough to read, by `source→target` key. */
function visible(laid: NonNullable<ReturnType<typeof layout>>): Set<string> {
  const out = new Set<string>();
  for (const l of laid.links) if (l.width >= VISIBLE_PX) out.add(`${l.source.id}→${l.target.id}`);
  return out;
}

/** How far a link reaches, in columns. Exactly 1 is the only healthy answer. */
function spans(laid: NonNullable<ReturnType<typeof layout>>): number[] {
  const xs = [...new Set(laid.nodes.map((n) => Math.round(n.x0)))].sort((a, b) => a - b);
  const col = (x: number) => xs.indexOf(Math.round(x));
  return laid.links.map((l) => col(l.target.x0) - col(l.source.x0));
}

const payslipKinds = ["gross", "deduction", "account"];

/** Whether a ribbon belongs to the payslip layer, following a routed chain back to its start. */
function isPayslip(l: { source: { kind: string; id: string }; target: { kind: string }; origin?: { source: string } }) {
  if (payslipKinds.includes(l.source.kind) || payslipKinds.includes(l.target.kind)) return true;
  return (l.origin?.source ?? l.source.id).startsWith("gross:");
}

test("the reported graph draws no crossing ribbons", () => {
  const found = crossings(laidOut(reportedGraph()));
  expect(found, `${found.length} crossing(s):\n  ${found.join("\n  ")}`).toEqual([]);
});

test("the reported graph stays uncrossed at every width the chart is drawn at", () => {
  // `fitToWidth` drops category levels as the box narrows, which changes the column arithmetic
  // — and the column arithmetic is where this has gone wrong twice. A card in the dashboard, a
  // phone, and the expand overlay are all real sizes.
  for (const w of [402, 560, 700, 820, 1100, 1440, 1900]) {
    for (const h of [320, 440, 520, 800]) {
      const laid = layout(reportedGraph().nodes, reportedGraph().links, w, h);
      if (!laid) continue;
      const found = crossings(laid);
      expect(found, `at ${w}x${h}: ${found.length} crossing(s):\n  ${found.join("\n  ")}`).toEqual([]);
    }
  }
});

/**
 * The invariant everything else rests on, and the one whose breach is invisible in a
 * screenshot until you notice the chart has lost a column.
 *
 * d3 sizes the chart at the longest path and *clamps* anything past it, so a layering that
 * asks for more columns than a path walks gets two of them stacked into one — and every link
 * between those two is then drawn inside a single column at zero length, with the ribbons
 * around it free to cross. A link reaching *across* a column is the same fault from the other
 * side: it is what makes the layering longer than the path in the first place.
 */
test("every link reaches exactly one column", () => {
  const bad: string[] = [];
  for (let seed = 1; seed <= 400; seed++) {
    const g = generate(seed);
    const laid = layout(g.nodes, g.links, BOX.w, BOX.h);
    if (!laid) continue;
    spans(laid).forEach((s, i) => {
      if (s !== 1) {
        const l = laid.links[i];
        bad.push(`seed ${seed}: ${l.source.id} → ${l.target.id} reaches ${s} column(s)`);
      }
    });
  }
  expect(bad, `${bad.length} misplaced link(s):\n  ${bad.slice(0, 10).join("\n  ")}`).toEqual([]);
});

/**
 * The guarantee, stated as the class of graph it actually holds for.
 *
 * `outwardOrder` makes the category tree planar and routing makes the payslip layer a chain of
 * single-column hops, so a household with one payslip has no crossings at all — which is every
 * household with one earner, and the shape this was reported on.
 *
 * Two payslips is a different graph. Each one's flows split into a band of deduction sinks,
 * which must sit above the income they were taken from, and a take-home that lands in that
 * income — so with two earners one's sinks and the other's take-home are forced to interleave,
 * whatever order the columns are in. There is no planar drawing to find. What is checked is
 * that the damage stays there: see the test below.
 */
test("a household with one payslip draws no visible crossing", () => {
  let checked = 0;
  const bad: string[] = [];
  for (let seed = 1; seed <= 400; seed++) {
    const g = generate(seed);
    const laid = layout(g.nodes, g.links, BOX.w, BOX.h);
    if (!laid) continue;
    if (laid.nodes.filter((n) => n.kind === "gross").length > 1) continue;
    checked++;
    const wide = visible(laid);
    const found = crossings(laid).filter((f) => f.split("  ×  ").every((k) => wide.has(k)));
    if (found.length) bad.push(`seed ${seed}: ${found[0]}`);
  }
  expect(checked, "the generator produced no single-payslip graphs to check").toBeGreaterThan(100);
  expect(bad, `${bad.length}/${checked} crossed:\n  ${bad.slice(0, 10).join("\n  ")}`).toEqual([]);
});

test("nothing outside the payslip layer ever crosses", () => {
  const bad: string[] = [];
  for (let seed = 1; seed <= 400; seed++) {
    const g = generate(seed);
    const laid = layout(g.nodes, g.links, BOX.w, BOX.h);
    if (!laid) continue;
    const wide = visible(laid);
    const payslip = new Set(
      laid.links.filter((l) => isPayslip(l)).map((l) => `${l.source.id}→${l.target.id}`),
    );
    for (const f of crossings(laid)) {
      const ends = f.split("  ×  ");
      if (!ends.every((k) => wide.has(k))) continue;
      // Both ribbons must be payslip ones for this to be the known, unavoidable case.
      if (!ends.every((k) => payslip.has(k))) bad.push(`seed ${seed}: ${f}`);
    }
  }
  expect(bad, `${bad.length} crossing(s) outside the payslip layer:\n  ${bad.slice(0, 10).join("\n  ")}`).toEqual([]);
});

// ---- generated graphs --------------------------------------------------------------------

/** Deterministic PRNG, so a failure names a seed that reproduces it exactly. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

/**
 * A graph with the shapes this app actually produces: an income tree and an expense tree either
 * side of the hub, optionally a payslip layer, optionally the surplus node, and values spanning
 * the range real categories do.
 */
function generate(seed: number): { nodes: Node[]; links: Link[]; seed: number } {
  const r = rng(seed);
  const int = (lo: number, hi: number) => lo + Math.floor(r() * (hi - lo + 1));
  const money = () => Math.round(10 ** (2 + r() * 5));
  const nodes: Node[] = [node("center", "center", null, "Cash flow")];
  const links: Link[] = [];

  const sub = (parent: string, kind: "income" | "expense", level: number, budget: number) => {
    const n = int(0, level >= 2 ? 0 : 3);
    for (let i = 0; i < n; i++) {
      const id = `${parent}.${i}`;
      nodes.push(node(id, kind, level, id));
      const v = Math.max(1, Math.round(budget / (n + r())));
      links.push(kind === "income" ? link(id, parent, v) : link(parent, id, v));
      sub(id, kind, level + 1, v);
    }
  };

  let incomeTotal = 0;
  for (let i = 0; i < int(1, 5); i++) {
    const id = `in:${i}`;
    const v = money() * int(1, 40);
    nodes.push(node(id, "income", 0, id));
    links.push(link(id, "center", v));
    sub(id, "income", 1, v);
    incomeTotal += v;
  }
  let expenseTotal = 0;
  for (let i = 0; i < int(1, 8); i++) {
    const id = `out:${i}`;
    const v = money() * int(1, 30);
    nodes.push(node(id, "expense", 0, id));
    links.push(link("center", id, v));
    sub(id, "expense", 1, v);
    expenseTotal += v;
  }
  // Perimeter crossings: an account the household moved cash to or from, level with the
  // top-level categories on whichever side the money went. They fan out into nothing, so they
  // widen no column — but they do share one, which is what the folding scale has to measure
  // against and what the hub's ordering has to band.
  for (let i = 0; i < int(0, 2); i++) {
    const id = `acct:${i}`;
    const v = money() * int(1, 20);
    nodes.push(node(id, "crossing", 0, id, "expense"));
    links.push(link("center", id, v));
    expenseTotal += v;
  }
  for (let i = 0; i < int(0, 2); i++) {
    const id = `acct:in${i}`;
    const v = money() * int(1, 20);
    nodes.push(node(id, "crossing", 0, id, "income"));
    links.push(link(id, "center", v));
    incomeTotal += v;
  }
  // Both arms, so the generated set covers a month that came out short as well as one that
  // didn't — the deficit is a *source* into the hub where savings is a sink out of it, and
  // getting that backwards is a whole column's worth of wrong.
  if (incomeTotal > expenseTotal) {
    nodes.push(node("savings", "savings", null, "Savings"));
    links.push(link("center", "savings", incomeTotal - expenseTotal));
  } else if (expenseTotal > incomeTotal) {
    nodes.push(node("deficit", "deficit", null, "Drawn from reserves"));
    links.push(link("deficit", "center", expenseTotal - incomeTotal));
  }
  if (r() < 0.6) {
    for (let e = 0; e < int(1, 2); e++) {
      const g = `gross:${e}`;
      const takeHome = Math.round(money() * int(10, 60));
      const sinks = ["paye", "acc", "sl", "kiwisaver"].slice(0, int(1, 4));
      nodes.push(node(g, "gross", null, g));
      // Which income node the take-home lands on is exactly the variable that decides whether
      // its ribbon has to reach across a column.
      const leaves = nodes.filter((n) => n.kind === "income");
      links.push(link(g, leaves[int(0, leaves.length - 1)].id, takeHome));
      for (const s of sinks) {
        const id = `ded:${s}:${e}`;
        nodes.push(node(id, "deduction", null, id));
        const v = Math.round(takeHome * (0.05 + r() * 0.3));
        links.push(link(g, id, v));
        if (r() < 0.4) {
          const d = `acct:${s}:${e}`;
          nodes.push(node(d, "account", null, d));
          links.push(link(id, d, v));
        }
      }
    }
  }
  return { nodes, links, seed };
}

// ---- the cash basis's own shapes -------------------------------------------
// A crossing and the surplus/deficit are placed by `(side, level)` like a category, which is
// the whole reason the wire states a side. These pin the arithmetic directly, where the
// generated tests above pin only that nothing ends up in the wrong column relative to its
// neighbours.

/** A graph with one root per side, plus whatever extra nodes a case needs. */
function withExtras(extra: { nodes: Node[]; links: Link[] }) {
  return {
    nodes: [
      node("center", "center", null, "Cash flow"),
      node("in:1", "income", 0, "Salary"),
      node("out:1", "expense", 0, "Lifestyle"),
      ...extra.nodes,
    ],
    links: [link("in:1", "center", 500_000), link("center", "out:1", 400_000), ...extra.links],
  };
}

const columnOfId = (laid: NonNullable<ReturnType<typeof layout>>, id: string) => {
  const xs = [...new Set(laid.nodes.map((n) => Math.round(n.x0)))].sort((a, b) => a - b);
  const n = laid.nodes.find((n) => n.id === id);
  expect(n, `no node ${id}`).toBeTruthy();
  return xs.indexOf(Math.round(n!.x0));
};

test("a crossing sits where a top-level category on its own side sits", () => {
  const laid = laidOut(
    withExtras({
      nodes: [
        node("acct:10", "crossing", 0, "Home Mortgage", "expense"),
        node("acct:21", "crossing", 0, "Sharesies", "income"),
      ],
      links: [link("center", "acct:10", 44_530), link("acct:21", "center", 114_269_63)],
    }),
  );
  expect(columnOfId(laid, "acct:10")).toBe(columnOfId(laid, "out:1"));
  expect(columnOfId(laid, "acct:21")).toBe(columnOfId(laid, "in:1"));
});

test("a deficit is a source left of the hub, where the surplus is a sink right of it", () => {
  const deficit = laidOut(
    withExtras({
      nodes: [node("deficit", "deficit", null, "Drawn from reserves")],
      // The extras' own links already leave income above expense, so this is the graph the
      // backend would actually emit alongside them.
      links: [link("deficit", "center", 20_000)],
    }),
  );
  expect(columnOfId(deficit, "deficit")).toBe(columnOfId(deficit, "in:1"));
  expect(spans(deficit).every((s) => s === 1)).toBe(true);

  const surplus = laidOut(
    withExtras({
      nodes: [node("savings", "savings", null, "Savings")],
      links: [link("center", "savings", 100_000)],
    }),
  );
  expect(columnOfId(surplus, "savings")).toBe(columnOfId(surplus, "out:1"));
});

test("folding measures a slice against the whole of the busiest column", () => {
  // An expense side that is mostly one unfoldable crossing, plus a tail of hairlines. Measured
  // against the folding candidates alone the tail reads as a large share of a small total and
  // survives; measured against the column it actually shares, it folds.
  const nodes: Node[] = [node("center", "center", null, "Cash flow"), node("in:1", "income", 0, "Salary")];
  const links: Link[] = [link("in:1", "center", 500_000_00)];
  nodes.push(node("acct:10", "crossing", 0, "Home Mortgage", "expense"));
  links.push(link("center", "acct:10", 499_000_00));
  for (let i = 0; i < 4; i++) {
    nodes.push(node(`out:${i}`, "expense", 0, `Hairline ${i}`));
    links.push(link("center", `out:${i}`, 250_00));
  }
  const laid = laidOut({ nodes, links });
  expect(laid.nodes.some((n) => n.id.startsWith("other:expense:"))).toBe(true);
});

test("the hub's crossings sit below its categories", () => {
  // Ordering within a sibling group is free, so a crossing is banded to the bottom of the fan:
  // interleaved by size, a mortgage repayment reads as one more spending category.
  const laid = laidOut(
    withExtras({
      nodes: [
        node("acct:10", "crossing", 0, "Home Mortgage", "expense"),
        node("out:2", "expense", 0, "Food"),
      ],
      // Larger than the category it must still sit below.
      links: [link("center", "acct:10", 300_000), link("center", "out:2", 50_000)],
    }),
  );
  const y = (id: string) => laid.nodes.find((n) => n.id === id)!.y0;
  expect(y("acct:10")).toBeGreaterThan(y("out:1"));
  expect(y("acct:10")).toBeGreaterThan(y("out:2"));
});

