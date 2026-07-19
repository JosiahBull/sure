<script lang="ts">
  import { sankey, sankeyLinkHorizontal, sankeyJustify } from "d3-sankey";
  import { colorFor } from "../api";

  interface Node {
    id: string;
    label: string;
    kind: string;
  }
  interface Link {
    source: string;
    target: string;
    value: number;
  }
  let {
    nodes,
    links,
    height = 440,
    format,
    onselect,
  }: {
    nodes: Node[];
    links: Link[];
    height?: number;
    /** Formats a minor-unit value for the hover tooltip. */
    format?: (minor: number) => string;
    /** Called when a category node/link is clicked (null = uncategorised). */
    onselect?: (categoryId: number | null) => void;
  } = $props();

  const W = 760;

  const uid = Math.random().toString(36).slice(2, 8);

  // Node colours mirror the previous app: a green spine for the "Cash flow" hub and the
  // "Savings" surplus, with each income/expense category tinted from the shared category
  // palette so the Sankey agrees with the dashboard pies. Uncategorised (key 0) stays a
  // neutral grey. Flows are drawn as a source→target gradient of these colours.
  const SPINE = "#10a861";
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function nodeColor(n: any): string {
    if (n.kind === "center" || n.kind === "savings") return SPINE;
    if (isCat(n.id)) return colorFor(catKey(n.id) || null);
    return "#64748b";
  }

  const graph = $derived.by(() => {
    const usable = links.filter((l) => l.value > 0);
    if (!nodes.length || !usable.length) return null;
    const index = new Map(nodes.map((n, i) => [n.id, i]));
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const g: any = {
      nodes: nodes.map((n) => ({ ...n })),
      links: usable
        .filter((l) => index.has(l.source) && index.has(l.target))
        .map((l) => ({
          source: index.get(l.source)!,
          target: index.get(l.target)!,
          value: l.value,
        })),
    };
    if (!g.links.length) return null;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const gen = (sankey() as any)
      .nodeWidth(15)
      .nodePadding(20)
      .nodeAlign(sankeyJustify)
      .extent([
        [6, 14],
        [W - 6, height - 14],
      ]);
    return gen(g) as { nodes: any[]; links: any[] };
  });
  const linkPath = sankeyLinkHorizontal();

  // Node shapes: pure-source (income) nodes round their outer/left edge, pure-target
  // (expense/savings) nodes round their outer/right edge, and the two-sided hub stays
  // square — reproducing the previous app's pill-ended leaf nodes.
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

  // Two-line labels need vertical room; in a crowded column they would overlap into an
  // unreadable stack. Hide any label sitting within MIN_LABEL_GAP of the previous visible
  // one in its column (keeping the topmost); hover reveals a hidden label via `nodeActive`.
  const MIN_LABEL_GAP = 24;
  const hiddenLabels = $derived.by(() => {
    const hide = new Set<string>();
    if (!graph) return hide;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const columns = new Map<number, any[]>();
    for (const n of graph.nodes) {
      const d = n.depth ?? 0;
      if (!columns.has(d)) columns.set(d, []);
      columns.get(d)!.push(n);
    }
    for (const col of columns.values()) {
      col.sort((a, b) => (a.y0 + a.y1) / 2 - (b.y0 + b.y1) / 2);
      let lastY = -Infinity;
      for (const n of col) {
        const y = (n.y0 + n.y1) / 2;
        if (y - lastY < MIN_LABEL_GAP) hide.add(n.id);
        else lastY = y;
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

  // Category nodes are `in:<key>` (income) / `out:<key>` (expense); `key` is a top-level
  // category id, or 0 for uncategorised. `center`/`savings` aren't category nodes.
  const isCat = (id: string) => id.startsWith("in:") || id.startsWith("out:");
  const catKey = (id: string) => Number(id.slice(id.indexOf(":") + 1));
  const fmt = (v: number) => (format ? format(v) : String(v));
  const emit = (key: number) => onselect?.(key === 0 ? null : key);

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
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  function linkCatNode(l: any): any | null {
    if (l.source.id === "center") return isCat(l.target.id) ? l.target : null;
    if (l.target.id === "center") return isCat(l.source.id) ? l.source : null;
    return null;
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
    tip = { ...point(e), label: n.label, value: fmt(n.value) };
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
    if (isCat(n.id) && (e.key === "Enter" || e.key === " ")) {
      e.preventDefault();
      emit(catKey(n.id));
    }
  }
</script>

{#if !graph}
  <div class="empty">No flows for this period.</div>
{:else}
  <div class="sankey-wrap" bind:this={container}>
    <svg viewBox="0 0 {W} {height}" width="100%">
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
          onclick={() => cat && emit(catKey(cat.id))}
        />
      {/each}
      {#each graph.nodes as n}
        {@const clickable = isCat(n.id)}
        {@const lx = n.x0 < W / 2 ? n.x1 + 7 : n.x0 - 7}
        {@const showLabel = !hiddenLabels.has(n.id) || (!!hovered && nodeActive(n))}
        <!-- svelte-ignore a11y_no_static_element_interactions, a11y_no_noninteractive_tabindex -->
        <g
          class="node"
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
          onclick={() => clickable && emit(catKey(n.id))}
          onkeydown={(e) => keyNode(e, n)}
        >
          <path d={nodePath(n)} fill={nodeColor(n)} />
          <text
            class="label"
            class:hidden={!showLabel}
            x={lx}
            y={(n.y0 + n.y1) / 2}
            dy="-0.2em"
            text-anchor={n.x0 < W / 2 ? "start" : "end"}
          >
            <tspan class="label-name">{n.label}</tspan>
            <tspan class="label-value" x={lx} dy="1.2em">{fmt(n.value)}</tspan>
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
  </div>
{/if}

<style>
  .sankey-wrap {
    position: relative;
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
    font-size: 12px;
    font-weight: 500;
  }
  .label-value {
    fill: var(--text-muted);
    font-family: var(--mono);
    font-size: 10.5px;
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
