<script lang="ts">
  import { formatDate, formatMoney } from "../api";
  import type { Schemas } from "../api";

  // History flows into a Monte Carlo projection: a solid actuals line, then a dashed
  // median line with a shaded P10-P90 band. Both series share one index-based x-axis
  // (like LineChart), so the "seam" is just the last historical point re-used as month 0
  // of the projection — no gap, no double-counted point.
  let {
    history,
    months,
    currency = "NZD",
    height = 240,
    onhover,
  }: {
    history: { x: string; y: number }[];
    months: Schemas["ForecastMonth"][];
    currency?: string;
    height?: number;
    /** Fires with the hovered point's date + values, or null on leave. */
    onhover?: (point: { as_of: string; median: number; p10?: number; p90?: number } | null) => void;
  } = $props();

  const W = 640;
  const pad = { l: 6, r: 10, t: 14, b: 22 };

  const seam = $derived(history.at(-1));
  const histLen = $derived(history.length);
  // One combined series so history and projection share a single evenly-spaced x-axis;
  // the projection's index 0 is the seam (the last actual point), so the two lines meet.
  const totalPoints = $derived(histLen + months.length);

  function monthAt(i: number): Schemas["ForecastMonth"] | null {
    const mi = i - histLen;
    return mi >= 0 && mi < months.length ? months[mi] : null;
  }

  const medianY = $derived.by(() => {
    const out: number[] = [];
    for (let i = 0; i < totalPoints; i++) {
      if (i < histLen) out.push(history[i].y);
      else out.push(monthAt(i)?.net_worth.median_minor ?? out.at(-1) ?? 0);
    }
    return out;
  });
  const p10Y = $derived.by(() => {
    const out: (number | null)[] = [];
    for (let i = 0; i < totalPoints; i++) {
      out.push(i === histLen - 1 ? (seam?.y ?? null) : (monthAt(i)?.net_worth.p10_minor ?? null));
    }
    return out;
  });
  const p90Y = $derived.by(() => {
    const out: (number | null)[] = [];
    for (let i = 0; i < totalPoints; i++) {
      out.push(i === histLen - 1 ? (seam?.y ?? null) : (monthAt(i)?.net_worth.p90_minor ?? null));
    }
    return out;
  });

  const allY = $derived(
    [...medianY, ...p10Y.filter((v): v is number => v != null), ...p90Y.filter((v): v is number => v != null)]
  );
  const minY = $derived(allY.length ? Math.min(0, ...allY) : 0);
  const maxY = $derived(allY.length ? Math.max(1, ...allY) : 1);

  function sx(i: number): number {
    const n = totalPoints;
    return pad.l + (W - pad.l - pad.r) * (n <= 1 ? 0 : i / (n - 1));
  }
  function sy(v: number): number {
    const t = (v - minY) / (maxY - minY || 1);
    return pad.t + (height - pad.t - pad.b) * (1 - t);
  }

  const historyPath = $derived(
    Array.from({ length: histLen }, (_, i) => `${i ? "L" : "M"} ${sx(i).toFixed(1)} ${sy(medianY[i]).toFixed(1)}`).join(" ")
  );
  const projectionPath = $derived(
    Array.from({ length: totalPoints - histLen + 1 }, (_, k) => {
      const i = histLen - 1 + k;
      return `${k ? "L" : "M"} ${sx(i).toFixed(1)} ${sy(medianY[i]).toFixed(1)}`;
    }).join(" ")
  );
  const bandPath = $derived.by(() => {
    const idxs = Array.from({ length: totalPoints - histLen + 1 }, (_, k) => histLen - 1 + k);
    const upper = idxs
      .map((i, k) => `${k ? "L" : "M"} ${sx(i).toFixed(1)} ${sy(p90Y[i] ?? medianY[i]).toFixed(1)}`)
      .join(" ");
    const lower = idxs
      .slice()
      .reverse()
      .map((i) => `L ${sx(i).toFixed(1)} ${sy(p10Y[i] ?? medianY[i]).toFixed(1)}`)
      .join(" ");
    return `${upper} ${lower} Z`;
  });

  // Vertical markers + callouts at +3/+6/+9/+12 months (only the ones within horizon).
  const CHECKPOINTS = [3, 6, 9, 12];
  const checkpoints = $derived(
    CHECKPOINTS.filter((m) => m <= months.length).map((m) => ({
      months: m,
      index: histLen - 1 + m,
      month: months[m - 1],
    }))
  );

  const zeroY = $derived(minY < 0 ? sy(0) : null);

  // ---- hover (no brush/zoom — a forward projection isn't something you zoom into) ---
  let svgEl = $state<SVGSVGElement | null>(null);
  let hover = $state<number | null>(null);

  function idxFromClientX(clientX: number): number {
    const rect = svgEl!.getBoundingClientRect();
    const vbX = ((clientX - rect.left) / rect.width) * W;
    const plotW = W - pad.l - pad.r;
    const n = totalPoints;
    if (n <= 1) return 0;
    const i = Math.round(((vbX - pad.l) / plotW) * (n - 1));
    return Math.max(0, Math.min(n - 1, i));
  }
  function setHover(i: number | null) {
    if (i === hover) return;
    hover = i;
    if (!onhover) return;
    if (i == null) {
      onhover(null);
      return;
    }
    const as_of = i < histLen ? history[i].x : (monthAt(i)?.as_of ?? "");
    onhover({
      as_of,
      median: medianY[i],
      p10: p10Y[i] ?? undefined,
      p90: p90Y[i] ?? undefined,
    });
  }
  function onPointerMove(e: PointerEvent) {
    if (!svgEl || totalPoints === 0) return;
    setHover(idxFromClientX(e.clientX));
  }
  function onPointerLeave() {
    setHover(null);
  }

  let tip = $state<{ left: number; top: number; below: boolean } | null>(null);
  $effect(() => {
    const i = hover;
    if (i == null || !svgEl) {
      tip = null;
      return;
    }
    const rect = svgEl.getBoundingClientRect();
    const left = Math.max(48, Math.min(rect.width - 48, (sx(i) / W) * rect.width));
    const top = (sy(medianY[i]) / height) * rect.height;
    tip = { left, top, below: top < 60 };
  });
  const hoverIsProjected = $derived(hover != null && hover >= histLen);
</script>

{#if totalPoints === 0}
  <div class="empty">No data to forecast yet.</div>
{:else}
  <div class="chart-wrap">
    <svg
      bind:this={svgEl}
      viewBox="0 0 {W} {height}"
      width="100%"
      preserveAspectRatio="none"
      role="img"
      aria-label="Net worth history and forecast — hover to inspect"
      class="chart"
      onpointermove={onPointerMove}
      onpointerleave={onPointerLeave}
    >
      {#if zeroY !== null}
        <line x1={pad.l} x2={W - pad.r} y1={zeroY} y2={zeroY} stroke="var(--border-strong)"
              stroke-dasharray="3 3" />
      {/if}
      {#if histLen > 0}
        <line x1={sx(histLen - 1)} x2={sx(histLen - 1)} y1={pad.t} y2={height - pad.b}
              stroke="var(--border-strong)" stroke-dasharray="2 3" vector-effect="non-scaling-stroke" />
      {/if}

      <path d={bandPath} fill="var(--accent)" fill-opacity="0.12" stroke="none" />
      <path d={historyPath} fill="none" stroke="var(--accent)" stroke-width="1.5"
            stroke-linejoin="round" vector-effect="non-scaling-stroke" />
      <path d={projectionPath} fill="none" stroke="var(--accent)" stroke-width="1.5"
            stroke-dasharray="5 4" stroke-linejoin="round" vector-effect="non-scaling-stroke" />

      {#each checkpoints as c (c.months)}
        <line x1={sx(c.index)} x2={sx(c.index)} y1={pad.t} y2={height - pad.b}
              stroke="var(--border)" stroke-dasharray="1 3" vector-effect="non-scaling-stroke" />
        <circle cx={sx(c.index)} cy={sy(c.month.net_worth.median_minor)} r="3" fill="var(--accent)" />
      {/each}

      {#if hover !== null}
        <line x1={sx(hover)} x2={sx(hover)} y1={pad.t} y2={height - pad.b}
              stroke="var(--border-strong)" vector-effect="non-scaling-stroke" />
        <circle cx={sx(hover)} cy={sy(medianY[hover])} r="4.5" fill="var(--accent)"
                stroke="var(--bg-elev)" stroke-width="1.5" />
      {/if}
    </svg>
    {#if tip && hover !== null}
      <div class="tooltip" class:below={tip.below} style="left:{tip.left}px;top:{tip.top}px">
        <div class="tt-val tabular">{formatMoney(medianY[hover], currency)}</div>
        {#if hoverIsProjected && p10Y[hover] != null && p90Y[hover] != null}
          <div class="tt-range tabular">
            {formatMoney(p10Y[hover] ?? 0, currency)} – {formatMoney(p90Y[hover] ?? 0, currency)}
          </div>
        {/if}
        <div class="tt-date">
          {formatDate(hover < histLen ? history[hover].x : monthAt(hover)?.as_of ?? "")}
          {#if hoverIsProjected}<span class="faint"> (projected)</span>{/if}
        </div>
      </div>
    {/if}
  </div>
  <div class="row spread small faint" style="margin-top:4px">
    <span>{history[0] ? formatDate(history[0].x) : ""}</span>
    <span>Today</span>
    <span>{months.at(-1) ? formatDate(months.at(-1)!.as_of) : ""}</span>
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
  .tt-range {
    font-size: 11.5px;
    color: var(--text-muted);
  }
  .tt-date {
    font-size: 11px;
    color: var(--text-faint);
  }
</style>
