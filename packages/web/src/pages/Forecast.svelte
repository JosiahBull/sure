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
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../lib/api";
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
    return HORIZONS.some((x) => x.months === h) ? h : 12;
  });

  let history = $state<{ x: string; y: number }[]>([]);
  let result = $state<Schemas["ForecastResult"] | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let hoverPoint = $state<{ as_of: string; median: number; p10?: number; p90?: number } | null>(
    null
  );

  async function load() {
    loading = true;
    error = null;
    try {
      // History shares the projection's axis, so the window scales with the horizon — a fixed
      // year against thirty projected ones is a 3% sliver. See `historyMonthsFor`.
      const from = new Date();
      from.setMonth(from.getMonth() - historyMonthsFor(horizon));
      const [nw, fc] = await Promise.all([
        api.GET("/api/reports/net-worth", {
          params: {
            query: { from: from.toISOString().slice(0, 10), interval: "month" },
          },
        }),
        api.GET("/api/forecast", { params: { query: { horizon_months: horizon } } }),
      ]);
      history = (nw.data?.points ?? []).map((p) => ({ x: p.as_of, y: p.net_worth_minor }));
      result = fc.data ?? null;
      if (nw.error || fc.error) error = "Failed to load forecast.";
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }
  onMount(load);
  $effect(() => {
    horizon; // re-run whenever the horizon changes — but not when the tab does
    load();
  });

  const currency = $derived(result?.currency ?? "NZD");
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
    <button class="btn btn-sm" onclick={load} title="Re-run the simulation">↻ Re-run</button>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:16px">{error}</div>{/if}

<section class="card" style="margin-bottom:16px">
  <div class="card-title">
    <h2>Net worth: history &amp; projection</h2>
    <span class="muted small">
      shaded band = P10–P90 across {result ? `${result.simulations.toLocaleString()} paths` : "…"}
    </span>
  </div>
  {#if hoverPoint}
    <div class="stat" style="margin-bottom:10px">
      <div class="value tabular">{formatMoney(hoverPoint.median, currency)}</div>
      <div class="label">
        {formatDate(hoverPoint.as_of)}
        {#if hoverPoint.p10 != null && hoverPoint.p90 != null}
          · range {formatMoney(hoverPoint.p10, currency)} – {formatMoney(
            hoverPoint.p90,
            currency
          )}
        {/if}
      </div>
    </div>
  {/if}
  <ForecastChart
    {history}
    months={result?.months ?? []}
    {currency}
    {checkpoints}
    events={chartEvents}
    onselectevent={selectEvent}
    onhover={(p) => (hoverPoint = p)}
  />
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
  <IncomeTab {result} {currency} onchanged={load} />
{:else if tab === "events"}
  <LifeEventsTab {result} {currency} onchanged={load} {focusEventId} />
{:else if tab === "assumptions"}
  <AssumptionsTab {result} {currency} onchanged={load} onerror={(m) => (error = m)} />
{/if}

{#if loading && !result}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{/if}

<style>
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
  .tab-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
