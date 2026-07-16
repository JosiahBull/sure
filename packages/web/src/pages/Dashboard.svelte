<script lang="ts">
  import { api, formatMoney, colorFor, type Schemas } from "../lib/api";
  import { filters, rangeDates } from "../lib/state.svelte";
  import LineChart from "../lib/charts/LineChart.svelte";
  import PieChart from "../lib/charts/PieChart.svelte";
  import Sankey from "../lib/charts/Sankey.svelte";

  let nw = $state<Schemas["NetWorthSeries"] | null>(null);
  let breakdown = $state<Schemas["CategoryBreakdown"] | null>(null);
  let sankey = $state<Schemas["SankeyGraph"] | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  async function load() {
    loading = true;
    error = null;
    const { from, to } = rangeDates(filters.range);
    try {
      const [a, b, s] = await Promise.all([
        api.GET("/api/reports/net-worth", { params: { query: { from, to, interval: "month" } } }),
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
    // Reload whenever the global filters change.
    filters.range;
    filters.includeOneOff;
    load();
  });

  const currency = $derived(breakdown?.currency ?? nw?.currency ?? "NZD");
  const points = $derived((nw?.points ?? []).map((p) => ({ x: p.as_of, y: p.net_worth_minor })));
  const latest = $derived(nw?.points.at(-1) ?? null);
  const first = $derived(nw?.points[0] ?? null);
  const change = $derived(latest && first ? latest.net_worth_minor - first.net_worth_minor : 0);

  const toSlice = (c: Schemas["CategoryTotal"]) => ({
    label: c.name,
    value: c.total_minor,
    color: c.color ?? colorFor(c.category_id ?? c.name),
  });
  const expenseSlices = $derived((breakdown?.expense ?? []).map(toSlice));
  const incomeSlices = $derived((breakdown?.income ?? []).map(toSlice));
  const totalExpense = $derived(expenseSlices.reduce((s, c) => s + c.value, 0));
  const totalIncome = $derived(incomeSlices.reduce((s, c) => s + c.value, 0));
  const sankeyLinks = $derived((sankey?.links ?? []).map((l) => ({ ...l, value: l.value_minor })));
</script>

{#if error}
  <div class="error-banner" style="margin-bottom:16px">{error}</div>
{/if}

<div class="grid cards">
  <section class="card">
    <div class="card-title">
      <h2>Net worth</h2>
      {#if latest}
        <span class="badge" class:pos={change >= 0} class:neg={change < 0}>
          {change >= 0 ? "▲" : "▼"}
          {formatMoney(Math.abs(change), currency)} this period
        </span>
      {/if}
    </div>
    {#if latest}
      <div class="stat" style="margin-bottom:10px">
        <div class="value tabular">{formatMoney(latest.net_worth_minor, currency)}</div>
        <div class="label">
          assets {formatMoney(latest.assets_minor, currency)} · liabilities
          {formatMoney(latest.liabilities_minor, currency)}
        </div>
      </div>
    {/if}
    <LineChart {points} />
  </section>

  <div class="grid two">
    {#each [{ title: "Where money went", slices: expenseSlices, total: totalExpense, cls: "neg" }, { title: "Where money came from", slices: incomeSlices, total: totalIncome, cls: "pos" }] as panel}
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
            />
            <ul class="legend grow">
              {#each panel.slices.slice(0, 6) as s}
                <li class="row spread">
                  <span class="row" style="gap:8px;min-width:0">
                    <span class="dot" style="background:{s.color}"></span>
                    <span class="ell">{s.label}</span>
                  </span>
                  <span class="tabular {panel.cls}">{formatMoney(s.value, currency)}</span>
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
    gap: 9px;
    font-size: 13.5px;
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
</style>
