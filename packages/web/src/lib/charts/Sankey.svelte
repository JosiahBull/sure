<script lang="ts" module>
  /**
   * What a click on the chart opens. A category filters the transactions page by category; a
   * crossing has no category — both legs of a mortgage payment carry the same "Transfer" — so it
   * filters by the account it reached instead.
   */
  export type SankeyTarget =
    | { t: "category"; categoryId: number | null; kind: "income" | "expense" }
    | { t: "account"; accountId: number };
</script>

<script lang="ts">
  import { sankeyLinkHorizontal } from "d3-sankey";
  import { categoryColor, colorFor, shade } from "../color";
  import { resolvedTheme } from "../theme.svelte";
  import { layout, isCatKind, sideOf, NODE_W, type Link, type Node } from "./sankeyLayout";

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
    /** Called when a node or link the reader can drill into is clicked. A category opens its
     * transactions (categoryId null = uncategorised; `kind` distinguishes an
     * uncategorised-income click from an uncategorised-expense one, which would otherwise be
     * indistinguishable); a crossing opens the account's instead. */
    onselect?: (target: SankeyTarget) => void;
  } = $props();

  // The chart lays out in real pixels against its measured box rather than scaling a fixed
  // viewBox to fit. With up to three category levels per side the graph can be seven
  // columns wide, and a viewBox that grew with it would shrink the labels exactly when
  // there are most of them to read. `bind:clientWidth/Height` is Svelte's ResizeObserver.
  let boxW = $state(0);
  let boxH = $state(0);


  const uid = Math.random().toString(36).slice(2, 8);

  // Node colours: a green spine for the "Cash flow" hub and the "Savings" surplus, and one
  // colour family per top-level category, shaded by how deep in that family a node sits —
  // so a branch reads as a unit and its levels stay apart. Uncategorised stays neutral
  // grey. Flows are drawn as a source→target gradient of these colours.
  const SPINE = "#10a861";
  /** Statutory deductions: a muted brick red, hardcoded like SPINE and legible on both themes. */
  const DEDUCTION = "#b35953";
  /** The surplus's opposite, and `--negative`'s light value the way SPINE is `--positive`'s.
   * Hot rather than muted on purpose: a household that spent more than it earned should not read
   * as one that saved. */
  const DEFICIT = "#ec2222";
  /** Money crossing the cash perimeter — a mortgage, a brokerage, a car. Structural rather than
   * categorical, so a desaturated slate that belongs to none of the ten category families. */
  const CROSSING = "#5f7d8c";
  const dark = $derived(resolvedTheme() === "dark");
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function nodeColor(n: any): string {
    // A routing waypoint is invisible; it exists only to give a long ribbon somewhere to pass
    // through, so it takes the colour of the node that ribbon is heading for.
    if (n.kind === "via") return nodeColor({ ...n, kind: n.via.colorKind });
    if (n.kind === "center" || n.kind === "savings") return SPINE;
    if (n.kind === "deficit") return DEFICIT;
    if (n.kind === "deduction") return DEDUCTION;
    // A crossing and the account it reaches read as one branch, the way a category and its child
    // do — which is what `shade` already does for the category families.
    if (n.kind === "crossing" || n.kind === "account")
      return shade(CROSSING, n.level ?? 0, dark);
    // A gross node's id is `gross:<person id>`; the id-derived palette is the same fallback
    // `personColor` uses, without coupling the chart to the household store.
    if (n.kind === "gross") return colorFor(Number(n.id.slice("gross:".length)) || 0);
    return categoryColor({ rootId: n.root_id, rootColor: n.root_color, depth: n.level ?? 0, dark });
  }

  const graph = $derived.by(() => layout(nodes, links, boxW, boxH));
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
    // A deduction's destination account joins them, and must: it sits one column right of the
    // deduction it came from, so labelling it leftwards drew its name back across the gap and
    // straight over its own deduction's — two nodes that, being a sink and the account it
    // feeds, carry the same name and the same figure ("Student Stadent loan").
    //
    // Everything else follows its side, which is the same rule stated once: an income-side
    // crossing (a loan drawdown) would otherwise fall through to the `end` branch below and
    // draw its label leftwards, into the strip the expense columns own.
    if (
      sideOf(n) === "income" ||
      n.kind === "gross" ||
      n.kind === "deduction" ||
      n.kind === "account"
    )
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
  //
  // The figure is the *measured* height of a rendered label box — 31.2px for the 12.5px name
  // over the 11px value at `dy=1.2em` — rounded up, and not a pixel less. At 26 two labels
  // could clear the test and still overlap by five pixels, which is how a crossing's two halves
  // came to draw "Interest" through "Principal": they sit in one column, adjacent, and nothing
  // else in the chart had ever put two nodes that close together.
  const MIN_LABEL_GAP = 32;
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
  /**
   * What a click on this node should open. An "Other" bucket stands for several categories at
   * once, so it has nothing to open; a crossing has no category to filter by and opens the
   * account it reached instead — which is also why `isCatKind` stays narrow rather than growing
   * to cover the new kinds, since it is what gates this.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const isClickable = (n: any) =>
    (isCat(n) && !n.aggregate) || (n.kind === "crossing" && n.account_id != null);
  const fmt = (v: number) => (format ? format(v) : String(v));
  /** id → laid node, for resolving a routed leg back to the flow's real endpoints. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const byNodeId = $derived(new Map<string, any>((graph?.nodes ?? []).map((n: any) => [n.id, n])));
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const emit = (n: any) =>
    onselect?.(
      n.kind === "crossing"
        ? { t: "account", accountId: n.account_id }
        : { t: "category", categoryId: n.category_id ?? null, kind: n.kind as "income" | "expense" },
    );

  /**
   * Each side's total, for the top-level percentages. The hub's own `value` is
   * `max(inflow, outflow)`, so it only equals the larger side — using it for both would
   * quietly understate every category on the smaller one.
   *
   * The surplus and the deficit are excluded: neither is money that came in or went out, each
   * *is* the difference between the two, and counting one would make every share on that side
   * read small. A crossing is not one of them — a mortgage principal repayment is cash that
   * genuinely left, and a category's share of outgoings that ignored it would overstate every
   * category on the page.
   */
  const NOT_A_FLOW = new Set(["savings", "deficit"]);
  const sideTotals = $derived.by(() => {
    const hub = graph?.nodes.find((n: any) => n.kind === "center");
    const sum = (ls: any[] | undefined, pick: (l: any) => any) =>
      (ls ?? []).reduce((t, l) => (NOT_A_FLOW.has(pick(l).kind) ? t : t + l.value), 0);
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
    // Crossings answer too: "Home Mortgage, 18% of outgoings" is one of the two most useful
    // numbers this chart gained.
    if (!graph || !(isCat(n) || n.kind === "crossing")) return null;
    const side = sideOf(n);
    // Income flows leaf→hub and expense hub→leaf, so the hub-ward link is the node's own.
    const own = side === "income" ? n.sourceLinks?.[0] : n.targetLinks?.[0];
    const parent = side === "income" ? own?.target : own?.source;
    if (!own || !parent) return null;
    const basis =
      parent.kind === "center"
        ? side === "income"
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
    return endsOf(l).includes(n.id);
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function linkActive(l: any, i: number): boolean {
    if (!hovered) return true;
    if (hovered.t === "link") {
      // A routed flow is drawn as several legs sharing one `origin`; highlighting one of them
      // and not the rest would break the ribbon in half under the pointer.
      const h = graph?.links[hovered.i];
      return h?.origin && l.origin ? h.origin.chain === l.origin.chain : i === hovered.i;
    }
    const hid = hovered.id;
    return endsOf(l).some((id: string) => id === hid);
  }
  /** A link's real endpoints: the flow's own, not the waypoints a long one was bent around. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const endsOf = (l: any): string[] =>
    l.origin ? [l.origin.source, l.origin.target] : [l.source.id, l.target.id];
  /**
   * The endpoint a link should deep-link to: the more specific of its two ends. Income flows
   * child→parent and expense parent→child, so "more specific" is whichever end is a category
   * node with the greater depth. With the hub on one end there's only one candidate anyway
   * — which is all this used to handle, so a link between two category levels was dead.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function linkTargetNode(l: any): any | null {
    const ends = endsOf(l)
      .map((id: string) => byNodeId.get(id))
      .filter((n: any) => n && isClickable(n));
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
    // The flow's own ends, so a leg of a routed chain still names where the money came from and
    // went to rather than the waypoint it happened to be bent around.
    const [from, to] = endsOf(l).map((id: string) => byNodeId.get(id)?.label ?? "");
    tip = { ...point(e), label: `${from} → ${to}`, value: fmt(l.value) };
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
        {@const cat = linkTargetNode(l)}
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
      {#each graph.nodes.filter((n: any) => n.kind !== "via") as n}
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
