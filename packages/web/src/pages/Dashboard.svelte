<script lang="ts">
  import { api, formatMoney, formatDate, colorFor, type Schemas } from "../lib/api";
  import { filters, activeRange } from "../lib/state.svelte";
  import { navigate } from "../lib/router.svelte";
  import { Tween } from "svelte/motion";
  import { cubicOut } from "svelte/easing";
  import LineChart from "../lib/charts/LineChart.svelte";
  import PieChart from "../lib/charts/PieChart.svelte";
  import Sankey from "../lib/charts/Sankey.svelte";

  let nw = $state<Schemas["NetWorthSeries"] | null>(null);
  let breakdown = $state<Schemas["CategoryBreakdown"] | null>(null);
  let sankey = $state<Schemas["SankeyGraph"] | null>(null);
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
      const [a, b, s] = await Promise.all([
        api.GET("/api/reports/net-worth", { params: { query: { from, to, interval } } }),
        api.GET("/api/reports/category-breakdown", {
          params: { query: { from, to, include_one_off: filters.includeOneOff } },
        }),
        api.GET("/api/reports/sankey", {
          params: { query: { from, to, include_one_off: filters.includeOneOff } },
        }),
      ]);
      nw = a.data ?? null;
      breakdown = b.data ?? null;
      sankey = s.data ?? null;
      if (a.error || b.error || s.error) error = "Failed to load reports.";
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    // Reload whenever the global filters change (preset, one-off, or brush window).
    filters.range;
    filters.includeOneOff;
    filters.custom;
    load();
  });

  const currency = $derived(breakdown?.currency ?? nw?.currency ?? "NZD");
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

  const toSlice = (c: Schemas["CategoryTotal"]) => ({
    label: c.name,
    value: c.total_minor,
    color: c.color ?? colorFor(c.category_id ?? c.name),
    categoryId: c.category_id ?? null,
  });
  const expenseSlices = $derived((breakdown?.expense ?? []).map(toSlice));
  const incomeSlices = $derived((breakdown?.income ?? []).map(toSlice));
  const totalExpense = $derived(expenseSlices.reduce((s, c) => s + c.value, 0));
  const totalIncome = $derived(incomeSlices.reduce((s, c) => s + c.value, 0));
  const sankeyLinks = $derived((sankey?.links ?? []).map((l) => ({ ...l, value: l.value_minor })));

  // Hovered slice per pie ([expense, income]) — shared between the donut and its legend.
  let hovered = $state<(number | null)[]>([null, null]);

  // Jump to the transactions page filtered to this category (its whole subtree) over the
  // overview's current range. A null id (the uncategorised slice) filters by range only.
  function goToCategory(categoryId: number | null | undefined) {
    const p = new URLSearchParams();
    if (categoryId != null) p.set("category", String(categoryId));
    p.set("range", filters.range);
    navigate(`/transactions?${p.toString()}`);
  }
</script>

{#if error}
  <div class="error-banner" style="margin-bottom:16px">{error}</div>
{/if}

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
  </section>

  <div class="grid two">
    {#each [{ title: "Where money went", slices: expenseSlices, total: totalExpense, cls: "neg" }, { title: "Where money came from", slices: incomeSlices, total: totalIncome, cls: "pos" }] as panel, pi}
      <section class="card">
        <h2>{panel.title}</h2>
        {#if panel.slices.length === 0}
          <div class="empty">Nothing here yet.</div>
        {:else}
          <div class="row" style="gap:18px;align-items:flex-start">
            <PieChart
              slices={panel.slices}
              size={150}
              thickness={26}
              centerValue={formatMoney(panel.total, currency).replace(/\.\d+$/, "")}
              centerLabel="total"
              active={hovered[pi]}
              onhover={(i) => (hovered[pi] = i)}
              onselect={(i) => goToCategory(panel.slices[i].categoryId)}
              format={(v) => formatMoney(v, currency)}
            />
            <ul class="legend grow">
              {#each panel.slices.slice(0, 6) as s, si}
                <li>
                  <button
                    type="button"
                    class="legend-row"
                    class:dim={hovered[pi] !== null && hovered[pi] !== si}
                    onpointerenter={() => (hovered[pi] = si)}
                    onpointerleave={() => (hovered[pi] = null)}
                    onfocus={() => (hovered[pi] = si)}
                    onblur={() => (hovered[pi] = null)}
                    onclick={() => goToCategory(s.categoryId)}
                    title="View {s.label} transactions"
                  >
                    <span class="row" style="gap:8px;min-width:0">
                      <span class="dot" style="background:{s.color}"></span>
                      <span class="ell">{s.label}</span>
                    </span>
                    <span class="tabular {panel.cls}">{formatMoney(s.value, currency)}</span>
                  </button>
                </li>
              {/each}
            </ul>
          </div>
        {/if}
      </section>
    {/each}
  </div>

  {#if sankey && sankey.links.length}
    <section class="card">
      <div class="card-title">
        <h2>Money flow</h2>
        <span class="muted small">income → cash flow → expenses</span>
      </div>
      <Sankey nodes={sankey.nodes} links={sankeyLinks} />
    </section>
  {/if}
</div>

{#if loading && !nw}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{/if}

<style>
  .cards {
    gap: 16px;
  }
  .two {
    grid-template-columns: 1fr 1fr;
  }
  @media (max-width: 720px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
  .legend {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: 13.5px;
  }
  .legend-row {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 5px 6px;
    border: none;
    border-radius: var(--r-sm);
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: opacity 0.15s ease, background 0.15s ease;
  }
  .legend-row:hover,
  .legend-row:focus-visible {
    background: var(--hover);
  }
  .legend-row.dim {
    opacity: 0.4;
  }
  .dot {
    width: 10px;
    height: 10px;
    border-radius: 3px;
    flex: none;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

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
