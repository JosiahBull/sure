<script lang="ts">
  import { formatDate, formatMoney } from "../api";

  // Responsive line+area chart on a fixed viewBox (scales to container width).
  // Supports a snapping hover crosshair/tooltip and a click-drag brush that
  // reports the selected date window back to the parent.
  let {
    points,
    height = 210,
    currency = "NZD",
    onhover,
    onbrush,
  }: {
    points: { x: string; y: number }[];
    height?: number;
    currency?: string;
    /** Fires with the snapped point index under the cursor, or null on leave. */
    onhover?: (index: number | null) => void;
    /** Fires once on drag-release with the inclusive date window that was swept. */
    onbrush?: (range: { from: string; to: string }) => void;
  } = $props();

  const W = 640;
  const pad = { l: 6, r: 10, t: 14, b: 22 };

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
  const last = $derived(points.at(-1));
  const zeroY = $derived(minY < 0 ? sy(0) : null);

  // ---- Interaction ----------------------------------------------------------
  let svgEl = $state<SVGSVGElement | null>(null);
  let hover = $state<number | null>(null);
  let brushing = $state(false);
  let brushA = $state<number | null>(null); // drag anchor index
  let brushB = $state<number | null>(null); // drag current index

  /** Map a client X coordinate to the nearest point index (snaps to data). */
  function idxFromClientX(clientX: number): number {
    const rect = svgEl!.getBoundingClientRect();
    const vbX = ((clientX - rect.left) / rect.width) * W; // preserveAspectRatio="none" => scale X independently
    const plotW = W - pad.l - pad.r;
    const n = points.length;
    if (n <= 1) return 0;
    const i = Math.round(((vbX - pad.l) / plotW) * (n - 1));
    return Math.max(0, Math.min(n - 1, i));
  }

  function setHover(i: number | null) {
    if (i === hover) return;
    hover = i;
    onhover?.(i);
  }

  function onPointerMove(e: PointerEvent) {
    if (!svgEl || points.length === 0) return;
    const i = idxFromClientX(e.clientX);
    if (brushing) brushB = i;
    else setHover(i);
  }

  function onPointerDown(e: PointerEvent) {
    if (!svgEl || points.length <= 1 || e.button !== 0) return;
    brushing = true;
    brushA = brushB = idxFromClientX(e.clientX);
    setHover(null); // the top stat tracks the drag range, not a single point
    svgEl.setPointerCapture(e.pointerId);
    e.preventDefault();
  }

  function endBrush(e: PointerEvent) {
    if (!brushing) return;
    brushing = false;
    try {
      svgEl?.releasePointerCapture(e.pointerId);
    } catch {
      /* pointer already released */
    }
    const a = brushA,
      b = brushB;
    brushA = brushB = null;
    // A drag that never left its start point reads as a click — don't zoom.
    if (a == null || b == null || a === b) return;
    const lo = Math.min(a, b),
      hi = Math.max(a, b);
    onbrush?.({ from: points[lo].x, to: points[hi].x });
  }

  function onPointerLeave() {
    if (!brushing) setHover(null);
  }

  const brushRect = $derived.by(() => {
    if (brushA == null || brushB == null) return null;
    const lo = Math.min(brushA, brushB),
      hi = Math.max(brushA, brushB);
    const x = sx(lo);
    return { x, w: Math.max(1, sx(hi) - x) };
  });

  // Tooltip position in wrapper pixels (recomputed when the snapped index moves).
  let tip = $state<{ left: number; top: number; below: boolean } | null>(null);
  $effect(() => {
    const i = hover;
    if (i == null || brushing || !svgEl || !points[i]) {
      tip = null;
      return;
    }
    const rect = svgEl.getBoundingClientRect();
    const left = Math.max(48, Math.min(rect.width - 48, (sx(i) / W) * rect.width));
    const top = (sy(points[i].y) / height) * rect.height;
    tip = { left, top, below: top < 52 };
  });
</script>

{#if points.length === 0}
  <div class="empty">No data for this period.</div>
{:else}
  <div class="chart-wrap">
    <svg
      bind:this={svgEl}
      viewBox="0 0 {W} {height}"
      width="100%"
      preserveAspectRatio="none"
      role="img"
      aria-label="Net worth over time — hover to inspect, drag to zoom"
      class="chart"
      onpointermove={onPointerMove}
      onpointerdown={onPointerDown}
      onpointerup={endBrush}
      onpointercancel={endBrush}
      onpointerleave={onPointerLeave}
    >
      {#if zeroY !== null}
        <line x1={pad.l} x2={W - pad.r} y1={zeroY} y2={zeroY} stroke="var(--border-strong)"
              stroke-dasharray="3 3" />
      {/if}
      <path d={linePath} fill="none" stroke="var(--accent)" stroke-width="1.5"
            stroke-linejoin="round" vector-effect="non-scaling-stroke" />
      {#if brushRect}
        <rect x={brushRect.x} y={pad.t} width={brushRect.w} height={height - pad.t - pad.b}
              fill="var(--accent)" fill-opacity="0.12" stroke="var(--accent)"
              stroke-opacity="0.5" vector-effect="non-scaling-stroke" />
      {/if}
      {#if hover !== null && points[hover] && !brushing}
        <line x1={sx(hover)} x2={sx(hover)} y1={pad.t} y2={height - pad.b}
              stroke="var(--border-strong)" vector-effect="non-scaling-stroke" />
        <circle cx={sx(hover)} cy={sy(points[hover].y)} r="4.5" fill="var(--accent)"
                stroke="var(--bg-elev)" stroke-width="1.5" />
      {/if}
      {#if last}
        <circle cx={sx(points.length - 1)} cy={sy(last.y)} r="3.5" fill="var(--accent)" />
      {/if}
    </svg>
    {#if tip && hover !== null && points[hover]}
      <div class="tooltip" class:below={tip.below} style="left:{tip.left}px;top:{tip.top}px">
        <div class="tt-val tabular">{formatMoney(points[hover].y, currency)}</div>
        <div class="tt-date">{formatDate(points[hover].x)}</div>
      </div>
    {/if}
  </div>
  <div class="row spread small faint" style="margin-top:4px">
    <span>{formatDate(points[0].x)}</span>
    <span>{formatDate(points[points.length - 1].x)}</span>
  </div>
{/if}

<style>
  .chart-wrap {
    position: relative;
  }
  .chart {
    cursor: crosshair;
    touch-action: none;
    -webkit-user-select: none;
    user-select: none;
  }
  .tooltip {
    position: absolute;
    z-index: 3;
    transform: translate(-50%, calc(-100% - 12px));
    padding: 6px 9px;
    border-radius: 8px;
    background: var(--bg-elev);
    border: 1px solid var(--border-strong);
    box-shadow: var(--shadow);
    pointer-events: none;
    white-space: nowrap;
    line-height: 1.25;
  }
  .tooltip.below {
    transform: translate(-50%, 12px);
  }
  .tt-val {
    font-size: 13.5px;
    font-weight: 640;
    letter-spacing: -0.01em;
  }
  .tt-date {
    font-size: 11px;
    color: var(--text-faint);
  }
</style>
