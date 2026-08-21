<script lang="ts">
  import { sankey, sankeyLinkHorizontal } from "d3-sankey";
  import { categoryColor, colorFor } from "../color";
  import { resolvedTheme } from "../theme.svelte";

  interface Node {
    id: string;
    label: string;
    kind: string;
    /** 0-based level within its own side; null for the hub and savings. */
    depth?: number | null;
    category_id?: number | null;
    root_id?: number | null;
    root_color?: string | null;
  }
  interface Link {
    source: string;
    target: string;
    value: number;
  }
  let {
    nodes,
    links,
    height = "440px",
    format,
    onselect,
  }: {
    nodes: Node[];
    links: Link[];
    /** CSS height for the chart box; the layout is measured from it, not scaled into it. */
    height?: string;
    /** Formats a minor-unit value for the hover tooltip. */
    format?: (minor: number) => string;
    /** Called when a category node/link is clicked (categoryId null = uncategorised;
     * kind distinguishes an uncategorised-income click from an uncategorised-expense one,
     * which would otherwise be indistinguishable. */
    onselect?: (categoryId: number | null, kind: "income" | "expense") => void;
  } = $props();

  // The chart lays out in real pixels against its measured box rather than scaling a fixed
  // viewBox to fit. With up to three category levels per side the graph can be seven
  // columns wide, and a viewBox that grew with it would shrink the labels exactly when
  // there are most of them to read. `bind:clientWidth/Height` is Svelte's ResizeObserver.
  let boxW = $state(0);
  let boxH = $state(0);

  const NODE_W = 12;
  const MAX_NODE_PAD = 18;
  const MIN_NODE_PAD = 4;
  /** Node padding may not eat more than this share of the box, however many nodes there are. */
  const MAX_PAD_RATIO = 0.4;
  const MARGIN_X = 4;
  /** Room above the graph for the hub's label, which sits over it rather than beside it. */
  const MARGIN_TOP = 30;
  const MARGIN_BOTTOM = 14;

  const uid = Math.random().toString(36).slice(2, 8);

  // Node colours: a green spine for the "Cash flow" hub and the "Savings" surplus, and one
  // colour family per top-level category, shaded by how deep in that family a node sits —
  // so a branch reads as a unit and its levels stay apart. Uncategorised stays neutral
  // grey. Flows are drawn as a source→target gradient of these colours.
  const SPINE = "#10a861";
  /** Statutory deductions: a muted brick red, hardcoded like SPINE and legible on both themes. */
  const DEDUCTION = "#b35953";
  const dark = $derived(resolvedTheme() === "dark");
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function nodeColor(n: any): string {
    if (n.kind === "center" || n.kind === "savings") return SPINE;
    if (n.kind === "deduction") return DEDUCTION;
    // A gross node's id is `gross:<person id>`; the id-derived palette is the same fallback
    // `personColor` uses, without coupling the chart to the household store.
    if (n.kind === "gross") return colorFor(Number(n.id.slice("gross:".length)) || 0);
    return categoryColor({ rootId: n.root_id, rootColor: n.root_color, depth: n.level ?? 0, dark });
  }

  /**
   * A node as this component handles it, with the wire's `depth` renamed to `level`.
   *
   * `depth` is d3-sankey's own: `computeNodeDepths` overwrites it with the longest path
   * ending at the node, and it does that *before* `nodeAlign` is consulted — so leaving the
   * field under its wire name means the layout silently reads d3's number instead of ours.
   */
  type Placed = Omit<Node, "depth"> & {
    level: number;
    /** True for a synthesised "Other" node — see {@link foldHairlines}. */
    aggregate?: boolean;
  };
  const placed = (n: Node): Placed => {
    const { depth, ...rest } = n;
    return { ...rest, level: depth ?? 0 };
  };

  type Cols = { income: number; center: number; expenseBase: number; total: number };

  /** How many columns each side needs, from the nodes actually being laid out. */
  function columnsOf(live: Placed[]): Cols {
    let income = 0;
    let expense = 0;
    let pre = 0;
    for (const n of live) {
      if (n.kind === "income") income = Math.max(income, n.level + 1);
      else if (n.kind === "expense") expense = Math.max(expense, n.level + 1);
      else if (n.kind === "savings") expense = Math.max(expense, 1);
      // The pre-income layer claims one column left of every category level; the deduction
      // sinks live inside the first income column, so only `gross` widens the graph.
      else if (n.kind === "gross") pre = 1;
    }
    income += pre;
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
      // `kind` is a plain string on the wire, so the hub — and anything a newer backend
      // adds — sits on the spine rather than breaking the layout.
      default:
        return c.center;
    }
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
  const isCatKind = (kind: string) => kind === "income" || kind === "expense";
  const pitchOf = (cols: Cols, width: number) =>
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
    const keep = (cap: number) => all.filter((n) => !isCatKind(n.kind) || n.level <= cap);
    let cap = all.reduce((m, n) => (isCatKind(n.kind) ? Math.max(m, n.level) : m), 0);
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
      // The pre-income layer never folds: a gross→category link would otherwise be read
      // backwards as the category's hub-ward edge (clobbering its real value), and ACC
      // vanishing into "Other (2)" is exactly what an itemised layer must not do.
      if (s.kind === "gross" || t.kind === "deduction") continue;
      const [child, parent] = s.kind === "income" ? [s, t] : [t, s];
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

  const graph = $derived.by(() => {
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
    const live = within.filter((n) => connected.has(n.id));
    const index = new Map(live.map((n, i) => [n.id, i]));
    const cols = columnsOf(live);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const g: any = {
      nodes: live.map((n) => ({ ...n })),
      links: kept.map((l) => ({
        source: index.get(l.source)!,
        target: index.get(l.target)!,
        value: l.value,
      })),
    };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const gen = (sankey() as any)
      .nodeWidth(NODE_W)
      .nodePadding(nodePadding(live.length, available))
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      .nodeAlign((n: any) => columnOf(n, cols))
      .extent([
        [MARGIN_X, MARGIN_TOP],
        [boxW - MARGIN_X, boxH - MARGIN_BOTTOM],
      ]);
    const laid = gen(g) as { nodes: any[]; links: any[] };
    // Column pitch — how much room a label has between its own column and the next.
    const kx = pitchOf(cols, boxW);
    return { ...laid, cols, kx: Number.isFinite(kx) ? kx : boxW };
  });
  const linkPath = sankeyLinkHorizontal();

  // Node shapes: pure-source (income leaf) nodes round their outer/left edge, pure-target
  // (expense leaf, savings) nodes round their outer/right edge, and anything with flows on
  // both sides — the hub and every intermediate level — stays square.
  const R = 8;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function nodePath(n: any): string {
    const x0 = n.x0,
      y0 = n.y0,
      x1 = n.x1,
      y1 = Math.max(n.y0 + 1, n.y1);
    const r = Math.max(0, Math.min(R, (y1 - y0) / 2));
    const src = (n.sourceLinks?.length ?? 0) > 0;
    const tgt = (n.targetLinks?.length ?? 0) > 0;
    if (y1 - y0 < r * 2 || (src && tgt)) return `M${x0},${y0} H${x1} V${y1} H${x0} Z`;
    if (src)
      return `M${x0 + r},${y0} H${x1} V${y1} H${x0 + r} Q${x0},${y1} ${x0},${y1 - r} V${y0 + r} Q${x0},${y0} ${x0 + r},${y0} Z`;
    return `M${x0},${y0} H${x1 - r} Q${x1},${y0} ${x1},${y0 + r} V${y1 - r} Q${x1},${y1} ${x1 - r},${y1} H${x0} Z`;
  }

  // ---- labels ---------------------------------------------------------------
  // Each label lives in the gap between its own column and the hub: income to the right of
  // its node, expense and savings to the left. That fills every inter-column gap with
  // exactly one column's worth — income column i uses gap(i, i+1) and expense column j uses
  // gap(j-1, j) — so no two columns compete for the same strip. The hub is nearly full
  // height and has no gap of its own, so its label goes above it (what MARGIN_TOP reserves).
  const LABEL_PAD = 7;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function labelPos(n: any): { x: number; y: number; anchor: "start" | "middle" | "end" } {
    if (n.kind === "center") return { x: (n.x0 + n.x1) / 2, y: n.y0 - 18, anchor: "middle" };
    // The pre-income nodes label rightwards like income: gross sits in the leftmost column
    // with only MARGIN_X to its left, and the deduction sinks share the income side's gaps.
    if (n.kind === "income" || n.kind === "gross" || n.kind === "deduction")
      return { x: n.x1 + LABEL_PAD, y: (n.y0 + n.y1) / 2, anchor: "start" };
    return { x: n.x0 - LABEL_PAD, y: (n.y0 + n.y1) / 2, anchor: "end" };
  }

  // SVG <text> has no ellipsis, and at full depth a gap is only ~130px, so clip the name to
  // what fits rather than letting it run under the neighbouring column. Hover still shows
  // the full name, and the expand view gives every label room.
  const CHAR_W = 6.6; // ~0.53em at the 12.5px label size
  const labelBudget = $derived(graph ? Math.max(48, graph.kx - NODE_W - 2 * LABEL_PAD) : 200);
  function clipLabel(name: string): string {
    const max = Math.max(4, Math.floor(labelBudget / CHAR_W));
    return name.length <= max ? name : `${name.slice(0, max - 1)}…`;
  }

  // Two-line labels need vertical room; in a crowded column they would overlap into an
  // unreadable stack. Hide any label sitting within MIN_LABEL_GAP of the previous visible
  // one in its column (keeping the topmost); hover reveals a hidden label via `nodeActive`.
  const MIN_LABEL_GAP = 26;
  const hiddenLabels = $derived.by(() => {
    const hide = new Set<string>();
    if (!graph) return hide;
    // Bucket by `layer` — the column our own nodeAlign produced — not d3's `depth`. They
    // diverge exactly in the ragged case: a childless income root has depth 0 but sits in
    // the last income column, while another branch's grandchild also has depth 0 in column
    // 0. Comparing those two nodes' y-positions would hide a label that never collided.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const columns = new Map<number, any[]>();
    for (const n of graph.nodes) {
      const c = n.layer ?? 0;
      if (!columns.has(c)) columns.set(c, []);
      columns.get(c)!.push(n);
    }
    // Tallest first, keeping a label only where it clears every label already kept in that
    // column. Walking top-to-bottom instead would let an arbitrary sliver claim the space
    // its much larger neighbour needed; going by height means the biggest flows — the ones
    // worth reading — always win the room, and nothing can overlap.
    for (const col of columns.values()) {
      const taken: number[] = [];
      for (const n of [...col].sort((a, b) => b.y1 - b.y0 - (a.y1 - a.y0))) {
        const y = (n.y0 + n.y1) / 2;
        if (taken.some((t) => Math.abs(t - y) < MIN_LABEL_GAP)) hide.add(n.id);
        else taken.push(y);
      }
    }
    return hide;
  });

  // ---- interactivity: hover highlights an element and its connected flows, click on a
  // category node/link opens the matching transactions. ------------------------------
  type Hover = { t: "node"; id: string } | { t: "link"; i: number } | null;
  let hovered = $state<Hover>(null);
  let container = $state<HTMLDivElement>();
  let tip = $state<{ x: number; y: number; flip: boolean; label: string; value: string } | null>(null);

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const isCat = (n: any) => isCatKind(n.kind);
  /** An "Other" bucket stands for several categories at once, so it has nothing to open. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const isClickable = (n: any) => isCat(n) && !n.aggregate;
  const fmt = (v: number) => (format ? format(v) : String(v));
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const emit = (n: any) => onselect?.(n.category_id ?? null, n.kind as "income" | "expense");

  /**
   * Each side's total, for the top-level percentages. The hub's own `value` is
   * `max(inflow, outflow)`, so it only equals the larger side — using it for both would
   * quietly understate every category on the smaller one. Savings is excluded from the
   * expense total: it's the leftover, not a thing that was spent.
   */
  const sideTotals = $derived.by(() => {
    const hub = graph?.nodes.find((n: any) => n.kind === "center");
    const sum = (ls: any[] | undefined, pick: (l: any) => any) =>
      (ls ?? []).reduce((t, l) => (pick(l).kind === "savings" ? t : t + l.value), 0);
    return {
      income: sum(hub?.targetLinks, (l) => l.source),
      expense: sum(hub?.sourceLinks, (l) => l.target),
    };
  });

  /**
   * A node's share of the flow it came out of: of its parent for a nested category, of its
   * whole side for a top-level one. Reading "71% of Employment" is most of what makes a
   * deep chart legible, and it's all derivable from the laid-out graph — the previous app
   * sent a `percentage` per node instead.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function sharePct(n: any): number | null {
    if (!graph || !isCat(n)) return null;
    // Income flows leaf→hub and expense hub→leaf, so the hub-ward link is the node's own.
    const own = n.kind === "income" ? n.sourceLinks?.[0] : n.targetLinks?.[0];
    const parent = n.kind === "income" ? own?.target : own?.source;
    if (!own || !parent) return null;
    const basis =
      parent.kind === "center"
        ? n.kind === "income"
          ? sideTotals.income
          : sideTotals.expense
        : parent.value;
    return basis > 0 ? (own.value / basis) * 100 : null;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function nodeActive(n: any): boolean {
    if (!hovered || !graph) return true;
    if (hovered.t === "node") {
      const hid = hovered.id;
      if (n.id === hid) return true;
      // Also light the nodes directly connected to the hovered one, so the path reads whole.
      return graph.links.some(
        (l: any) => (l.source.id === hid && l.target.id === n.id) || (l.target.id === hid && l.source.id === n.id),
      );
    }
    const l = graph.links[hovered.i];
    return l.source.id === n.id || l.target.id === n.id;
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function linkActive(l: any, i: number): boolean {
    if (!hovered) return true;
    if (hovered.t === "link") return i === hovered.i;
    return l.source.id === hovered.id || l.target.id === hovered.id;
  }
  /**
   * The endpoint a link should deep-link to: the more specific of its two ends. Income flows
   * child→parent and expense parent→child, so "more specific" is whichever end is a category
   * node with the greater depth. With the hub on one end there's only one candidate anyway
   * — which is all this used to handle, so a link between two category levels was dead.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function linkCatNode(l: any): any | null {
    const ends = [l.source, l.target].filter(isClickable);
    if (!ends.length) return null;
    return ends.reduce((a, b) => ((b.level ?? 0) > (a.level ?? 0) ? b : a));
  }

  function point(e: PointerEvent): { x: number; y: number; flip: boolean } {
    const r = container?.getBoundingClientRect();
    const x = r ? e.clientX - r.left : 0;
    return { x, y: r ? e.clientY - r.top : 0, flip: r ? x > r.width / 2 : false };
  }
  function moveTip(e: PointerEvent) {
    if (tip) Object.assign(tip, point(e));
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function enterNode(n: any, e: PointerEvent) {
    hovered = { t: "node", id: n.id };
    const pct = sharePct(n);
    tip = {
      ...point(e),
      label: n.label,
      value: pct === null ? fmt(n.value) : `${fmt(n.value)} (${pct.toFixed(1)}%)`,
    };
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function enterLink(l: any, i: number, e: PointerEvent) {
    hovered = { t: "link", i };
    tip = { ...point(e), label: `${l.source.label} → ${l.target.label}`, value: fmt(l.value) };
  }
  const leave = () => {
    hovered = null;
    tip = null;
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function keyNode(e: KeyboardEvent, n: any) {
    if (isClickable(n) && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      emit(n);
    }
  }
</script>

<div class="sankey-wrap" bind:this={container} bind:clientWidth={boxW} bind:clientHeight={boxH} style:height>
  {#if !graph}
    <div class="empty">No flows for this period.</div>
  {:else}
    <svg width={boxW} height={boxH}>
      <defs>
        {#each graph.links as l, i}
          <linearGradient
            id="sk-{uid}-{i}"
            gradientUnits="userSpaceOnUse"
            x1={l.source.x1}
            x2={l.target.x0}
          >
            <stop offset="0%" stop-color={nodeColor(l.source)} />
            <stop offset="100%" stop-color={nodeColor(l.target)} />
          </linearGradient>
        {/each}
      </defs>
      {#each graph.links as l, i}
        {@const cat = linkCatNode(l)}
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
        <path
          class="link"
          d={linkPath(l) ?? ""}
          fill="none"
          stroke="url(#sk-{uid}-{i})"
          stroke-opacity={hovered ? (linkActive(l, i) ? 0.62 : 0.06) : 0.3}
          stroke-width={Math.max(1, l.width)}
          style:cursor={cat ? "pointer" : "default"}
          onpointerenter={(e) => enterLink(l, i, e)}
          onpointermove={moveTip}
          onpointerleave={leave}
          onclick={() => cat && emit(cat)}
        />
      {/each}
      {#each graph.nodes as n}
        {@const clickable = isClickable(n)}
        {@const lp = labelPos(n)}
        {@const showLabel = !hiddenLabels.has(n.id) || (!!hovered && nodeActive(n))}
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_no_noninteractive_tabindex -->
        <g
          class="node"
          data-node-id={n.id}
          opacity={hovered && !nodeActive(n) ? 0.25 : 1}
          role={clickable ? "button" : undefined}
          tabindex={clickable ? 0 : undefined}
          aria-label={clickable ? `${n.label}, ${fmt(n.value)}` : undefined}
          style:cursor={clickable ? "pointer" : "default"}
          onpointerenter={(e) => enterNode(n, e)}
          onpointermove={moveTip}
          onpointerleave={leave}
          onfocus={() => (hovered = { t: "node", id: n.id })}
          onblur={leave}
          onclick={() => clickable && emit(n)}
          onkeydown={(e) => keyNode(e, n)}
        >
          <path d={nodePath(n)} fill={nodeColor(n)} />
          <text
            class="label"
            class:hidden={!showLabel}
            x={lp.x}
            y={lp.y}
            dy="-0.2em"
            text-anchor={lp.anchor}
          >
            <tspan class="label-name">{clipLabel(n.label)}</tspan>
            <tspan class="label-value" x={lp.x} dy="1.2em">{fmt(n.value)}</tspan>
          </text>
        </g>
      {/each}
    </svg>
    {#if tip}
      <div class="tip" class:flip={tip.flip} style="left:{tip.x}px; top:{tip.y}px">
        <span class="tip-label">{tip.label}</span>
        <span class="tip-value tabular">{tip.value}</span>
      </div>
    {/if}
  {/if}
</div>

<style>
  .sankey-wrap {
    position: relative;
    width: 100%;
  }
  .empty {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
  }
  .node,
  .link {
    transition: opacity 0.15s ease, stroke-opacity 0.15s ease;
  }
  .label {
    user-select: none;
    transition: opacity 0.2s ease;
  }
  .label.hidden {
    opacity: 0;
    pointer-events: none;
  }
  .label-name {
    fill: var(--text);
    font-size: 12.5px;
    font-weight: 500;
  }
  .label-value {
    fill: var(--text-muted);
    font-family: var(--mono);
    font-size: 11px;
  }
  .tip {
    position: absolute;
    transform: translate(12px, -50%);
    background: var(--bg-elev);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    padding: 5px 9px;
    display: flex;
    flex-direction: column;
    gap: 1px;
    font-size: 12.5px;
    line-height: 1.3;
    white-space: nowrap;
    pointer-events: none;
    box-shadow: var(--shadow);
    z-index: 5;
  }
  .tip.flip {
    transform: translate(calc(-100% - 12px), -50%);
  }
  .tip-label {
    color: var(--text);
    font-weight: 600;
  }
  .tip-value {
    color: var(--text-muted);
  }
</style>
