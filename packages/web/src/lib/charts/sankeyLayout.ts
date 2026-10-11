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
  /** 0-based level within its own side; null for the hub and the balance nodes. */
  depth?: number | null;
  category_id?: number | null;
  root_id?: number | null;
  root_color?: string | null;
    side?: string | null;
  /** The account a cash movement reached. */
  account_id?: number | null;
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

/** A node at a visible category level. */
export type Placed = Omit<Node, "depth"> & {
  level: number;
  /** True for a synthesised "Other" node — see {@link foldHairlines}. */
  aggregate?: boolean;
};
export const placed = (n: Node): Placed => {
  const { depth, ...rest } = n;
  return { ...rest, level: depth ?? 0 };
};

export type Cols = { income: number; center: number; expenseBase: number; total: number };

/** Each side occupies one column per category level. */
function columnsOf(live: Placed[]): Cols {
  let income = 0;
  let expense = 0;
  for (const n of live) {
    const side = sideOf(n);
    if (side === "income") income = Math.max(income, n.level + 1);
    if (side === "expense") expense = Math.max(expense, n.level + 1);
  }
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
    case "center":
      return c.center;
    default: {
      // Everything that hangs off the hub by a side and a depth, in one rule because they are
      // one shape: both category trees, a perimeter crossing (depth 0, so it lands beside the
      // top-level categories), and the surplus or deficit. `kind` is a plain string on the wire,
      // so anything a newer backend adds without a side still sits on the spine rather than
      // breaking the layout.
      const side = sideOf(n);
      if (!side) return c.center;
      return side === "income" ? c.income - 1 - n.level : c.expenseBase + n.level;
    }
  }
}

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

  const kids = new Map<string, string[]>();
  const inward = new Map<string, number>();

  // Hub-rooted child lists, using the same orientation trick `foldHairlines` uses: on the
  // income side the source is the child, on the expense side the target is. `center→savings`
  // lands in the expense case and makes savings an ordinary hub child.
  //
  for (const l of links) {
    const s = byId.get(l.source);
    const t = byId.get(l.target);
    if (!s || !t) continue;
    // Which end is the child is "which end is further from the hub", which `sideOf` answers for
    // every kind — including a waypoint, whose own kind says nothing and whose chain's direction
    // says everything, and the deficit, whose link points *at* the hub: without this it would
    const [child, parent] = sideOf(s) === "income" ? [s, t] : [t, s];
    kids.set(parent.id, [...(kids.get(parent.id) ?? []), child.id]);
    inward.set(child.id, l.value);
  }
  // Within a sibling group the order is free (see the note above), so it ranks by value —
  // except that the hub's own children are two different things. Categories are money earned and
  // spent; a crossing is money that left the household's cash for an account, and interleaving
  // the two by size reads a mortgage repayment as a spending category. Crossings sink to the
  // the deficit stay in the size order they have always had.
  const bandOf = (id: string) => (byId.get(id)!.kind === "crossing" ? 1 : 0);
  for (const list of kids.values()) {
    list.sort(
      (a, b) =>
        bandOf(a) - bandOf(b) ||
        (inward.get(b) ?? 0) - (inward.get(a) ?? 0) ||
        (a < b ? -1 : 1),
    );
  }

  const inColumn = new Map<number, string[]>();
  for (const n of live) {
    const c = columnOfId.get(n.id)!;
    inColumn.set(c, [...(inColumn.get(c) ?? []), n.id]);
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
    // The tree: each parent's children, in the parents' own order.
    for (const p of prevIds) take(kids.get(p) ?? []);
    // The hub, and anything with no hub-ward edge. `foldHairlines` takes a
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

/**
 * Which half of the graph a node is drawn in — and therefore which way "toward the hub" points,
 * which is the question {@link columnOf}, {@link outwardOrder} and {@link foldHairlines} were
 * each answering from `kind` on their own.
 *
 * The wire now states a side for every node that has one, because a crossing broke the old
 * inference: a mortgage repayment is an outflow and a loan drawdown an inflow, and both are
 * `crossing`. The table below wins over it for exactly two kinds. The surplus and the deficit
 * carry no side — neither is income or spending, each *is* the difference between them — but
 * each has a half it must be drawn in: the surplus is a sink one column right of the hub, the
 * deficit a source one column left. Reusing one for the other sends a ribbon backwards through
 * the hub, which d3 would draw looping behind it.
 *
 * The `kind` fallback beneath that is for a server older than `side`, and for the layout tests,
 * which write kinds and nothing else.
 */
const SIDE_BY_KIND: Record<string, "income" | "expense"> = {
  income: "income",
  expense: "expense",
  savings: "expense",
  deficit: "income",
};
export const sideOf = (n: Pick<Placed, "kind" | "side">): "income" | "expense" | null => {
  // `SIDE_BY_KIND` first, then the wire's own word — narrowed rather than trusted, because
  // `side` is a plain string there and an unrecognised one has to land on the spine like an
  // unrecognised `kind` does, not be asserted into a column.
  return SIDE_BY_KIND[n.kind] ?? SIDE_BY_KIND[n.side ?? ""] ?? null;
};
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
function fitToWidth(all: Placed[], width: number): Placed[] {
  // Keyed on *having a side*, which is exactly the set `columnOf` places by `(side, level)`.
  // The trimmable set and the side-placed set have to be the same set: if they diverge, a node
  // survives a cap that `columnsOf` has already shrunk the side past, `columnOf` returns a
  // column nobody allocated, and d3 silently clamps two columns into one — the invisible failure
  // the column calculation guards against. A depth-0 node (a crossing, the
  // surplus, the deficit) is never trimmed, since `cap >= 0`.
  const keep = (cap: number) => all.filter((n) => sideOf(n) === null || n.level <= cap);
  let cap = all.reduce((m, n) => (sideOf(n) === null ? m : Math.max(m, n.level)), 0);
  while (cap > 0 && pitchOf(columnsOf(keep(cap)), width) < MIN_PITCH) cap--;
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
    // Which end is the child is "which end is further from the hub", which `sideOf` answers for
    // every kind — including a waypoint, whose own kind says nothing and whose chain's direction
    // says everything.
    const [child, parent] = sideOf(s) === "income" ? [s, t] : [t, s];
    // Only categories fold. A crossing carries an account and a click target, and an itemised
    // perimeter that collapses into "Other (3)" is worse than not drawing it — the same argument
    if (!isCatKind(child.kind)) continue;
    inward.set(child.id, { parent: parent.id, value: l.value });
    const key = groupKey(child.kind, parent.id);
    siblings.set(key, [...(siblings.get(key) ?? []), child.id]);
  }
  const childrenOf = (id: string) => [
    ...(siblings.get(groupKey("income", id)) ?? []),
    ...(siblings.get(groupKey("expense", id)) ?? []),
  ];
  // The tallest column, which sets the scale for every other one — measured off the hub's own
  // links rather than off the folding candidates.
  //
  // `inward` holds only categories, and a crossing, the surplus and the deficit all share a
  // column with the top-level categories while being unfoldable. Counting only what *can* fold
  // therefore reads every slice as taller than it lands and folds too little, so the hairline
  // stack this function exists to remove comes back — on a household whose expense side is half
  // mortgage, badly. Both sides of the hub sum to the same figure by construction: whichever way
  // the month came out, the difference is the surplus or the deficit, and that sits in a column
  // too.
  let hubIn = 0;
  let hubOut = 0;
  for (const l of links) {
    if (l.target === "center") hubIn += l.value;
    if (l.source === "center") hubOut += l.value;
  }
  const scale = Math.max(hubIn, hubOut);
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
      // Redundant with `kind` for a category, which `SIDE_BY_KIND` already answers — written
      // anyway so the literal is honest against the interface rather than relying on a fallback.
      side,
      account_id: null,
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
  const depthFitted = fitToWidth(nodes.map(placed), boxW);
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
  const live = connectedNodes;
  const cols = columnsOf(live);
  const index = new Map(live.map((n, i) => [n.id, i]));
  // d3 mutates the graph it is handed: it resolves each link's endpoints to the node objects
  // and fills their sourceLinks/targetLinks, so the input cannot be reused or shared.
  const build = () => ({
    nodes: live.map((n) => ({ ...n })),
    links: kept.map((l) => ({
      source: index.get(l.source)!,
      target: index.get(l.target)!,
      value: l.value,
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
  const rank = outwardOrder(live, kept, cols);
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
   * and it is well defined precisely because each tree edge spans
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
