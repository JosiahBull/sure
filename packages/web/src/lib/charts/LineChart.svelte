<script lang="ts">
  import { formatShort, formatDate } from "../api";

  // Responsive line+area chart on a fixed viewBox (scales to container width).
  let {
    points,
    height = 210,
  }: {
    points: { x: string; y: number }[];
    height?: number;
  } = $props();

  const W = 640;
  const pad = { l: 6, r: 54, t: 14, b: 22 };

  const ys = $derived(points.map((p) => p.y));
  const minY = $derived(points.length ? Math.min(0, ...ys) : 0);
  const maxY = $derived(points.length ? Math.max(1, ...ys) : 1);

  function sx(i: number): number {
    const n = points.length;
    return pad.l + (W - pad.l - pad.r) * (n <= 1 ? 0 : i / (n - 1));
  }
  function sy(v: number): number {
    const t = (v - minY) / (maxY - minY || 1);
    return pad.t + (height - pad.t - pad.b) * (1 - t);
  }

  const linePath = $derived(
    points.map((p, i) => `${i ? "L" : "M"} ${sx(i).toFixed(1)} ${sy(p.y).toFixed(1)}`).join(" ")
  );
  const areaPath = $derived(
    points.length
      ? `${linePath} L ${sx(points.length - 1).toFixed(1)} ${sy(minY).toFixed(1)} L ${sx(0).toFixed(1)} ${sy(minY).toFixed(1)} Z`
      : ""
  );
  const last = $derived(points.at(-1));
  const zeroY = $derived(minY < 0 ? sy(0) : null);
</script>

{#if points.length === 0}
  <div class="empty">No data for this period.</div>
{:else}
  <svg viewBox="0 0 {W} {height}" width="100%" preserveAspectRatio="none" role="img"
       aria-label="Net worth over time">
    <defs>
      <linearGradient id="nw-fill" x1="0" x2="0" y1="0" y2="1">
        <stop offset="0%" stop-color="var(--accent)" stop-opacity="0.28" />
        <stop offset="100%" stop-color="var(--accent)" stop-opacity="0" />
      </linearGradient>
    </defs>
    {#if zeroY !== null}
      <line x1={pad.l} x2={W - pad.r} y1={zeroY} y2={zeroY} stroke="var(--border-strong)"
            stroke-dasharray="3 3" />
    {/if}
    <path d={areaPath} fill="url(#nw-fill)" />
    <path d={linePath} fill="none" stroke="var(--accent)" stroke-width="2.5"
          stroke-linejoin="round" vector-effect="non-scaling-stroke" />
    {#if last}
      <circle cx={sx(points.length - 1)} cy={sy(last.y)} r="3.5" fill="var(--accent)" />
      <text x={W - pad.r + 6} y={sy(maxY) + 4} font-size="11" fill="var(--text-faint)"
            class="tabular">{formatShort(maxY)}</text>
      <text x={W - pad.r + 6} y={sy(minY)} font-size="11" fill="var(--text-faint)"
            class="tabular">{formatShort(minY)}</text>
    {/if}
  </svg>
  <div class="row spread small faint" style="margin-top:4px">
    <span>{formatDate(points[0].x)}</span>
    <span>{formatDate(points[points.length - 1].x)}</span>
  </div>
{/if}
