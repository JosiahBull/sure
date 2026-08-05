<script lang="ts">
  import { formatDate, formatMoney } from "../api";
  import type { Schemas } from "../api";
  import { CHART_W, CHART_PAD_X, sxFor, seriesIndex, yearTicks } from "./forecastScale";

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
    checkpoints = [3, 6, 9, 12],
    events = [],
    onselectevent,
  }: {
    history: { x: string; y: number }[];
    months: Schemas["ForecastMonth"][];
    currency?: string;
    height?: number;
    /** Fires with the hovered point's date + values, or null on leave. */
    onhover?: (point: { as_of: string; median: number; p10?: number; p90?: number } | null) => void;
    /**
     * Month offsets to mark with a rule and a dot. A prop rather than a local const because the
     * page shows the matching callout tiles and the two must agree — they were separate copies
     * of `[3, 6, 9, 12]` before, which stopped being right the moment the horizon could be 30
     * years and four marks landed inside the first 3% of the axis.
     */
    checkpoints?: number[];
    /**
     * How each event *actually landed* across the simulated paths — realised timing, not the
     * configured window. Relations move dates, so the two genuinely differ, and drawing the input
     * would be a lie about precisely the thing this chart exists to show.
     */
    events?: ChartEvent[];
    /** Fires when a marker is activated, so the page can scroll to that event's editor. */
    onselectevent?: (eventId: number) => void;
  } = $props();

  export type ChartEvent = {
    id: number;
    name: string;
    /** A CSS colour — the person's swatch, or a neutral for a household event. */
    color: string;
    probabilityBps: number;
    /** Month offsets from today. The parent owns the date→index mapping, as it does for `months`. */
    p10: number;
    median: number;
    p90: number;
    /** The p90 ran past the horizon, so the band's right edge must not read as a committed date. */
    truncated: boolean;
  };

  const W = CHART_W;
  // Split by axis. `sx` and pointer-mapping read `padX`; only `sy` reads `padY`. The two are
  // separate so the vertical padding can later grow with the number of event-label lanes —
  // which are packed by x — without the lane packing depending on its own output.
  const padX = CHART_PAD_X;
  const LANE_H = 15;

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
    return sxFor(i, totalPoints);
  }
  function sy(v: number): number {
    const t = (v - minY) / (maxY - minY || 1);
    return padY.t + (height - padY.t - padY.b) * (1 - t);
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

  // Vertical markers at the page's checkpoint months (only the ones within horizon).
  const marks = $derived(
    checkpoints
      .filter((m) => m <= months.length)
      .map((m) => ({
        months: m,
        index: seriesIndex(histLen, m),
        month: months[m - 1],
      }))
  );

  // Year gridlines + labels. A 30-year chart with no year ticks is a shape, not a chart; the
  // three-span first/Today/last footer this replaces said nothing about the twenty-eight years
  // in between.
  const ticks = $derived(yearTicks(histLen, months));

  const zeroY = $derived(minY < 0 ? sy(0) : null);

  // ---- life-event annotations ------------------------------------------------------
  //
  // A single vertical line per event would assert a date the model does not have. The timing is a
  // distribution, so it is drawn as the p10–p90 window with a marker at the median. Indices are
  // clamped into the projection rather than dropped: an event whose window runs past the horizon is
  // drawn to the edge and flagged, because "it may not have happened yet by then" is exactly what a
  // thirty-year chart is being asked.
  function clampIdx(i: number): number {
    return Math.max(histLen - 1, Math.min(totalPoints - 1, i));
  }

  const eventBands = $derived.by(() =>
    events.map((e) => {
      const lo = clampIdx(seriesIndex(histLen, e.p10));
      const mid = clampIdx(seriesIndex(histLen, e.median));
      const hi = clampIdx(seriesIndex(histLen, e.p90));
      const x = sx(lo);
      const p = Math.max(0, Math.min(1, e.probabilityBps / 10_000));
      return {
        ...e,
        mid,
        x,
        w: Math.max(2, sx(hi) - x),
        mx: sx(mid),
        p,
        // Probability drives four channels, because none of them is legible alone on a 240px-tall
        // chart. Fill opacity, marker radius, and a dash pattern — read by area, by size or by
        // texture, a 40%-likely event cannot be mistaken for a 95% one, and the dash survives
        // greyscale. The fourth channel is the number itself, in the label: every visual encoding
        // is a hint, and only the number is an answer.
        fill: 0.05 + 0.14 * p,
        r: 2.4 + 2.6 * p,
        dash: p >= 0.85 ? undefined : p >= 0.6 ? "3 2" : "1.5 2.5",
        filled: p >= 0.85,
      };
    })
  );

  // Greedy first-fit lane packing. Sorted by median x, each label takes the topmost lane whose
  // previous label ended at least 4 units earlier. Two events a month apart therefore stack, while
  // the common case — a handful of events years apart — all lands in lane 0 and costs no vertical
  // space at all.
  const CHAR_W = 5.1;
  const laid = $derived.by(() => {
    const laneEnd: number[] = [];
    return eventBands
      .slice()
      .sort((a, b) => a.mx - b.mx)
      .map((e) => {
        const w = 30 + e.name.length * CHAR_W;
        const left = Math.max(padX.l, Math.min(e.mx - w / 2, W - padX.r - w));
        let lane = laneEnd.findIndex((end) => end <= left - 4);
        if (lane === -1) {
          lane = laneEnd.length;
          laneEnd.push(0);
        }
        laneEnd[lane] = left + w;
        return { ...e, labelX: left, labelW: w, lane };
      });
  });
  const laneCount = $derived(laid.reduce((n, e) => Math.max(n, e.lane + 1), 0));

  // Declared here, below the packing, because it *reads* it. The label lanes grow the top padding,
  // so a single `pad` object would be a reactive cycle: packing reads `sx`, `sy` reads the padding,
  // and the marker positions read `sy`. Splitting by axis breaks it — `sx` never touches `padY` —
  // and the marker y-positions live in their own `$derived` rather than inside the packing.
  const padY = $derived({ t: 14 + laneCount * LANE_H, b: 22 });

  // Separate from `laid` on purpose: this reads `sy`, which reads `padY`, which reads `laneCount`.
  // Folding it into the packing closes that loop.
  const markers = $derived(laid.map((e) => ({ ...e, my: sy(medianY[e.mid] ?? 0) })));

  let evHover = $state<number | null>(null);
  let selectedEvent = $state<number | null>(null);
  const hoveredEvent = $derived(laid.find((e) => e.id === evHover) ?? null);

  function fmtWindow(e: { p10: number; p90: number; truncated: boolean }): string {
    const at = (m: number) => monthAt(seriesIndex(histLen, m))?.as_of ?? "";
    const lo = at(e.p10);
    const hi = at(e.p90);
    const range = lo && hi ? `${formatDate(lo)} – ${formatDate(hi)}` : "unknown";
    return e.truncated ? `${range}, or later` : range;
  }

  // ---- hover (no brush/zoom — a forward projection isn't something you zoom into) ---
  let svgEl = $state<SVGSVGElement | null>(null);
  let hover = $state<number | null>(null);

  function idxFromClientX(clientX: number): number {
    const rect = svgEl!.getBoundingClientRect();
    const vbX = ((clientX - rect.left) / rect.width) * W;
    const plotW = W - padX.l - padX.r;
    const n = totalPoints;
    if (n <= 1) return 0;
    const i = Math.round(((vbX - padX.l) / plotW) * (n - 1));
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
    // One tooltip at a time: two of them fighting for the same forty pixels is unreadable.
    if (evHover !== null) return;
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
      {#each ticks as t (t.index)}
        {#if !t.today}
          <line x1={sx(t.index)} x2={sx(t.index)} y1={padY.t} y2={height - padY.b}
                stroke="var(--border)" stroke-dasharray="1 4" vector-effect="non-scaling-stroke" />
        {/if}
      {/each}
      {#if zeroY !== null}
        <line x1={padX.l} x2={W - padX.r} y1={zeroY} y2={zeroY} stroke="var(--border-strong)"
              stroke-dasharray="3 3" />
      {/if}
      {#if histLen > 0}
        <line x1={sx(histLen - 1)} x2={sx(histLen - 1)} y1={padY.t} y2={height - padY.b}
              stroke="var(--border-strong)" stroke-dasharray="2 3" vector-effect="non-scaling-stroke" />
      {/if}

      <!-- Behind the projection paths, so the median line stays the most legible thing on the
           chart, but in front of the "today" seam so a band starting at today is not cut in half. -->
      <g class="ev-bands" aria-hidden="true">
        {#each eventBands as e (e.id)}
          <g style="--ev:{e.color}">
            <rect
              x={e.x} y={padY.t} width={e.w} height={height - padY.t - padY.b}
              fill="var(--ev, var(--accent))" fill-opacity={e.fill}
              stroke="var(--ev, var(--accent))" stroke-opacity={e.fill * 1.6}
              stroke-dasharray="2 3" vector-effect="non-scaling-stroke" rx="2"
              data-ev-band={e.id}
            />
            <line
              x1={e.mx} x2={e.mx} y1={padY.t} y2={height - padY.b}
              stroke="var(--ev, var(--accent))" stroke-opacity={0.25 + 0.45 * e.p}
              stroke-dasharray={e.dash} vector-effect="non-scaling-stroke"
            />
          </g>
        {/each}
      </g>

      <path d={bandPath} fill="var(--accent)" fill-opacity="0.12" stroke="none" />
      <path d={historyPath} fill="none" stroke="var(--accent)" stroke-width="1.5"
            stroke-linejoin="round" vector-effect="non-scaling-stroke" />
      <path d={projectionPath} fill="none" stroke="var(--accent)" stroke-width="1.5"
            stroke-dasharray="5 4" stroke-linejoin="round" vector-effect="non-scaling-stroke" />

      {#each marks as c (c.months)}
        <line x1={sx(c.index)} x2={sx(c.index)} y1={padY.t} y2={height - padY.b}
              stroke="var(--border)" stroke-dasharray="1 3" vector-effect="non-scaling-stroke" />
        <circle cx={sx(c.index)} cy={sy(c.month.net_worth.median_minor)} r="3" fill="var(--accent)" />
      {/each}

      <g class="ev-marks">
        {#each markers as e (e.id)}
          <g style="--ev:{e.color}" class:sel={selectedEvent === e.id || evHover === e.id}>
            <circle
              cx={e.mx} cy={e.my} r={e.r}
              fill={e.filled ? "var(--ev, var(--accent))" : "var(--bg-elev)"}
              stroke="var(--ev, var(--accent))" stroke-width="1.4"
              stroke-dasharray={e.dash} vector-effect="non-scaling-stroke"
            />
            {#if e.truncated}
              <!-- An open arrow rather than a closed edge: the window runs past the horizon, so the
                   band's right-hand end must not read as a date the model committed to. -->
              <path d="M {W - padX.r - 7} {padY.t + 5} l 5 4 l -5 4"
                    fill="none" stroke="var(--ev, var(--accent))" stroke-opacity="0.7"
                    vector-effect="non-scaling-stroke" />
            {/if}
          </g>
        {/each}
      </g>

      {#if hover !== null}
        <line x1={sx(hover)} x2={sx(hover)} y1={padY.t} y2={height - padY.b}
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
    <!-- Labels are HTML, not SVG <text>. `preserveAspectRatio="none"` stretches the viewBox
         horizontally to fill the container and would stretch glyphs by the same factor — which is
         why every label in this file has always been HTML. It also makes them real focusable
         buttons in DOM order, so typography and keyboard access are solved in one move. -->
    <div class="ev-layer">
      {#each laid as e (e.id)}
        <button
          type="button"
          class="ev-label"
          class:sel={selectedEvent === e.id}
          style="--ev:{e.color};left:{(e.labelX / W) * 100}%;top:{4 + e.lane * LANE_H}px;--o:{0.5 +
            0.5 * e.p}"
          onpointerenter={() => (evHover = e.id)}
          onpointerleave={() => (evHover = null)}
          onfocus={() => (evHover = e.id)}
          onblur={() => (evHover = null)}
          onclick={() => {
            selectedEvent = e.id;
            onselectevent?.(e.id);
          }}
        >
          <span class="ev-dot" class:hollow={!e.filled}></span>
          <span class="ev-name">{e.name}</span>
          <!-- The fourth probability channel, and the only unambiguous one: every visual encoding
               is a hint, the number is the answer. -->
          <span class="ev-pct tabular">{Math.round(e.p * 100)}%</span>
        </button>
      {/each}
    </div>
    {#if hoveredEvent}
      <div
        class="tooltip ev"
        class:below={hoveredEvent.lane * LANE_H < 40}
        style="left:{(hoveredEvent.mx / W) * 100}%;top:{18 + hoveredEvent.lane * LANE_H}px"
      >
        <div class="tt-val">{hoveredEvent.name}</div>
        <div class="tt-range tabular">{Math.round(hoveredEvent.p * 100)}% likely</div>
        <div class="tt-date">
          around {formatDate(monthAt(hoveredEvent.mid)?.as_of ?? "")}
          <span class="faint"> · 80% of runs {fmtWindow(hoveredEvent)}</span>
        </div>
      </div>
    {/if}
  </div>
  {#if laid.length > 0}
    <!-- `role="img"` hides the SVG's children from assistive tech whatever is drawn inside it, so
         the annotations would otherwise be invisible. Same facts, as text. -->
    <ul class="sr-only">
      {#each laid as e (e.id)}
        <li>
          {e.name}: {Math.round(e.p * 100)}% likely, around {formatDate(
            monthAt(e.mid)?.as_of ?? ""
          )}, 80% of runs {fmtWindow(e)}.
        </li>
      {/each}
    </ul>
  {/if}
  <!-- Ticks are HTML, not SVG <text>: `preserveAspectRatio="none"` stretches the viewBox
       horizontally to fill the container, and it would stretch glyphs by the same factor. The
       percentages come from the same scale the SVG uses, so they line up exactly. -->
  <div class="ticks small faint" style="height:14px">
    {#each ticks as t (t.index)}
      <span class="tick" class:today={t.today} style="left:{(sx(t.index) / W) * 100}%">{t.label}</span>
    {/each}
  </div>
{/if}

<style>
  .chart-wrap {
    position: relative;
  }
  .ticks {
    position: relative;
    margin-top: 2px;
  }
  .tick {
    position: absolute;
    transform: translateX(-50%);
    white-space: nowrap;
  }
  /* The seam is the one tick whose position carries meaning on its own, so it is the one that
     stays legible when a neighbouring year label crowds it. */
  .tick.today {
    color: var(--text-muted);
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
  /* The event tooltip hangs off a lane rather than off the median line, so it positions from the
     top instead of flipping around a data point. */
  .tooltip.ev {
    transform: translate(-50%, 0);
    z-index: 4;
  }
  .tooltip.ev.below {
    transform: translate(-50%, 0);
  }
  .ev-layer {
    position: absolute;
    inset: 0;
    /* The layer must not eat the crosshair; only the labels themselves are interactive. */
    pointer-events: none;
  }
  .ev-label {
    all: unset;
    position: absolute;
    pointer-events: auto;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 1px 5px;
    border-radius: 999px;
    white-space: nowrap;
    font-size: 10.5px;
    line-height: 1.3;
    opacity: var(--o);
    color: var(--text-muted);
    background: color-mix(in srgb, var(--bg-elev) 88%, transparent);
    border: 1px solid color-mix(in srgb, var(--ev) 35%, var(--border));
  }
  .ev-label:hover,
  .ev-label.sel {
    opacity: 1;
    color: var(--text);
    background: var(--bg-elev);
  }
  .ev-label:focus-visible {
    outline: 2px solid var(--ev);
    outline-offset: 1px;
    opacity: 1;
  }
  .ev-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ev);
    flex: none;
  }
  /* Hollow mirrors the marker on the chart: the same "less certain" reading in both places. */
  .ev-dot.hollow {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--ev);
  }
  .ev-pct {
    color: var(--text-faint);
    font-size: 9.5px;
  }
  .ev-marks .sel circle {
    stroke-width: 2.2;
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
