<script lang="ts">
  import { sankey, sankeyLinkHorizontal, sankeyJustify } from "d3-sankey";

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
  }: {
    nodes: Node[];
    links: Link[];
    height?: number;
  } = $props();

  const W = 760;

  const KIND_COLOR: Record<string, string> = {
    income: "#34d399",
    expense: "#fb7185",
    center: "#38bdf8",
    savings: "#2dd4bf",
  };
  const color = (kind: string) => KIND_COLOR[kind] ?? "#64748b";

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
      .nodeWidth(14)
      .nodePadding(16)
      .nodeAlign(sankeyJustify)
      .extent([
        [2, 10],
        [W - 2, height - 10],
      ]);
    return gen(g) as { nodes: any[]; links: any[] };
  });
  const linkPath = sankeyLinkHorizontal();
</script>

{#if !graph}
  <div class="empty">No flows for this period.</div>
{:else}
  <svg viewBox="0 0 {W} {height}" width="100%" role="img" aria-label="Money flow diagram">
    {#each graph.links as l}
      <path
        d={linkPath(l) ?? ""}
        fill="none"
        stroke={color(l.source.kind)}
        stroke-opacity="0.32"
        stroke-width={Math.max(1, l.width)}
      />
    {/each}
    {#each graph.nodes as n}
      <rect
        x={n.x0}
        y={n.y0}
        width={n.x1 - n.x0}
        height={Math.max(1, n.y1 - n.y0)}
        fill={color(n.kind)}
        rx="2"
      />
      <text
        x={n.x0 < W / 2 ? n.x1 + 6 : n.x0 - 6}
        y={(n.y0 + n.y1) / 2}
        dy="0.35em"
        text-anchor={n.x0 < W / 2 ? "start" : "end"}
        font-size="11.5"
        fill="var(--text-muted)">{n.label}</text
      >
    {/each}
  </svg>
{/if}
