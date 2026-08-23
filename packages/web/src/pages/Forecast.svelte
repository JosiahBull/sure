<script lang="ts">
  // The forecast page is a shell: the chart on top, always mounted, and tabs under it for the
  // things you edit.
  //
  // The chart deliberately does NOT live in a tab. The tabs are what you change; the chart is
  // what you are changing it toward — turn an assumption up and watch the band above it widen,
  // with no tab switch, no SVG remount and no refetch of the net-worth history.
  //
  // All loading lives here and the tabs get props, so the chart and the editors cannot disagree
  // about what they are showing.
  import { untrack } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../lib/api";
  import { streamForecast } from "../lib/forecastStream";
  import ForecastChart from "../lib/charts/ForecastChart.svelte";
  import FxNotice from "../lib/FxNotice.svelte";
  import ProjectionTab from "./forecast/ProjectionTab.svelte";
  import AssumptionsTab from "./forecast/AssumptionsTab.svelte";
  import IncomeTab from "./forecast/IncomeTab.svelte";
  import LifeEventsTab from "./forecast/LifeEventsTab.svelte";
  import { people, personColor } from "../lib/people.svelte";
  import { queryParams, setQueryParam } from "../lib/router.svelte";
  import { HORIZONS, checkpointsFor, historyMonthsFor } from "../lib/charts/forecastScale";

  const TABS = [
    { key: "projection", label: "Projection" },
    { key: "income", label: "Income" },
    { key: "events", label: "Life events" },
    { key: "assumptions", label: "Assumptions" },
  ] as const;
  type TabKey = (typeof TABS)[number]["key"];

  // Tab and horizon live in the hash *query*, not the path. `App.svelte` keys the active page on
  // `router.path.split("?")[0]` and remounts with `{#key activePath}`, so a query param changes
  // state without tearing the page down and refetching — where `#/forecast/assumptions` would
  // have remounted on every click. It also makes a bookmark, a shared link and a Playwright
  // baseline all reproducible.
  const tab = $derived.by<TabKey>(() => {
    const t = queryParams().get("tab");
    // An unrecognised value is the default view rather than an error: a stale bookmark or a
    // renamed tab should land somewhere useful.
    return (TABS.find((x) => x.key === t)?.key ?? "projection") as TabKey;
  });
  const horizon = $derived.by(() => {
    const h = Number(queryParams().get("h"));
    return HORIZONS.some((x) => x.months === h) ? h : HORIZONS[0].months;
  });

  let history = $state<{ x: string; y: number }[]>([]);
  let result = $state<Schemas["ForecastResult"] | null>(null);
  let error = $state<string | null>(null);
  // Paths simulated so far and the total this run will reach, straight off the stream. `total`
  // is the *clamped* count, so a 30-year horizon reports the 2 000 it will really run rather
  // than whatever was asked for.
  let completed = $state(0);
  let total = $state(0);
  // A run is in flight and has not yet delivered its first snapshot, so what is on screen
  // belongs to the previous query. The chart stays up, dimmed, rather than blanking: the first
  // snapshot is ten paths in — under a percent of the run — so this is a couple of frames, and
  // a flash of empty axes would be worse than a moment of visibly-superseded ones.
  let stale = $state(false);
  let streaming = $state(true);
  // Bumped by "Re-run" so the effect below re-fires on an unchanged horizon.
  let runNonce = $state(0);
  type Readout = { as_of: string; median: number; p10?: number; p90?: number };
  let hoverPoint = $state<Readout | null>(null);

  async function load(signal: AbortSignal) {
    error = null;
    // `untrack`, and it is load-bearing: this runs inside the `$effect` below, so a *tracked*
    // read of `result` here would make the effect depend on state the stream is about to
    // write — it re-fired on every snapshot, aborting and restarting its own run forever.
    stale = untrack(() => result != null);
    streaming = true;
    completed = 0;
    total = 0;
    try {
      // History shares the projection's axis, so the window scales with the horizon — a fixed
      // year against thirty projected ones is a 3% sliver. See `historyMonthsFor`.
      const from = new Date();
      from.setMonth(from.getMonth() - historyMonthsFor(horizon));
      // History first, and *awaited* rather than raced with the stream. The chart draws one
      // combined series whose seam is the last historical point re-used as projected month 0 —
      // so `months` without `history` is not a state it can render, and letting a snapshot land
      // first produced `NaN` in the path data. It costs about 15 ms against a first snapshot
      // that takes 30-70, which is the right price for not having to teach the chart a state
      // that never otherwise occurs.
      const nw = await api.GET("/api/reports/net-worth", {
        params: { query: { from: from.toISOString().slice(0, 10), interval: "month" } },
      });
      if (signal.aborted) return;
      history = (nw.data?.points ?? []).map((p) => ({ x: p.as_of, y: p.net_worth_minor }));
      if (nw.error) error = "Failed to load net-worth history.";

      // The projection arrives in pieces: a rough one after ten paths, then 100, 1 000, and
      // every 1 000 after that, with counter-only ticks in between to move the bar. Each
      // snapshot is a *complete* projection over fewer paths — the same months, wider bands —
      // so everything downstream (the tiles, the markers, the chart) takes it unchanged.
      await streamForecast({ horizon_months: horizon }, signal, (p, kind) => {
        completed = p.completed;
        total = p.total;
        if (kind === "snapshot" && p.result) {
          result = p.result;
          stale = false;
        }
      });
    } catch (e) {
      // A superseded run is not a failure — the effect below aborted it on purpose.
      if (signal.aborted) return;
      error = e instanceof Error ? e.message : String(e);
    } finally {
      if (!signal.aborted) {
        streaming = false;
        stale = false;
      }
    }
  }

  // One run per (horizon, Re-run), aborted when either changes or the page goes away. Not
  // `onMount` *and* an effect: an effect already runs on mount, and having both is what made
  // every visit to this page start two full simulations — the second of which the compute pool
  // could shed as a 503, reported to the user as "Failed to load forecast".
  $effect(() => {
    horizon; // re-run whenever the horizon changes — but not when the tab does
    runNonce;
    const controller = new AbortController();
    load(controller.signal);
    return () => controller.abort();
  });

  const currency = $derived(result?.currency ?? "NZD");
  // Clamped, because a tick can land marginally ahead of the snapshot that follows it.
  const pct = $derived(total > 0 ? Math.min(100, (completed / total) * 100) : 0);
  /** The hovered point, or — with the pointer off the chart — the last actual. */
  const readout = $derived.by<Readout | null>(() => {
    if (hoverPoint) return hoverPoint;
    const last = history.at(-1);
    return last ? { as_of: last.x, median: last.y } : null;
  });
  // Derived from the horizon and passed to the chart as well, so the tiles and the marks on the
  // chart cannot disagree about which months they describe.
  const checkpoints = $derived(checkpointsFor(horizon));

  /**
   * The events, shaped for the chart: *realised* timing, and a colour per person.
   *
   * Realised, not configured — a timing rule can push an event years later than what was typed, and
   * the chart has to show where it actually lands. An event that never occurred on any path has no
   * timing to draw, so it is dropped rather than pinned to its expected date.
   */
  const chartEvents = $derived(
    (result?.events ?? [])
      .filter((e) => e.month_median != null && e.occurrence_rate_bps > 0)
      .map((e) => {
        const who = e.person_id != null ? people.list.find((p) => p.id === e.person_id) : null;
        return {
          id: e.event_id,
          name: e.label,
          color: who ? personColor(who) : "var(--text-muted)",
          // The *realised* rate, so an `only_if` that never fires reads as unlikely on the chart
          // even when the event itself was configured as a certainty.
          probabilityBps: e.occurrence_rate_bps,
          p10: e.month_p10 ?? e.month_median!,
          median: e.month_median!,
          p90: e.month_p90 ?? e.month_median!,
          truncated: e.truncated,
        };
      })
  );

  /**
   * Debts the projection expects to clear, drawn through the same marker machinery as events —
   * both answer "this lands around here, give or take", and a second visual language for the
   * same question would be one to learn for no reason.
   *
   * Two differences it does carry. The id is negated so it cannot collide with a real event's
   * (the chart keys and selects on it), and `kind` makes the marker inert, because a milestone
   * has no row to open. `cleared_rate_bps` stands in for probability: it is not a chance of
   * happening but the share of paths that got there inside the horizon, which is the same
   * "how much of this is committed" signal the opacity and dash already encode.
   */
  const chartMilestones = $derived(
    (result?.milestones ?? [])
      // Only when the median path actually gets there. Below half, "the P50 month" is the median
      // of a minority — at a 12-month horizon Josiah's loan clears on one path in a thousand, and
      // drawing that as a milestone puts a payoff date on the chart for something that, in this
      // window, does not happen. It reappears on its own once the horizon is long enough to hold
      // it, which is the honest behaviour: the marker tracks the question being asked.
      .filter((m) => m.cleared_rate_bps >= 5_000)
      .map((m) => {
      const who = m.person_id != null ? people.list.find((p) => p.id === m.person_id) : null;
      // Two accounts really are both called "Student loan"; the owner is what tells them apart.
      const name = who ? `${who.name} ${m.label.toLowerCase()} paid off` : `${m.label} paid off`;
      return {
        id: -m.account_id,
        name,
        color: who ? personColor(who) : "var(--text-muted)",
        probabilityBps: m.cleared_rate_bps,
        p10: m.month_p10,
        median: m.month_p50,
        p90: m.month_p90,
        truncated: m.cleared_rate_bps < 10_000,
        kind: "milestone" as const,
      };
    })
  );

  /**
   * Ansam's teaching scale, and any other dated raise, drawn on the chart.
   *
   * These come back already filtered to the streams the projection modelled and to steps inside
   * the horizon, so there is nothing to guard here. `probabilityBps` is a flat 10 000 because a
   * step *is* certain — that is the difference from a milestone, and it shows up as a solid rule
   * rather than a dashed one. p10 and p90 equal the median for the same reason: one date, no band.
   *
   * The id is offset past the milestones' negative range so the three marker sources cannot
   * collide on the key the chart selects by.
   */
  const chartPaySteps = $derived(
    (result?.pay_steps ?? []).map((s, i) => {
      const who = s.person_id != null ? people.list.find((p) => p.id === s.person_id) : null;
      // The step's own label is the useful half ("Step 5 + 1 unit"); the stream name is the
      // fallback for a step that was never named.
      const what = s.label?.trim() || `${s.stream_label} rises`;
      return {
        id: -100_000 - i,
        name: who ? `${who.name} — ${what}` : what,
        color: who ? personColor(who) : "var(--text-muted)",
        probabilityBps: 10_000,
        p10: s.month,
        median: s.month,
        p90: s.month,
        truncated: false,
        kind: "milestone" as const,
      };
    })
  );

  /** Set when a chart marker is clicked, so the Life events tab opens that row. */
  let focusEventId = $state<number | null>(null);
  function selectEvent(id: number) {
    focusEventId = id;
    setQueryParam("tab", "events");
  }
</script>

<div class="row spread" style="margin-bottom:14px">
  <h1 style="font-size:20px;margin:0">Forecast</h1>
  <div class="row" style="gap:10px">
    <select
      class="select"
      style="width:auto"
      value={horizon}
      onchange={(e) => setQueryParam("h", (e.currentTarget as HTMLSelectElement).value)}
    >
      {#each HORIZONS as h (h.months)}<option value={h.months}>{h.label}</option>{/each}
    </select>
    <button
      class="btn btn-sm"
      onclick={() => (runNonce += 1)}
      disabled={streaming}
      title="Re-run the simulation">↻ Re-run</button
    >
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:16px">{error}</div>{/if}

{#if result?.warnings.length}
  <!-- Things that changed meaning rather than things that went wrong: linking an account discards
       its measured rate, and saying so is the only way that is visible. -->
  <div class="notice" style="margin-bottom:16px">
    {#each result.warnings as w (w)}<div>{w}</div>{/each}
  </div>
{/if}

<section class="card" style="margin-bottom:16px">
  <div class="card-title">
    <h2>Net worth: history &amp; projection</h2>
    {#if streaming}
      <!-- The caption becomes the progress readout while a run is in flight: it is the same
           fact ("across how many paths") either way, so a separate widget beside it would be
           two places to read one number. -->
      <span class="muted small run">
        <span class="bar" aria-hidden="true"><span class="fill" style="width:{pct}%"></span></span>
        {#if total === 0}
          <!-- The load phase, before a single path has run: resolving assumptions is a dozen
               queries and is most of the wait on a short horizon, so it gets said rather than
               shown as "0 / 0". -->
          <span>reading your ledger…</span>
        {:else}
          <span class="tabular"
            >{completed.toLocaleString()} / {total.toLocaleString()} paths</span
          >
        {/if}
      </span>
    {:else}
      <span class="muted small">
        shaded band = P10–P90 across {result
          ? `${result.simulations.toLocaleString()} paths`
          : "…"}
      </span>
    {/if}
  </div>
  <!-- Always rendered, never `{#if hoverPoint}`. Mounting this on hover pushed the chart down by
       its own height, which moved the line out from under the pointer and immediately unhovered
       it — the chart flickered up and down as long as the cursor sat near the top of the plot.
       At rest it reads the latest actual, so the space is occupied by something useful rather
       than reserved by an empty box. -->
  <div class="stat readout" style="margin-bottom:10px">
    <div class="value tabular">{readout ? formatMoney(readout.median, currency) : "—"}</div>
    <div class="label">
      {#if readout}
        {formatDate(readout.as_of)}
        {#if readout.p10 != null && readout.p90 != null}
          · range {formatMoney(readout.p10, currency)} – {formatMoney(readout.p90, currency)}
        {:else if !hoverPoint}
          · latest actual
        {/if}
      {:else}
        &nbsp;
      {/if}
    </div>
  </div>
  <div class="plot" class:stale>
    <ForecastChart
      {history}
      months={result?.months ?? []}
      {currency}
      {checkpoints}
      events={[...chartEvents, ...chartMilestones, ...chartPaySteps]}
      onselectevent={selectEvent}
      onhover={(p) => (hoverPoint = p)}
    />
  </div>
  <!-- An account whose currency has no rate is left out of the simulation entirely rather
       than projected from a parity starting balance, which would be wrong in every month of
       every path. Both the history line and the bands are then partial. -->
  <FxNotice unconverted={result?.unconverted ?? []} ratesAsOf={result?.rates_as_of} {currency} />
</section>

<div class="tabs-nav" role="tablist">
  {#each TABS as t (t.key)}
    <button
      class="tab-btn"
      class:active={tab === t.key}
      role="tab"
      aria-selected={tab === t.key}
      onclick={() => setQueryParam("tab", t.key)}>{t.label}</button
    >
  {/each}
</div>

{#if tab === "projection"}
  <ProjectionTab {result} {checkpoints} {currency} />
{:else if tab === "income"}
  <IncomeTab {result} {currency} />
{:else if tab === "events"}
  <LifeEventsTab {result} {currency} onchanged={() => (runNonce += 1)} {focusEventId} />
{:else if tab === "assumptions"}
  <AssumptionsTab {result} {currency} onchanged={() => (runNonce += 1)} onerror={(m) => (error = m)} />
{/if}

{#if streaming && !result}
  <!-- Only before the *first* snapshot of the *first* run: after that the bar in the card
       header is the affordance, and a spinner under the page as well would say it twice. The
       old `loading && !result` said it once and then never again, which is why changing the
       horizon used to have no visible effect at all until the numbers moved. -->
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{/if}

<style>
  /* One line, always: the range only appears on a projected point, and letting it wrap would
     reintroduce the very height change this readout was made permanent to avoid. */
  .readout .label {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* Lifted from Transactions.svelte rather than promoted to app.css: two copies is not yet a
     pattern, and the repo's precedent (.chip-row, .swatches, .confirm) is that page-local styles
     stay page-local until a third caller turns up. */
  .tabs-nav {
    display: inline-flex;
    gap: 2px;
    padding: 3px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--r);
    margin-bottom: 16px;
  }
  .tab-btn {
    appearance: none;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: 13px;
    font-weight: 560;
    padding: 5px 12px;
    border-radius: calc(var(--r) - 4px);
    cursor: pointer;
  }
  .tab-btn:hover {
    color: var(--text);
  }
  .tab-btn.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow);
  }
  .notice {
    padding: 10px 12px;
    border-radius: var(--r-sm);
    border: 1px solid color-mix(in srgb, var(--warn) 45%, var(--border));
    background: color-mix(in srgb, var(--warn) 7%, transparent);
    font-size: 12.5px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .tab-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  /* Superseded, not gone. See `stale`. */
  .plot {
    transition: opacity 120ms ease-out;
  }
  .plot.stale {
    opacity: 0.4;
  }
  .run {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  /* The same 6px pill as EquityPanel's grant bar. Two copies is not yet a pattern — the repo's
     precedent is that page-local styles stay page-local until a third caller turns up. */
  .run .bar {
    width: 90px;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .run .fill {
    display: block;
    height: 100%;
    background: var(--accent);
    /* Ticks arrive about every 1% of the run, so the fill is already smooth; this only keeps
       the four snapshot-sized jumps from reading as stutter. */
    transition: width 90ms linear;
  }
</style>
