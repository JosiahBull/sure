<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../lib/api";
  import { activeRange, filters, periodLinkParams } from "../lib/state.svelte";
  import { navigate } from "../lib/router.svelte";
  import { Tween } from "svelte/motion";
  import { cubicOut } from "svelte/easing";
  import LineChart from "../lib/charts/LineChart.svelte";
  import Sankey, { type SankeyTarget } from "../lib/charts/Sankey.svelte";
  import FxNotice from "../lib/FxNotice.svelte";
  import StaleFeedNotice from "../lib/StaleFeedNotice.svelte";

  let nw = $state<Schemas["NetWorthSeries"] | null>(null);
  let sankey = $state<Schemas["SankeyGraph"] | null>(null);
  let flowExpanded = $state(false);
  let forciblyIncludeAll = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);

  /** Sample finer as the window narrows so a zoomed-in range stays legible. */
  function intervalFor(from?: string, to?: string): "day" | "week" | "month" {
    if (!from || !to) return "month";
    const days = (new Date(to).getTime() - new Date(from).getTime()) / 86_400_000;
    if (days <= 31) return "day";
    if (days <= 180) return "week";
    return "month";
  }

  async function load() {
    loading = true;
    error = null;
    hoverIndex = null;
    const { from, to } = activeRange();
    const interval = intervalFor(from, to);
    try {
      const [a, b] = await Promise.all([
        api.GET("/api/reports/net-worth", { params: { query: { from, to, interval } } }),
        api.GET("/api/reports/sankey", { params: { query: { from, to, include_excluded_from_cashflow: forciblyIncludeAll } } }),
      ]);
      nw = a.data ?? null;
      sankey = b.data ?? null;
      if (a.error || b.error) error = "Failed to load reports.";
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    // Reload whenever the global filters change (preset or brush window).
    filters.range;
    filters.custom;
    forciblyIncludeAll;
    load();
  });

  /** Bank connections whose stale balances affect both reports. */
  let providers = $state<Schemas["Provider"][]>([]);
  onMount(async () => {
    // Silent on failure: this is a footnote about the page, and a household with no connections
    // at all is the common case. A broken request must not put an error banner over working
    // charts.
    const { data } = await api.GET("/api/providers", {});
    providers = data ?? [];
  });
  const currency = $derived(sankey?.currency ?? nw?.currency ?? "NZD");
  const points = $derived((nw?.points ?? []).map((p) => ({ x: p.as_of, y: p.net_worth_minor })));
  const latest = $derived(nw?.points.at(-1) ?? null);
  const first = $derived(nw?.points[0] ?? null);

  // The point feeding the headline stat: whatever the chart cursor is over, else the latest.
  let hoverIndex = $state<number | null>(null);
  const activePoint = $derived(
    (hoverIndex != null ? nw?.points[hoverIndex] : null) ?? latest
  );
  const activeChange = $derived(
    activePoint && first ? activePoint.net_worth_minor - first.net_worth_minor : 0
  );
  const inspecting = $derived(hoverIndex != null && nw != null && hoverIndex < nw.points.length);

  // Smoothly tween the headline numbers so scrubbing the chart animates them.
  // First real value snaps in (duration 0) so the at-rest view is stable.
  const tNet = new Tween(0, { duration: 260, easing: cubicOut });
  const tAssets = new Tween(0, { duration: 260, easing: cubicOut });
  const tLiab = new Tween(0, { duration: 260, easing: cubicOut });
  const tChange = new Tween(0, { duration: 260, easing: cubicOut });
  let primed = false;
  $effect(() => {
    const p = activePoint;
    if (!p) return;
    const opts = primed ? undefined : { duration: 0 };
    tNet.set(p.net_worth_minor, opts);
    tAssets.set(p.assets_minor, opts);
    tLiab.set(p.liabilities_minor, opts);
    tChange.set(activeChange, opts);
    primed = true;
  });

  const cashIn = $derived((sankey?.links ?? []).filter((l) => l.target === "center" && l.source !== "deficit").reduce((sum, l) => sum + l.value_minor, 0));
  const cashOut = $derived((sankey?.links ?? []).filter((l) => l.source === "center" && l.target !== "savings").reduce((sum, l) => sum + l.value_minor, 0));
  const sankeyLinks = $derived((sankey?.links ?? []).map((l) => ({ ...l, value: l.value_minor })));

  function goToCategory(categoryId: number | null, kind?: "income" | "expense") {
    filters.includeExcludedFromCashflow = forciblyIncludeAll;
    const p = new URLSearchParams(periodLinkParams());
    p.set("cashflow", "1");
    if (forciblyIncludeAll) p.set("include_excluded", "1");
    p.set("category", categoryId == null ? "none" : String(categoryId));
    if (kind) p.set("type", kind);
    navigate(`/transactions?${p.toString()}`);
  }

  /** Keep the chart period when opening an account, including the default period. */
  function goToAccountInPeriod(accountId: number, kind?: "income" | "expense") {
    filters.includeExcludedFromCashflow = forciblyIncludeAll;
    const p = new URLSearchParams(periodLinkParams());
    if (!p.has("range") && !p.has("start")) p.set("range", filters.range);
    p.set("cashflow", "1");
    if (forciblyIncludeAll) p.set("include_excluded", "1");
    p.set("cashflow_account", String(accountId));
    if (kind) p.set("type", kind);
    navigate(`/transactions?${p.toString()}`);
  }

  const openFlow = (t: SankeyTarget) =>
    t.t === "account" ? goToAccountInPeriod(t.accountId, t.kind) : goToCategory(t.categoryId, t.kind);
</script>

{#if error}
  <div class="error-banner" style="margin-bottom:16px">{error}</div>
{/if}

<StaleFeedNotice {providers} href="#/settings/providers" />

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") flowExpanded = false;
  }}
/>

<div class="grid cards">
  <section class="card">
    <div class="card-title">
      <h2>Net worth</h2>
      {#if latest}
        <span class="badge">
          <span class="delta" class:pos={activeChange >= 0} class:neg={activeChange < 0}>
            {activeChange >= 0 ? "▲" : "▼"}
            {formatMoney(Math.round(Math.abs(tChange.current)), currency)}
          </span>
          {inspecting && activePoint ? `to ${formatDate(activePoint.as_of)}` : "this period"}
        </span>
      {/if}
    </div>
    {#if latest && activePoint}
      <div class="stat" class:live={inspecting} style="margin-bottom:10px">
        <div class="value tabular">{formatMoney(Math.round(tNet.current), currency)}</div>
        <div class="label">
          {#if inspecting}<span class="on">{formatDate(activePoint.as_of)}</span> · {/if}assets
          {formatMoney(Math.round(tAssets.current), currency)} · liabilities
          {formatMoney(Math.round(tLiab.current), currency)}
        </div>
      </div>
    {/if}
    <LineChart
      {points}
      {currency}
      onhover={(i) => (hoverIndex = i)}
      onbrush={(r) => (filters.custom = { from: r.from, to: r.to })}
    />
    <!-- Accounts the series could not convert are missing from every point above; saying so
         is the difference between an incomplete figure and a wrong one. -->
    <FxNotice unconverted={nw?.unconverted ?? []} ratesAsOf={nw?.rates_as_of} {currency} />
  </section>

  {#if sankey}
    <section class="card">
      <div class="card-title">
        <h2>Money flow</h2>
        <div class="row" style="gap:10px">
          <label class="row small"><input type="checkbox" bind:checked={forciblyIncludeAll} /> Forcibly Include All</label>
          <!-- The chart shows as many category levels as the width can render legibly, so a
               narrow card gets fewer. This is where the rest of them live. -->
          <button type="button" class="btn btn-sm" onclick={() => (flowExpanded = true)}>Expand</button>
        </div>
      </div>
      <div class="cash-summary">
        <span>Cash in <strong>{formatMoney(cashIn, currency)}</strong></span>
        <span>Cash out <strong>{formatMoney(cashOut, currency)}</strong></span>
        <span class:pos={cashIn >= cashOut} class:neg={cashIn < cashOut}>
          {cashIn >= cashOut ? "Cash left over" : "Cash shortfall"}
          <strong>{formatMoney(Math.abs(cashIn - cashOut), currency)}</strong>
        </span>
      </div>
      <table aria-label="Monthly cashflow">
        <thead><tr><th>Month</th><th>Cash in</th><th>Cash out</th><th>Left over / shortfall</th></tr></thead>
        <tbody>
          {#each sankey.months as m}
            <tr>
              <td>{m.month}</td>
              <td>{formatMoney(m.inflow_minor, currency)}</td>
              <td>{formatMoney(m.outflow_minor, currency)}</td>
              <td class:pos={m.inflow_minor >= m.outflow_minor} class:neg={m.inflow_minor < m.outflow_minor}>
                {formatMoney(m.inflow_minor - m.outflow_minor, currency)}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#if sankey.links.length}
      <Sankey
        nodes={sankey.nodes}
        links={sankeyLinks}
        format={(v) => formatMoney(v, currency)}
        onselect={openFlow}
      />
      {:else}<p class="muted">No cash movements in this period.</p>{/if}
      <FxNotice unconverted={sankey.unconverted ?? []} currency={sankey.currency} />
    </section>
  {/if}

  {#if flowExpanded && sankey}
    <div
      class="overlay"
      role="presentation"
      onclick={(e) => {
        if (e.target === e.currentTarget) flowExpanded = false;
      }}
    >
      <div class="modal" role="dialog" aria-modal="true" aria-label="Money flow">
        <div class="card-title">
          <h2>Money flow</h2>
          <div class="row" style="gap:10px">
            <button type="button" class="btn btn-sm" onclick={() => (flowExpanded = false)}>Close</button>
          </div>
        </div>
        <Sankey
          nodes={sankey.nodes}
          links={sankeyLinks}
          height="calc(85dvh - 72px)"
          format={(v) => formatMoney(v, currency)}
          onselect={(t) => {
            flowExpanded = false;
            openFlow(t);
          }}
        />
      </div>
    </div>
  {/if}


</div>

{#if loading && !nw}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{/if}

<style>
  /* Expanded money-flow view. Mirrors the overlay/modal shell the account modals use, but
     sized to the viewport rather than a form: the whole point is horizontal room. */
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(2px);
  }
  .overlay .modal {
    display: flex;
    flex-direction: column;
    width: min(1650px, 96vw);
    padding: 16px;
    border-radius: var(--r-lg);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.35);
  }

  .cards { gap: 16px; }
  .cash-summary { display: flex; flex-wrap: wrap; gap: 16px 32px; margin-bottom: 16px; }
  .cash-summary strong { display: block; font-size: 20px; font-variant-numeric: tabular-nums; }
  table { width: 100%; margin-bottom: 20px; font-variant-numeric: tabular-nums; }
  th, td { text-align: right; }
  th:first-child, td:first-child { text-align: left; }
  .pos { color: var(--positive); }
  .neg { color: var(--negative); }
  @container main (max-width: 520px) { th, td { padding: 6px 3px; font-size: 11px; } }

  /* Headline stat + badge shift subtly while the chart is being inspected. */
  .badge {
    gap: 0.34em; /* space between the coloured delta and the muted suffix */
  }
  .badge .delta {
    font-weight: 620;
    transition: color 0.18s ease;
  }
  .badge .delta.pos {
    color: var(--positive);
  }
  .badge .delta.neg {
    color: var(--negative);
  }
  .stat .value {
    transition: color 0.18s ease;
  }
  .stat.live .value {
    color: var(--accent);
  }
  .stat .label .on {
    color: var(--text-muted);
    font-weight: 600;
  }

</style>
