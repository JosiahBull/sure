<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, colorFor, type Schemas } from "../lib/api";
  import { filters, activeRange, attributionParam } from "../lib/state.svelte";
  import { navigate } from "../lib/router.svelte";
  import { Tween } from "svelte/motion";
  import { cubicOut } from "svelte/easing";
  import LineChart from "../lib/charts/LineChart.svelte";
  import PieChart from "../lib/charts/PieChart.svelte";
  import Sankey from "../lib/charts/Sankey.svelte";
  import WeightBar from "../lib/charts/WeightBar.svelte";
  import { balances, refresh as refreshBalances } from "../lib/balances.svelte";
  import { groupByKind } from "../lib/balanceGroups";
  // Disabled with the "Net worth by person" card below — re-add `groupByOwner` and
  // `people` here if that card comes back.
  // import { groupByOwner } from "../lib/balanceGroups";
  // import { people } from "../lib/people.svelte";
  import Icon from "../lib/Icon.svelte";
  import FxNotice from "../lib/FxNotice.svelte";

  let nw = $state<Schemas["NetWorthSeries"] | null>(null);
  let breakdown = $state<Schemas["CategoryBreakdown"] | null>(null);
  let sankey = $state<Schemas["SankeyGraph"] | null>(null);
  let flowExpanded = $state(false);
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
    // Whose money these charts describe. Net worth filters *accounts* by owner; the
    // category/flow reports filter *transactions* by effective attribution — see the
    // domain query types for why those differ.
    const attributed_to = attributionParam();
    try {
      const [a, b, s] = await Promise.all([
        api.GET("/api/reports/net-worth", {
          params: { query: { from, to, interval, attributed_to } },
        }),
        api.GET("/api/reports/category-breakdown", {
          params: {
            query: { from, to, include_one_off: filters.includeOneOff, attributed_to },
          },
        }),
        api.GET("/api/reports/sankey", {
          params: {
            query: { from, to, include_one_off: filters.includeOneOff, attributed_to },
          },
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
    filters.attributedTo;
    load();
  });

  // Balance Sheet / Investments show today's balances, not the date-range report — loaded
  // once (shared with the account panel, which may already have triggered this).
  onMount(() => {
    if (!balances.data) refreshBalances();
  });
  let expandedBSKinds = $state(new Set<string>());
  function toggleBSKind(kind: string) {
    const next = new Set(expandedBSKinds);
    if (next.has(kind)) next.delete(kind);
    else next.add(kind);
    expandedBSKinds = next;
  }
  const assetsGrouped = $derived(groupByKind(balances.data?.accounts ?? [], "assets"));
  const liabilitiesGrouped = $derived(groupByKind(balances.data?.accounts ?? [], "debts"));

  /**
   * Net worth split by owner. Account-level, so it comes off the same balances response the
   * cards above use — and it is deliberately *not* driven by the header's "whose money"
   * filter: this card's whole job is the side-by-side comparison, which filtering to one
   * person would collapse.
   *
   * Disabled 2026-08-06: the side-by-side comparison frames a household's finances as two
   * scores to compare, which is not how we want to think about a relationship. Kept rather
   * than deleted in case it earns a place back — `groupByOwner` in lib/balanceGroups.ts is
   * still exported and tested, so this is the only thing to un-comment (plus the markup and
   * the .owner-* CSS, both marked below).
   */
  // const byOwner = $derived(groupByOwner(balances.data?.accounts ?? [], "all"));
  const investmentAccounts = $derived(
    (balances.data?.accounts ?? []).filter((a) => a.class === "investment")
  );
  const investmentTotal = $derived(investmentAccounts.reduce((s, a) => s + a.value_minor, 0));

  // Per-account brokerage snapshots (positions + 30d activity), fetched in parallel once the
  // balances store identifies the investment-class accounts.
  let snapshots = $state<Record<number, Schemas["BrokerageSnapshot"]>>({});
  $effect(() => {
    const ids = investmentAccounts.map((a) => a.account_id);
    if (ids.length === 0) {
      snapshots = {};
      return;
    }
    Promise.all(
      ids.map((id) => api.GET("/api/accounts/{id}/brokerage", { params: { path: { id } } }))
    ).then((results) => {
      const next: Record<number, Schemas["BrokerageSnapshot"]> = {};
      results.forEach((r, i) => {
        if (r.data) next[ids[i]] = r.data;
      });
      snapshots = next;
    });
  });

  // Every position across every investment account, largest first. Market value stays in each
  // position's own trading currency (matching the per-row native-currency convention used in the
  // account panel), so weight% is a naive share of the summed minor units.
  const holdings = $derived(
    Object.values(snapshots)
      .flatMap((s) => s.positions)
      .sort((a, b) => (b.market_value_minor ?? 0) - (a.market_value_minor ?? 0))
  );
  const holdingsValueMinor = $derived(
    holdings.reduce((s, p) => s + (p.market_value_minor ?? 0), 0)
  );

  // Aggregate return over holdings that carry a cost basis; a holding without one is skipped
  // rather than blanking the whole figure. Estimated (average-cost) — see the return-column note.
  const costed = $derived(
    holdings.filter((p) => p.cost_basis_minor != null && p.market_value_minor != null)
  );
  const totalCostMinor = $derived(costed.reduce((s, p) => s + (p.cost_basis_minor ?? 0), 0));
  const totalReturnMinor = $derived(
    costed.reduce((s, p) => s + (p.market_value_minor ?? 0), 0) - totalCostMinor
  );
  const totalReturnPct = $derived(
    totalCostMinor > 0 ? (totalReturnMinor / totalCostMinor) * 100 : null
  );

  // Combined 30-day cash-movement summary across every investment account.
  const activity = $derived(
    Object.values(snapshots).reduce(
      (acc, s) => ({
        contributions_minor: acc.contributions_minor + s.activity_30d.contributions_minor,
        withdrawals_minor: acc.withdrawals_minor + s.activity_30d.withdrawals_minor,
        trades: acc.trades + s.activity_30d.trades,
      }),
      { contributions_minor: 0, withdrawals_minor: 0, trades: 0 }
    )
  );
  const hasSnapshots = $derived(Object.keys(snapshots).length > 0);

  // Currencies missing from the snapshots' own totals, deduped across accounts, plus the
  // oldest rate date any of them used — the pessimistic one, since a single stale account is
  // enough to make the combined figures stale.
  const snapshotList = $derived(Object.values(snapshots));
  const holdingsUnconverted = $derived([
    ...new Set(snapshotList.flatMap((s) => s.unconverted)),
  ]);
  const holdingsRatesAsOf = $derived(
    snapshotList
      .map((s) => s.rates_as_of)
      .filter((d): d is string => d != null)
      .sort()[0] ?? null
  );

  const currency = $derived(breakdown?.currency ?? nw?.currency ?? "NZD");
  // Whole-dollar money (no cents) — keeps the donut centre from overflowing on hover.
  const money0 = (v: number) => formatMoney(v, currency).replace(/\.\d+$/, "");
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
  // overview's current range. Shared by the pie arcs, their legend rows and the Sankey, so
  // all three open the same slice the same way.
  //
  // A null id is the uncategorised bucket — every source of one means that specific slice,
  // never "no category filter" (`reports.rs` gives it the sentinel key 0 and the API renders
  // that back as `category_id: null`). So it maps to the transactions page's own `none`
  // filter, not to an omitted param, which would land on *every* transaction instead of the
  // handful the user clicked. `kind` still narrows it to income or outgoings, since a null
  // category alone can't tell an uncategorised income transaction from an expense one.
  function goToCategory(categoryId: number | null, kind?: "income" | "expense") {
    const p = new URLSearchParams();
    p.set("category", categoryId == null ? "none" : String(categoryId));
    if (kind) p.set("type", kind);
    p.set("range", filters.range);
    navigate(`/transactions?${p.toString()}`);
  }
</script>

{#if error}
  <div class="error-banner" style="margin-bottom:16px">{error}</div>
{/if}

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

  <div class="grid two">
    {#each [{ title: "Where money went", slices: expenseSlices, total: totalExpense }, { title: "Where money came from", slices: incomeSlices, total: totalIncome }] as panel, pi}
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
              centerValue={money0(panel.total)}
              centerLabel="total"
              active={hovered[pi]}
              onhover={(i) => (hovered[pi] = i)}
              onselect={(i) => goToCategory(panel.slices[i].categoryId, pi === 0 ? "expense" : "income")}
              format={money0}
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
                    onclick={() => goToCategory(s.categoryId, pi === 0 ? "expense" : "income")}
                    title="View {s.label} transactions"
                  >
                    <span class="row" style="gap:8px;min-width:0">
                      <span class="dot" style="background:{s.color}"></span>
                      <span class="ell">{s.label}</span>
                    </span>
                    <span class="tabular">{formatMoney(s.value, currency)}</span>
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
        <div class="row" style="gap:10px">
          <span class="muted small">income → cash flow → expenses</span>
          <!-- The chart shows as many category levels as the width can render legibly, so a
               narrow card gets fewer. This is where the rest of them live. -->
          <button type="button" class="btn btn-sm" onclick={() => (flowExpanded = true)}>Expand</button>
        </div>
      </div>
      <Sankey
        nodes={sankey.nodes}
        links={sankeyLinks}
        format={(v) => formatMoney(v, currency)}
        onselect={goToCategory}
      />
    </section>
  {/if}

  <!-- Three category levels per side is up to seven columns, which is tight inside a card.
       The same chart, given a window to breathe in — the previous app had the same escape
       hatch. -->
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
          <button type="button" class="btn btn-sm" onclick={() => (flowExpanded = false)}>Close</button>
        </div>
        <Sankey
          nodes={sankey.nodes}
          links={sankeyLinks}
          height="calc(85dvh - 72px)"
          format={(v) => formatMoney(v, currency)}
          onselect={(id, kind) => {
            flowExpanded = false;
            goToCategory(id, kind);
          }}
        />
      </div>
    </div>
  {/if}

  <!-- "Net worth by person" — disabled 2026-08-06, see the byOwner note in the script block.
       Un-commenting this needs the byOwner derived, the groupByOwner/people imports, and the
       .owner-* CSS restored too.
  {#if balances.data && people.list.length > 1 && byOwner.groups.length > 1}
    <section class="card">
      <div class="card-title">
        <h2>Net worth by person</h2>
        <span class="muted small tabular">
          {formatMoney(byOwner.totalMinor, balances.data.currency)}
        </span>
      </div>
      <div class="owner-cards">
        {#each byOwner.groups as g (g.key)}
          <div class="owner-card" style={g.color ? `--owner:${g.color}` : undefined}>
            <span class="owner-name">{g.label}</span>
            <span class="owner-total tabular" class:neg={g.totalMinor < 0}>
              {formatMoney(g.totalMinor, balances.data.currency)}
            </span>
            <span class="muted small">
              {g.accounts.length} account{g.accounts.length === 1 ? "" : "s"}
            </span>
          </div>
        {/each}
      </div>
      <p class="muted small" style="margin:10px 2px 0">
        Joint accounts are their own column rather than split in half — nothing in the data
        says what the split is.
      </p>
    </section>
  {/if}
  -->

  {#if balances.data && (assetsGrouped.groups.length || liabilitiesGrouped.groups.length)}
    <div class="grid two">
      {#each [{ title: "Assets", grouped: assetsGrouped }, { title: "Liabilities", grouped: liabilitiesGrouped }] as panel}
        <section class="card">
          <div class="card-title">
            <h2>{panel.title}</h2>
            <span class="muted small tabular">
              {formatMoney(panel.grouped.totalMinor, balances.data?.currency)}
            </span>
          </div>
          {#if panel.grouped.groups.length === 0}
            <div class="empty">Nothing here yet.</div>
          {:else}
            <WeightBar
              segments={panel.grouped.groups.map((g) => ({
                label: g.label,
                color: colorFor(g.kind),
                weightPct: g.weightPct,
              }))}
            />
            <ul class="legend" style="margin-top:12px">
              {#each panel.grouped.groups as g (g.kind)}
                <li>
                  <button type="button" class="legend-row" onclick={() => toggleBSKind(g.kind)}>
                    <span class="row" style="gap:6px;min-width:0">
                      <Icon name={expandedBSKinds.has(g.kind) ? "chevron-down" : "chevron-right"} size={14} />
                      <span class="dot" style="background:{colorFor(g.kind)}"></span>
                      <span class="ell">{g.label}</span>
                    </span>
                    <span class="row" style="gap:8px">
                      <span class="small faint tabular">{g.weightPct.toFixed(1)}%</span>
                      <span class="tabular">{formatMoney(g.totalMinor, balances.data?.currency)}</span>
                    </span>
                  </button>
                  {#if expandedBSKinds.has(g.kind)}
                    <ul class="sub-list">
                      {#each g.accounts as a (a.account_id)}
                        <li class="row spread small" style="padding:4px 8px 4px 30px">
                          <span class="ell muted">{a.name}</span>
                          <span class="tabular">{formatMoney(a.value_minor, a.currency_code)}</span>
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}
        </section>
      {/each}
    </div>
  {/if}

  {#if investmentAccounts.length > 0}
    <section class="card">
      <div class="card-title">
        <h2>Investments</h2>
      </div>
      <div class="stat" style="margin-bottom:4px">
        <div class="value tabular">{formatMoney(investmentTotal, balances.data?.currency)}</div>
      </div>
      <!-- A holding priced in a currency with no rate is listed below in its own currency but
           is not inside any account total — the same figure `revalue` refuses to persist. -->
      <FxNotice
        unconverted={holdingsUnconverted}
        ratesAsOf={holdingsRatesAsOf}
        currency={balances.data?.currency}
      />
      {#if totalReturnPct != null}
        <div class="small" style="margin-bottom:14px">
          <span class="muted">Total return:</span>
          <span
            class="tabular"
            class:pos={totalReturnMinor >= 0}
            class:neg={totalReturnMinor < 0}
            style="font-weight:620"
            title="Estimated — average cost basis"
          >
            {formatMoney(totalReturnMinor, balances.data?.currency)}
            ({totalReturnPct >= 0 ? "+" : ""}{totalReturnPct.toFixed(1)}%)
          </span>
        </div>
      {/if}

      {#if holdings.length > 0}
        <table class="table holdings">
          <thead>
            <tr>
              <th>Holding</th>
              <th class="num">Weight</th>
              <th class="num">Value</th>
              <th class="num" title="Estimated — average cost basis">Return</th>
            </tr>
          </thead>
          <tbody>
            {#each holdings as p (p.exchange + ":" + p.ticker)}
              <tr>
                <td>
                  <span class="row" style="gap:10px;min-width:0">
                    <span class="avatar">{p.ticker.slice(0, 2).toUpperCase()}</span>
                    <span class="hold-name">
                      <span class="ell" style="font-weight:560">{p.ticker}</span>
                      <span class="ell small faint">{p.name ?? p.exchange}</span>
                    </span>
                  </span>
                </td>
                <td class="num tabular faint">
                  {holdingsValueMinor > 0 && p.market_value_minor != null
                    ? ((p.market_value_minor / holdingsValueMinor) * 100).toFixed(1) + "%"
                    : "—"}
                </td>
                <td class="num tabular">
                  {p.market_value_minor != null
                    ? formatMoney(p.market_value_minor, p.currency_code)
                    : "—"}
                </td>
                <td
                  class="num tabular"
                  class:pos={p.return_pct != null && p.return_pct >= 0}
                  class:neg={p.return_pct != null && p.return_pct < 0}
                >
                  {p.return_pct != null
                    ? (p.return_pct >= 0 ? "+" : "") + p.return_pct.toFixed(1) + "%"
                    : "—"}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {:else}
        <ul class="legend">
          {#each investmentAccounts as a (a.account_id)}
            <li>
              <button
                type="button"
                class="legend-row"
                onclick={() => navigate(`/transactions?account=${a.account_id}`)}
              >
                <span class="ell">{a.name}</span>
                <span class="tabular">{formatMoney(a.value_minor, a.currency_code)}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}

      {#if hasSnapshots}
        <div class="activity">
          <div class="activity-head faint">Last 30 days activity</div>
          <div class="activity-stats">
            <div class="astat">
              <span class="faint small">Contributions</span>
              <span class="tabular">
                {formatMoney(activity.contributions_minor, balances.data?.currency)}
              </span>
            </div>
            <div class="astat">
              <span class="faint small">Withdrawals</span>
              <span class="tabular">
                {formatMoney(activity.withdrawals_minor, balances.data?.currency)}
              </span>
            </div>
            <div class="astat">
              <span class="faint small">Trades</span>
              <span class="tabular">{activity.trades}</span>
            </div>
          </div>
        </div>
      {/if}
    </section>
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

  /* Disabled with the "Net worth by person" card — kept so restoring it is one un-comment.
     One column per household member, plus joint. Wraps rather than scrolls: a household is
     two or three people, not a table.

  .owner-cards {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
  }
  .owner-card {
    flex: 1 1 160px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-left: 3px solid var(--owner, var(--border));
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .owner-name {
    font-size: 13px;
    font-weight: 650;
    color: var(--owner, var(--text));
  }
  .owner-total {
    font-size: 18px;
    font-weight: 600;
  }
  .owner-total.neg {
    color: var(--negative);
  }
  */

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
  .sub-list {
    list-style: none;
    margin: 2px 0 4px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 13px;
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

  /* Investments holdings table */
  .holdings th.num,
  .holdings td.num {
    text-align: right;
    white-space: nowrap;
  }
  .avatar {
    flex: none;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--text-muted);
    font-size: 11px;
    font-weight: 600;
  }
  .hold-name {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.25;
  }

  /* Last-30-days activity strip */
  .activity {
    margin-top: 14px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }
  .activity-head {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin-bottom: 10px;
  }
  .activity-stats {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }
  .astat {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  @media (max-width: 480px) {
    .activity-stats {
      grid-template-columns: 1fr;
    }
  }
</style>
