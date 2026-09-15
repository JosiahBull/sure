/**
 * The money-flow chart's layout: which column every node belongs in, what order the nodes sit
 * in within it, and the d3-sankey run that turns those two answers into pixels.
 *
 * Extracted from Sankey.svelte, which now only draws what this returns. Not for tidiness: this
 * is where the crossings live, and a crossing is a property of *positions*, so the only honest
 * test is one that runs the real layout and measures the result. In the component that needed a
 * browser, a seeded database and a screenshot; here it is a function call, which is what lets
 * tests/sankey-layout.spec.ts throw several hundred generated graphs at it and count the
 * crossings in every one.
 */
import { sankey } from "d3-sankey";

export interface Node {
  id: string;
  label: string;
  kind: string;
  /** 0-based level within its own side; null for the hub and savings. */
  depth?: number | null;
  category_id?: number | null;
  root_id?: number | null;
  root_color?: string | null;
}
export interface Link {
  source: string;
  target: string;
  value: number;
}

export const NODE_W = 12;
const MAX_NODE_PAD = 18;
const MIN_NODE_PAD = 4;
/** Node padding may not eat more than this share of the box, however many nodes there are. */
const MAX_PAD_RATIO = 0.4;
const MARGIN_X = 4;
/** Room above the graph for the hub's label, which sits over it rather than beside it. */
const MARGIN_TOP = 30;
const MARGIN_BOTTOM = 14;

/**
 * A routing waypoint: where a link that would otherwise reach across columns is bent.
 *
 * `column` is authoritative — `columnOf` returns it as-is — which leaves the node's `level`
 * free to carry something else, and it does: the colour. A waypoint is invisible, so it exists
 * only to be a place for a ribbon to pass through and to take the shading of the node the chain
 * is heading for.
 */
export type Via = {
  column: number;
  /** Which way the chain is travelling, which is what tells the ordering pass its parent. */
  side: "income" | "expense";
  /** The kind of the node the chain is heading for, so the ribbon keeps one colour through it. */
  colorKind: string;
};

export type Placed = Omit<Node, "depth"> & {
  level: number;
  /** True for a synthesised "Other" node — see {@link foldHairlines}. */
  aggregate?: boolean;
  /** Set on a routing waypoint — see {@link routeSpans}. */
  via?: Via;
};
export const placed = (n: Node): Placed => {
  const { depth, ...rest } = n;
  return { ...rest, level: depth ?? 0 };
};

export type Cols = { income: number; center: number; expenseBase: number; total: number };

/**
 * How many columns each side needs, from the nodes actually being laid out.
 *
 * **The count has to be one a path can actually walk**, not merely one the levels imply. d3
 * sizes the chart at `max(node.depth) + 1` columns — the longest path in links — and clamps
 * anything past that, silently stacking two columns into one and drawing every link between
 * them at zero length inside a single column. So a column this asks for and no path traverses
 * is not slack; it is the last column of the chart collapsing onto its neighbour.
 *
 * Every column below is path-backed by construction except one. A category at level k reaches
 * the hub in exactly k+1 hops, so both sides' widths are what their deepest chain walks. The
 * exception is the gross column: it is only walked if some gross node has a link that carries on
 * past its deduction sinks — a take-home. Deductions are terminal, so a payslip whose take-home
 * leaf was folded away (or rounded to nothing) reaches column 1 and stops, and reserving column
 * 0 for it costs the expense side its deepest column. Hence `pre` asks the links, not the nodes.
 */
function columnsOf(live: Placed[], links: Link[]): Cols {
  const byId = new Map(live.map((n) => [n.id, n]));
  let income = 0;
  let expense = 0;
  let anyGross = false;
  let anyDeduction = false;
  for (const n of live) {
    if (n.kind === "income") income = Math.max(income, n.level + 1);
    else if (n.kind === "expense") expense = Math.max(expense, n.level + 1);
    else if (n.kind === "savings") expense = Math.max(expense, 1);
    else if (n.kind === "gross") anyGross = true;
    else if (n.kind === "deduction") anyDeduction = true;
  }
  const walksOn = links.some(
    (l) => byId.get(l.source)?.kind === "gross" && byId.get(l.target)?.kind !== "deduction",
  );
  if (anyGross && walksOn) income += 1;
  // A payslip with no income categories at all still needs somewhere left of the hub to put its
  // gross node and its sinks, or they land on the spine and their links have nowhere to go.
  if (income === 0 && (anyGross || anyDeduction)) income = 1;
  return { income, center: income, expenseBase: income + 1, total: income + 1 + expense };
}

/**
 * Place nodes by what they *mean* rather than by d3's default packing: income fans out
 * leftwards from the hub by depth, expense rightwards.
 *
 * `sankeyJustify` — the default, and what this used before — is
 * `node.sourceLinks.length ? node.depth : n - 1`, and d3 sets `depth` to the longest path
 * *ending* at a node, which is 0 for any source. A childless top-level income category is
 * a source, so it would land in the far-left column, two columns from the hub it feeds
 * and alongside some other branch's grandchild.
 *
 * d3 clamps this into [0, x-1] where `x = max(node.depth) + 1`, and *throws* if any
 * column in that range ends up empty (`computeNodeBreadths` maps over a sparse array).
 * Both are safe here: the longest path is (deepest income chain) → hub → (deepest expense
 * chain), which is exactly `total` columns, and every level of a chain is occupied
 * because a node at depth d always has its parent at d-1 in the graph.
 */
function columnOf(n: Placed, c: Cols): number {
  switch (n.kind) {
    case "income":
      return c.income - 1 - n.level;
    case "expense":
      return c.expenseBase + n.level;
    case "savings":
      return c.expenseBase;
    // The reconstructed payslips: gross pay on the far left, its deduction sinks pinned
    // into the first income column (their natural d3 depth is 1, which is only the same
    // thing while exactly one category level is drawn).
    case "gross":
      return 0;
    case "deduction":
      return Math.min(1, c.center);
    // The account a deduction was routed into — a terminal sink, one hop past its sink.
    // Immediately right of the deductions, *not* on the spine: falling through to the hub's
    // column (which is what `default` used to do for it) made the ribbon span every income
    // column in between, so it cut across the whole income fan to reach a node the width of
    // a hairline. Clamped to the hub because a graph with no income categories has nothing
    // between the two, and a column past the hub would put a payslip sink among the
    // expenses.
    case "destination":
      return Math.min(Math.min(1, c.center) + 1, c.total - 1);
    // A routing waypoint carries its column outright — see `routeSpans`.
    case "via":
      return n.via!.column;
    // `kind` is a plain string on the wire, so the hub — and anything a newer backend
    // adds — sits on the spine rather than breaking the layout.
    default:
      return c.center;
  }
}

/**
 * Vertical order for every column, computed before the layout runs.
 *
 * Two goals that look opposed and are not. Sorting each column purely by value reads
 * beautifully — every column ranks top to bottom by size — but it scatters each parent's
 * children across the column by their own magnitude, so a big leaf of a small branch sits
 * above a small leaf of a big one and its ribbon crosses the whole diagram to reach its
 * parent. Measured on this household's own graph that is 23-35 crossings; d3's own ordering
 * has none, but it minimises crossings *only*, so it puts a $700 category above an $18,000 one
 * whenever that shortens a ribbon and the eye cannot rank anything by position.
 *
 * The resolution is that this graph is a **tree**: every category has exactly one hub-ward
 * link, income flowing leaf→parent→hub and expense hub→parent→leaf. For a layered tree a
 * planar order always exists — group each column by parent, keep the groups in their parents'
 * order — and *within* a sibling group the order is free, so value ordering there costs
 * nothing. Sweeping outward from the hub and applying both rules gives zero crossings with
 * size ordering everywhere it is achievable.
 *
 * Why zero falls out: for two links p1→c1 and p2→c2 in one gap, either p1 and p2 are the same
 * node (siblings, consistently ordered) or they are not, in which case every child of the
 * earlier parent precedes every child of the later one. Neither case can invert.
 *
 * **That argument covers one gap at a time, so it says nothing about a link that spans two.**
 * One shape does: a take-home flowing to an income leaf that is not at the deepest level —
 * an uncategorised one sits at level 0, beside the hub, while the gross node it comes from is
 * pinned to column 0. Its ribbon crosses whatever lies in the column it passes through, and no
 * ordering of the columns at either end can help. Measured on the payslip graph below, that is
 * one crossing at the two widths where `fitToWidth` leaves exactly two income levels, and none
 * at the widths either side of them. Removing it means giving that link a node to land on
 * halfway, which is a question for whatever builds the graph rather than for this ordering.
 *
 * What is given up is *global* size order in the outer columns — a big grandchild of a small
 * root sits below a small grandchild of a big root. That is not a tuning choice: any
 * zero-crossing order must group by parent, so it is the price of the crossings going away.
 *
 * This has to be a pre-pass rather than a comparator. `computeNodeLayers` sorts each column
 * before any node has a y-position, so a parent's placement is unknowable from inside a
 * comparator. It is safe to decide the order here because supplying a comparator makes the
 * array order final: both relaxation directions skip their `column.sort(ascendingBreadth)`,
 * and `resolveCollisions` only pushes nodes apart in array order, never reorders them.
 */
function outwardOrder(live: Placed[], links: Link[], cols: Cols): Map<string, number> {
  const byId = new Map(live.map((n) => [n.id, n]));
  const columnOfId = new Map(live.map((n) => [n.id, columnOf(n, cols)]));

  // d3's own `computeNodeValues`, which has not run yet: a node is as tall as the larger of
  // what flows in and what flows out.
  const inSum = new Map<string, number>();
  const outSum = new Map<string, number>();
  for (const l of links) {
    outSum.set(l.source, (outSum.get(l.source) ?? 0) + l.value);
    inSum.set(l.target, (inSum.get(l.target) ?? 0) + l.value);
  }
  const valueOf = (id: string) => Math.max(inSum.get(id) ?? 0, outSum.get(id) ?? 0);
  const bigFirst = (a: string, b: string) => valueOf(b) - valueOf(a) || (a < b ? -1 : 1);

  // Hub-rooted child lists, using the same orientation trick `foldHairlines` uses: on the
  // income side the source is the child, on the expense side the target is. `center→savings`
  // lands in the expense case and makes savings an ordinary hub child.
  //
  // A gross node's links are skipped entirely. They point *outward* from column 0 rather than
  // hub-ward, so they are not tree edges — and following the `gross→center` fallback (used
  // when a take-home leaf rounded away) would make the hub a child of a payslip.
  const kids = new Map<string, string[]>();
  const inward = new Map<string, number>();
  for (const l of links) {
    const s = byId.get(l.source);
    const t = byId.get(l.target);
    if (!s || !t || s.kind === "gross") continue;
    // Which end is the child is "which end is further from the hub", and for a waypoint that
    // is the side its chain is travelling on rather than its own kind.
    const towardHub = (n: Placed) =>
      n.kind === "income" || (n.kind === "via" && n.via!.side === "income");
    const [child, parent] = towardHub(s) ? [s, t] : [t, s];
    kids.set(parent.id, [...(kids.get(parent.id) ?? []), child.id]);
    inward.set(child.id, l.value);
  }
  for (const list of kids.values()) {
    list.sort((a, b) => (inward.get(b) ?? 0) - (inward.get(a) ?? 0) || (a < b ? -1 : 1));
  }

  const inColumn = new Map<number, string[]>();
  for (const n of live) {
    const c = columnOfId.get(n.id)!;
    inColumn.set(c, [...(inColumn.get(c) ?? []), n.id]);
  }

  /**
   * The deductions' own top-to-bottom order, decided here rather than inside `layColumn`.
   *
   * A destination has to be ordered by the deduction it came from, and the two sit in
   * different columns — laid on the *same* leftward sweep, with the destination's column
   * reached first, so by the time the deductions are ordered it is already too late to ask.
   * Both columns read this instead, which is also what keeps the two bands in step: the
   * deduction that is second from the top has its account second from the top.
   */
  // The gross nodes' own order, for the same reason and with the same problem: their column is
  // the last one the income sweep reaches, so nothing laid before it can ask where they ended up.
  // Biggest payslip first.
  const grossRank = new Map<string, number>();
  live
    .filter((n) => n.kind === "gross")
    .map((n) => n.id)
    .sort(bigFirst)
    .forEach((id, i) => grossRank.set(id, i));

  /**
   * The deductions, grouped by the payslip they were taken from and then by size.
   *
   * Grouping first is the rule the category tree already follows, and it matters for the same
   * reason: ordered by size alone, two earners' sinks interleave, and each earner's ribbons then
   * have to thread past the other's to reach them. A household's sinks may be *shared* between
   * earners — one "PAYE" fed by everyone — and no order untangles that, so a shared sink takes
   * the rank of its earliest contributor and the rest is as good as it gets.
   */
  const dedParent = new Map<string, number>();
  for (const l of links) {
    if (byId.get(l.source)?.kind === "gross" && byId.get(l.target)?.kind === "deduction") {
      const r = grossRank.get(l.source) ?? 0;
      dedParent.set(l.target, Math.min(dedParent.get(l.target) ?? r, r));
    }
  }
  const deductionRank = new Map<string, number>();
  live
    .filter((n) => n.kind === "deduction")
    .map((n) => n.id)
    .sort(
      (a, b) => (dedParent.get(a) ?? Infinity) - (dedParent.get(b) ?? Infinity) || bigFirst(a, b),
    )
    .forEach((id, i) => deductionRank.set(id, i));

  /** `dest:<account>` → the rank of the `ded:*` it hangs off, for the ordering above. */
  const destParentRank = new Map<string, number>();
  for (const l of links) {
    if (byId.get(l.source)?.kind === "deduction" && byId.get(l.target)?.kind === "destination") {
      destParentRank.set(l.target, deductionRank.get(l.source) ?? 0);
    }
  }

  const rank = new Map<string, number>();
  function layColumn(c: number, prevIds: string[]): string[] {
    const here = inColumn.get(c) ?? [];
    const hereSet = new Set(here);
    const out: string[] = [];
    const seen = new Set<string>();
    const take = (ids: string[]) => {
      for (const id of ids) {
        if (hereSet.has(id) && !seen.has(id)) {
          seen.add(id);
          out.push(id);
        }
      }
    };
    // The payslip layer rides above the income it is taken out of, in every column it touches:
    // the gross nodes, then their sinks one column right, then the accounts those were routed
    // into one right again. All three bands have to agree, because a gross node left in the
    // middle of its column sends its ribbons sweeping up across the whole income tree to reach
    // sinks pinned to the top of the next one.
    take(
      here
        .filter((id) => byId.get(id)!.kind === "gross")
        .sort((a, b) => grossRank.get(a)! - grossRank.get(b)!),
    );
    // The deductions ride above the income they are taken out of, wherever that column lands —
    // `columnOf` puts them in the deepest income column normally, but in the hub's own column
    // when there are no income categories at all.
    take(
      here
        .filter((id) => byId.get(id)!.kind === "deduction")
        .sort((a, b) => deductionRank.get(a)! - deductionRank.get(b)!),
    );
    // And their destination accounts ride directly above the income in the *next* column, in
    // the same order, so the payslip reads as one band across the top rather than a ribbon
    // dropped through the middle of the income fan to reach a sink at the bottom.
    take(
      here
        .filter((id) => byId.get(id)!.kind === "destination")
        .sort(
          (a, b) =>
            (destParentRank.get(a) ?? Infinity) - (destParentRank.get(b) ?? Infinity) ||
            (a < b ? -1 : 1),
        ),
    );
    // The tree: each parent's children, in the parents' own order.
    for (const p of prevIds) take(kids.get(p) ?? []);
    // The hub, the gross nodes, and anything with no hub-ward edge. `foldHairlines` takes a
    // folded node's whole subtree with it, so this is a safety net rather than a live path.
    take([...here].sort(bigFirst));
    out.forEach((id, i) => rank.set(id, i));
    return out;
  }

  const hub = layColumn(cols.center, []);
  for (const dir of [1, -1]) {
    let cur = hub;
    for (let c = cols.center + dir; c >= 0 && c < cols.total; c += dir) {
      cur = layColumn(c, cur);
    }
  }
  return rank;
}

/**
 * Padding shrinks as the node count grows, so a deep graph doesn't spend most of its
 * height on gaps. Ported from the previous app's `#calculateNodePadding`.
 */
function nodePadding(count: number, available: number): number {
  const dynamic = Math.floor((available * MAX_PAD_RATIO) / Math.max(count - 1, 1));
  return Math.max(MIN_NODE_PAD, Math.min(MAX_NODE_PAD, dynamic));
}

/** Room a two-line label needs before a column is worth drawing at all. */
const MIN_PITCH = 104;
export const isCatKind = (kind: string) => kind === "income" || kind === "expense";
export const pitchOf = (cols: Cols, width: number) =>
  cols.total > 1 ? (width - 2 * MARGIN_X - NODE_W) / (cols.total - 1) : Infinity;

/**
 * The deepest category level this width can actually show, and the nodes that survive it.
 *
 * Seven columns in a phone-width card is a pile of overlapping labels, not a chart, so
 * levels are dropped from the leaf end until each column has room to be read. Nothing is
 * recomputed to do it: every node's hub-ward link already carries its whole subtree, so a
 * node whose children are dropped simply becomes a leaf holding their total. The Expand
 * view, being far wider, keeps all of them.
 */
function fitToWidth(all: Placed[], links: Link[], width: number): Placed[] {
  const keep = (cap: number) => all.filter((n) => !isCatKind(n.kind) || n.level <= cap);
  let cap = all.reduce((m, n) => (isCatKind(n.kind) ? Math.max(m, n.level) : m), 0);
  while (cap > 0 && pitchOf(columnsOf(keep(cap), links), width) < MIN_PITCH) cap--;
  return keep(cap);
}

/** Below this many pixels tall a slice can't be told apart from the one above it. */
const MIN_VISIBLE_PX = 2;

/**
 * Gather each side's hairline categories into a single "Other" node.
 *
 * A sankey draws value as height, so over a long window — where every category that ever
 * saw a dollar earns a slot — the tail collapses into a stack of 1px slivers that crowds
 * out the categories worth reading. Their own labels can't fit either, so the few that do
 * get drawn end up sitting over the stack.
 *
 * The threshold is a pixel budget rather than a fixed percentage, so the roomier expand
 * view folds less than the card does — the same bargain it makes with depth. The scale is
 * the *busiest column's* total, not the side's: d3 derives one height-per-unit for the
 * whole diagram from whichever column sums highest, and the expense roots share their
 * column with `savings`. Measuring against the side alone reads every expense slice as
 * over twice as tall as it lands.
 *
 * Only genuinely undrawable slices go — a 4px node is small but real, and folding those
 * would bury ordinary categories. Folding also needs at least two members: replacing one
 * node with an "Other" standing for exactly it would only lose its name.
 */
function foldHairlines(nodes: Placed[], links: Link[], available: number): {
  nodes: Placed[];
  links: Link[];
} {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  // Each category's hub-ward link — income flows leaf→hub, expense hub→leaf — which is
  // both its own value and the edge naming its parent.
  const inward = new Map<string, { parent: string; value: number }>();
  // Grouped by side as well as parent: the hub is the parent of *both* sides' top-level
  // categories, so keying on it alone would pool income roots with expense roots and
  // measure them against the wrong total.
  const siblings = new Map<string, string[]>();
  const groupKey = (side: string, parent: string) => `${side}:${parent}`;
  for (const l of links) {
    const s = byId.get(l.source);
    const t = byId.get(l.target);
    if (!s || !t) continue;
    // The pre-income layer never folds: a gross→category link would otherwise be read
    // backwards as the category's hub-ward edge (clobbering its real value), and ACC
    // vanishing into "Other (2)" is exactly what an itemised layer must not do.
    if (s.kind === "gross" || t.kind === "deduction") continue;
    // Which end is the child is "which end is further from the hub", and for a waypoint that
    // is the side its chain is travelling on rather than its own kind.
    const towardHub = (n: Placed) =>
      n.kind === "income" || (n.kind === "via" && n.via!.side === "income");
    const [child, parent] = towardHub(s) ? [s, t] : [t, s];
    if (!isCatKind(child.kind)) continue;
    inward.set(child.id, { parent: parent.id, value: l.value });
    const key = groupKey(child.kind, parent.id);
    siblings.set(key, [...(siblings.get(key) ?? []), child.id]);
  }
  const childrenOf = (id: string) => [
    ...(siblings.get(groupKey("income", id)) ?? []),
    ...(siblings.get(groupKey("expense", id)) ?? []),
  ];
  const sideTotal = { income: 0, expense: 0 };
  for (const [id, { parent, value }] of inward) {
    if (parent === "center") sideTotal[byId.get(id)!.kind as "income" | "expense"] += value;
  }
  // The tallest column, which sets the scale for every other one. Whichever side is
  // larger is it: when income exceeds expense the shortfall reappears as `savings` in the
  // expense roots' own column, so both columns sum to the same figure.
  const scale = Math.max(sideTotal.income, sideTotal.expense);
  const floor = (scale * MIN_VISIBLE_PX) / Math.max(available, 1);

  const drop = new Set<string>();
  const extraNodes: Placed[] = [];
  const extraLinks: Link[] = [];
  for (const [key, members] of siblings) {
    const side = byId.get(members[0])!.kind as "income" | "expense";
    const parent = key.slice(side.length + 1);
    const small = members.filter((id) => inward.get(id)!.value < floor);
    if (small.length < 2) continue;
    // A folded node takes its descendants with it: they have nothing left to hang off.
    const queue = [...small];
    while (queue.length) {
      const id = queue.pop()!;
      if (drop.has(id)) continue;
      drop.add(id);
      queue.push(...childrenOf(id));
    }
    const value = small.reduce((t, id) => t + inward.get(id)!.value, 0);
    const id = `other:${side}:${parent}`;
    extraNodes.push({
      id,
      label: `Other (${small.length})`,
      kind: side,
      level: byId.get(small[0])!.level,
      category_id: null,
      root_id: null,
      root_color: null,
      aggregate: true,
    });
    extraLinks.push(
      side === "income" ? { source: id, target: parent, value } : { source: parent, target: id, value },
    );
  }
  if (!drop.size) return { nodes, links };
  // Filter the synthesised links alongside the original ones rather than appending them
  // afterwards: a small parent can be folded by its own group *after* its children were
  // folded into an "Other", which would otherwise leave that "Other" pointing at a node
  // no longer in the graph — and d3 throws on a link whose endpoint it can't resolve.
  const kept = [...nodes.filter((n) => !drop.has(n.id)), ...extraNodes];
  const ids = new Set(kept.map((n) => n.id));
  return {
    nodes: kept,
    links: [...links, ...extraLinks].filter((l) => ids.has(l.source) && ids.has(l.target)),
  };
}


/** A link once routing has run: it may be one leg of a chain that stands in for a longer one. */
export type RoutedLink = Link & {
  /**
   * The link this leg came from, when it is one. Every leg of a chain carries the same one, so
   * the drawing can treat the chain as the single flow it represents — one tooltip, one
   * highlight, one click target — while the layout sees only single-column hops.
   */
  origin?: { source: string; target: string; chain: string };
};

/**
 * Bend every link that would reach across more than one column so it passes through a waypoint
 * in each column on the way.
 *
 * **This is the fix for two problems that look unrelated and are the same one.**
 *
 * The visible one: `outwardOrder` guarantees planarity by arguing about one gap at a time, so a
 * ribbon spanning two columns is outside the argument entirely and crosses whatever happens to
 * lie in the column it passes through. No ordering of the columns at either end can help it,
 * because the trouble is in the column *between* them.
 *
 * The invisible one, and the worse of the two: d3 sizes the chart at `max(node.depth) + 1`
 * columns, where `depth` is the longest path *in links*. A layering that spans a column without
 * stopping in it needs more columns than the longest path has links — so d3 allocates too few
 * and `computeNodeLayers` **clamps** the overflow, quietly stacking two of our columns into one.
 * Every link between those two then has nowhere to go: it is drawn inside a single column, at
 * zero length, and the ribbons around it cross freely. Measured on 400 generated graphs before
 * this existed, 125 of them had at least one such link and 191 drew a crossing.
 *
 * A waypoint in every column the link passes through fixes both at once: the ordering pass can
 * place it like any other node (it is a child of whatever the chain heads for, which is what
 * `side` records), and the longest path now equals the column count, so nothing is clamped.
 *
 * The waypoints are invisible. Consecutive legs share an endpoint exactly, so the ribbon reads
 * as one — with a brief flattening where it passes through, which is what a Sankey that routes
 * long edges properly looks like.
 */
function routeSpans(
  live: Placed[],
  links: Link[],
  cols: Cols,
): { nodes: Placed[]; links: RoutedLink[] } {
  const byId = new Map(live.map((n) => [n.id, n]));
  const columnAt = new Map(live.map((n) => [n.id, columnOf(n, cols)]));
  const extra: Placed[] = [];
  const out: RoutedLink[] = [];

  for (const l of links) {
    const s = byId.get(l.source);
    const t = byId.get(l.target);
    const from = s ? columnAt.get(s.id)! : 0;
    const to = t ? columnAt.get(t.id)! : 0;
    if (!s || !t || to - from <= 1) {
      out.push(l);
      continue;
    }
    // The chain travels toward the hub on whichever side it ends up; a link that starts left of
    // the hub and finishes at or before it is an income-side one.
    const side: "income" | "expense" = to <= cols.center ? "income" : "expense";
    const chain = `${l.source}->${l.target}`;
    const origin = { source: l.source, target: l.target, chain };
    let prev = l.source;
    for (let c = from + 1; c < to; c++) {
      const id = `via:${chain}:${c}`;
      extra.push({
        id,
        label: "",
        kind: "via",
        // The colour of the node the chain is heading for, so the ribbon does not change
        // shade as it passes through. `level` is only ever read for colour on a waypoint —
        // `columnOf` uses `via.column` instead.
        level: t.level,
        category_id: null,
        root_id: t.root_id ?? null,
        root_color: t.root_color ?? null,
        via: { column: c, side, colorKind: t.kind },
      });
      out.push({ source: prev, target: id, value: l.value, origin });
      prev = id;
    }
    out.push({ source: prev, target: l.target, value: l.value, origin });
  }
  return { nodes: [...live, ...extra], links: out };
}

/** A node once d3 has placed it. `any` because d3-sankey's own types stop at the generic. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type LaidNode = any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type LaidLink = any;

export type Laid = {
  nodes: LaidNode[];
  links: LaidLink[];
  cols: Cols;
  /** Column pitch — how much room a label has between its own column and the next. */
  kx: number;
};

/**
 * Lay the graph out in a box, or `null` when there is nothing to draw.
 *
 * The whole pipeline, in the order it has to happen: drop what the width cannot show, fold the
 * hairlines, drop whatever that left unconnected, decide the columns, decide the order within
 * them, and hand all of it to d3.
 */
export function layout(
  nodes: Node[],
  links: Link[],
  boxW: number,
  boxH: number,
): Laid | null {
  if (boxW <= 0 || boxH <= 0) return null; // not measured yet
  const usable = links.filter((l) => l.value > 0);
  if (!nodes.length || !usable.length) return null;
  const available = boxH - MARGIN_TOP - MARGIN_BOTTOM;
  const depthFitted = fitToWidth(nodes.map(placed), usable, boxW);
  const withinIds = new Set(depthFitted.map((n) => n.id));
  const { nodes: within, links: kept } = foldHairlines(
    depthFitted,
    usable.filter((l) => withinIds.has(l.source) && withinIds.has(l.target)),
    available,
  );
  if (!kept.length) return null;
  // Drop any node left with no surviving link. It would lay out at zero height, and if it
  // were the only occupant of a column d3 would throw rather than merely look wrong.
  const connected = new Set<string>();
  for (const l of kept) {
    connected.add(l.source);
    connected.add(l.target);
  }
  const connectedNodes = within.filter((n) => connected.has(n.id));
  // The columns are decided before routing and unchanged by it: a waypoint's kind counts for
  // nothing in `columnsOf`, and every column it occupies is one some real node's level already
  // claimed — it is the gap between two of them that was never filled.
  const cols = columnsOf(connectedNodes, kept);
  const { nodes: live, links: routed } = routeSpans(connectedNodes, kept, cols);
  const index = new Map(live.map((n, i) => [n.id, i]));
  // d3 mutates the graph it is handed: it resolves each link's endpoints to the node objects
  // and fills their sourceLinks/targetLinks, so the input cannot be reused or shared.
  const build = () => ({
    nodes: live.map((n) => ({ ...n })),
    links: routed.map((l) => ({
      source: index.get(l.source)!,
      target: index.get(l.target)!,
      value: l.value,
      origin: l.origin,
    })),
  });
  const gen = () =>
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    (sankey() as any)
      .nodeWidth(NODE_W)
      .nodePadding(nodePadding(live.length, available))
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      .nodeAlign((n: any) => columnOf(n, cols))
      .extent([
        [MARGIN_X, MARGIN_TOP],
        [boxW - MARGIN_X, boxH - MARGIN_BOTTOM],
      ]);
  const rank = outwardOrder(live, routed, cols);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const order = (a: any, b: any) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0);
  /**
   * Where each link sits in the stack at either of its ends.
   *
   * Without this d3 decides it, and decides it *from the positions as they stand* — sorting a
   * node's links by the other end's `y0` during relaxation, repeatedly, as everything moves. The
   * order it settles on is therefore the order things were in at some point during the
   * iteration, which is not the order they finish in: a node whose position is dominated by one
   * huge flow drifts away from a thin one it also carries, and the thin one is left threaded
   * through its own siblings. That is a crossing inside a single parent's fan, which no amount
   * of getting the *node* order right can prevent.
   *
   * Ranking by the ordering pass instead makes the stack agree with the columns by construction,
   * and it is well defined precisely because routing has already guaranteed every link spans
   * exactly one column: all of a node's outgoing links land in the same column, so their targets'
   * ranks are comparable, and likewise for incoming. Supplying a comparator also stops d3
   * reordering during relaxation at all, so what is decided here is what is drawn.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const linkOrder = (a: any, b: any) => {
    const at = rank.get(a.target.id) ?? 0;
    const bt = rank.get(b.target.id) ?? 0;
    // One array holds links out of a shared source, the other links into a shared target; which
    // end to compare is whichever end differs.
    return a.source === b.source ? at - bt : (rank.get(a.source.id) ?? 0) - (rank.get(b.source.id) ?? 0);
  };
  const laid = gen().nodeSort(order).linkSort(linkOrder)(build()) as { nodes: any[]; links: any[] };
  // Column pitch — how much room a label has between its own column and the next.
  const kx = pitchOf(cols, boxW);
  return { ...laid, cols, kx: Number.isFinite(kx) ? kx : boxW };
}
