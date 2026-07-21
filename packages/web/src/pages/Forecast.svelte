<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../lib/api";
  import ForecastChart from "../lib/charts/ForecastChart.svelte";

  type ResolvedAssumption = Schemas["ResolvedAssumption"];
  type ForecastEvent = Schemas["ForecastEvent"];

  const HORIZONS = [
    { months: 6, label: "6 months" },
    { months: 12, label: "12 months" },
    { months: 24, label: "24 months" },
    { months: 36, label: "36 months" },
  ];
  const CHECKPOINTS = [3, 6, 9, 12];

  let horizon = $state(12);
  let history = $state<{ x: string; y: number }[]>([]);
  let result = $state<Schemas["ForecastResult"] | null>(null);
  let events = $state<ForecastEvent[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let hoverPoint = $state<{ as_of: string; median: number; p10?: number; p90?: number } | null>(
    null
  );

  async function load() {
    loading = true;
    error = null;
    try {
      const today = new Date();
      const from = new Date(today);
      from.setFullYear(from.getFullYear() - 1);
      const [nw, fc, ev] = await Promise.all([
        api.GET("/api/reports/net-worth", {
          params: {
            query: { from: from.toISOString().slice(0, 10), interval: "month" },
          },
        }),
        api.GET("/api/forecast", { params: { query: { horizon_months: horizon } } }),
        api.GET("/api/forecast/events", {}),
      ]);
      history = (nw.data?.points ?? []).map((p) => ({ x: p.as_of, y: p.net_worth_minor }));
      result = fc.data ?? null;
      events = ev.data ?? [];
      if (nw.error || fc.error || ev.error) error = "Failed to load forecast.";
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }
  onMount(load);
  $effect(() => {
    horizon; // re-run whenever the horizon selector changes
    load();
  });

  const currency = $derived(result?.currency ?? "NZD");
  const checkpointMonths = $derived(
    CHECKPOINTS.filter((m) => m <= (result?.months.length ?? 0)).map((m) => ({
      months: m,
      month: result!.months[m - 1],
    }))
  );

  // Only targets the simulation actually resolved an assumption for — excludes cash
  // (pooled) and everyday transaction accounts, so the "add event" form can't be pointed
  // at a target that would silently have no effect.
  const targets = $derived(
    (result?.assumptions ?? []).map((a) => ({
      key: `${a.target_type}:${a.target_id}`,
      target_type: a.target_type,
      target_id: a.target_id,
      label: a.label,
    }))
  );
  const targetLabels = $derived(new Map(targets.map((t) => [t.key, t.label])));
  function targetLabel(e: ForecastEvent): string {
    return targetLabels.get(`${e.target_type}:${e.target_id}`) ?? `#${e.target_id}`;
  }

  function pct(bps: number): string {
    return `${bps >= 0 ? "+" : ""}${(bps / 100).toFixed(1)}%`;
  }
  function sourceLabel(s: Schemas["AssumptionSource"]): string {
    switch (s) {
      case "override":
        return "override";
      case "cron":
        return "from scheduled adjustment";
      case "derived":
        return "from history";
      case "deterministic":
        return "amortisation schedule";
      case "insufficient_history":
        return "not enough history";
    }
  }

  // ---- assumption override editing ------------------------------------------------
  let editingKey = $state<string | null>(null);
  let editForm = $state({ growth: "0", volatility: "0", dividendYield: "0" });

  function startEdit(a: ResolvedAssumption) {
    editingKey = `${a.target_type}:${a.target_id}`;
    editForm = {
      growth: (a.annual_growth_bps / 100).toString(),
      volatility: (a.annual_volatility_bps / 100).toString(),
      dividendYield: ((a.dividend_yield_bps ?? 0) / 100).toString(),
    };
  }
  function cancelEdit() {
    editingKey = null;
  }
  async function saveEdit(a: ResolvedAssumption) {
    const body: Schemas["SaveForecastAssumption"] = {
      target_type: a.target_type,
      target_id: a.target_id,
      annual_growth_bps: Math.round(parseFloat(editForm.growth || "0") * 100),
      annual_volatility_bps: Math.round(parseFloat(editForm.volatility || "0") * 100),
      dividend_yield_bps:
        a.dividend_yield_bps != null
          ? Math.round(parseFloat(editForm.dividendYield || "0") * 100)
          : null,
    };
    const { error: e } = await api.PUT("/api/forecast/assumptions", { body });
    if (e) {
      error = "Failed to save the override.";
      return;
    }
    editingKey = null;
    load();
  }
  async function clearOverride(a: ResolvedAssumption) {
    await api.DELETE("/api/forecast/assumptions/{target_type}/{target_id}", {
      params: { path: { target_type: a.target_type, target_id: a.target_id } },
    });
    load();
  }

  // ---- known future events ----------------------------------------------------------
  let ef = $state({
    targetKey: "",
    kind: "step_change" as Schemas["ForecastEventKind"],
    effective_date: new Date().toISOString().slice(0, 10),
    amount: "",
    label: "",
  });

  async function addEvent() {
    if (!ef.targetKey || !ef.label.trim() || !ef.amount) return;
    const [target_type, target_id] = ef.targetKey.split(":") as [
      Schemas["ForecastTargetType"],
      string,
    ];
    const body: Schemas["SaveForecastEvent"] = {
      target_type,
      target_id: Number(target_id),
      kind: ef.kind,
      effective_date: ef.effective_date,
      amount_minor: Math.round(parseFloat(ef.amount) * 100),
      label: ef.label.trim(),
    };
    const { error: e } = await api.POST("/api/forecast/events", { body });
    if (e) {
      error = "Failed to add the event.";
      return;
    }
    ef.label = "";
    ef.amount = "";
    load();
  }
  async function deleteEvent(id: number) {
    await api.DELETE("/api/forecast/events/{id}", { params: { path: { id } } });
    load();
  }
</script>

<div class="row spread" style="margin-bottom:14px">
  <h1 style="font-size:20px;margin:0">Forecast</h1>
  <div class="row" style="gap:10px">
    <select class="select" style="width:auto" bind:value={horizon}>
      {#each HORIZONS as h}<option value={h.months}>{h.label}</option>{/each}
    </select>
    <button class="btn btn-sm" onclick={load} title="Re-run the simulation">↻ Re-run</button>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:16px">{error}</div>{/if}

<div class="grid cards">
  <section class="card">
    <div class="card-title">
      <h2>Net worth: history &amp; projection</h2>
      <span class="muted small">
        shaded band = P10–P90 across {result ? "simulated paths" : "…"}
      </span>
    </div>
    {#if hoverPoint}
      <div class="stat" style="margin-bottom:10px">
        <div class="value tabular">{formatMoney(hoverPoint.median, currency)}</div>
        <div class="label">
          {formatDate(hoverPoint.as_of)}
          {#if hoverPoint.p10 != null && hoverPoint.p90 != null}
            · range {formatMoney(hoverPoint.p10, currency)} – {formatMoney(hoverPoint.p90, currency)}
          {/if}
        </div>
      </div>
    {/if}
    <ForecastChart {history} months={result?.months ?? []} {currency} onhover={(p) => (hoverPoint = p)} />
  </section>

  {#if checkpointMonths.length > 0}
    <section class="card">
      <h2>Checkpoints</h2>
      <div class="checkpoints">
        {#each checkpointMonths as c (c.months)}
          <div class="checkpoint">
            <div class="cp-label">+{c.months} {c.months === 1 ? "month" : "months"}</div>
            <div class="cp-value tabular">{formatMoney(c.month.net_worth.median_minor, currency)}</div>
            <div class="cp-range tabular small faint">
              {formatMoney(c.month.net_worth.p10_minor, currency)} – {formatMoney(
                c.month.net_worth.p90_minor,
                currency
              )}
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  <section class="card">
    <div class="card-title">
      <h2>Assumptions</h2>
      <span class="muted small">tune any of these — clear an override to go back to the derived default</span>
    </div>
    {#if !result?.assumptions.length}
      <div class="empty">Nothing to forecast yet — add accounts, transactions and categorise your spending.</div>
    {:else}
      <div class="assumption-list">
        {#each result.assumptions as a (a.target_type + ":" + a.target_id)}
          {@const key = a.target_type + ":" + a.target_id}
          <div class="assumption-row">
            <div class="a-main row spread">
              <span class="row" style="gap:8px;min-width:0">
                <span class="badge target-badge">{a.target_type}</span>
                <span class="ell" style="font-weight:560">{a.label}</span>
              </span>
              {#if a.source !== "deterministic"}
                <div class="row" style="gap:14px">
                  <span class="tabular small">growth {pct(a.annual_growth_bps)}/yr</span>
                  <span class="tabular small faint">± {(a.annual_volatility_bps / 100).toFixed(1)}%/yr</span>
                  {#if a.dividend_yield_bps != null}
                    <span class="tabular small faint">yield {(a.dividend_yield_bps / 100).toFixed(1)}%</span>
                  {/if}
                </div>
              {/if}
            </div>
            <div class="a-meta row spread">
              <span class="small faint">{sourceLabel(a.source)}</span>
              {#if a.source !== "deterministic"}
                <div class="row" style="gap:6px">
                  {#if a.source === "override"}
                    <button class="btn btn-sm" onclick={() => clearOverride(a)}>Clear override</button>
                  {/if}
                  <button class="btn btn-sm" onclick={() => (editingKey === key ? cancelEdit() : startEdit(a))}>
                    {editingKey === key ? "Cancel" : "Override"}
                  </button>
                </div>
              {/if}
            </div>
            {#if editingKey === key}
              <div class="edit-form">
                <label class="field">
                  <span class="small faint">Growth %/yr</span>
                  <input class="input tabular" bind:value={editForm.growth} />
                </label>
                <label class="field">
                  <span class="small faint">Volatility %/yr</span>
                  <input class="input tabular" bind:value={editForm.volatility} />
                </label>
                {#if a.dividend_yield_bps != null}
                  <label class="field">
                    <span class="small faint">Dividend yield %</span>
                    <input class="input tabular" bind:value={editForm.dividendYield} />
                  </label>
                {/if}
                <button class="btn btn-primary btn-sm" onclick={() => saveEdit(a)}>Save</button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>

  <section class="card">
    <div class="card-title">
      <h2>Known future changes</h2>
      <span class="muted small">a promotion, a planned bonus, a fixed appreciation — applied exactly, not estimated</span>
    </div>
    <div class="event-form">
      <select class="select" bind:value={ef.targetKey}>
        <option value="" disabled>Target…</option>
        {#each targets as t (t.key)}<option value={t.key}>{t.label}</option>{/each}
      </select>
      <select class="select" bind:value={ef.kind}>
        <option value="step_change">New recurring baseline from date</option>
        <option value="one_off_amount">One-off amount on date</option>
      </select>
      <input class="input" type="date" bind:value={ef.effective_date} />
      <input class="input tabular" placeholder="Amount" bind:value={ef.amount} />
      <input class="input" placeholder="Label" bind:value={ef.label} />
      <button class="btn btn-primary" onclick={addEvent}>Add</button>
    </div>
    {#if result?.assumptions.length === 0}
      <div class="small faint" style="margin-top:8px">Add an account or category first.</div>
    {/if}
    <div class="event-list">
      {#each events as e (e.id)}
        <div class="line row spread">
          <span>
            <span class="badge target-badge">{e.kind === "step_change" ? "step change" : "one-off"}</span>
            {e.label} on {targetLabel(e)}
            <span class="faint small">from {formatDate(e.effective_date)}</span>
          </span>
          <div class="row" style="gap:8px">
            <span class="tabular small">{formatMoney(e.amount_minor, currency)}</span>
            <button class="btn btn-sm btn-danger" onclick={() => deleteEvent(e.id)}>✕</button>
          </div>
        </div>
      {/each}
      {#if events.length === 0}<div class="small faint">None yet.</div>{/if}
    </div>
  </section>
</div>

{#if loading && !result}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{/if}

<style>
  .cards {
    gap: 16px;
  }
  .checkpoints {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 14px;
  }
  .checkpoint {
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }
  .cp-label {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
    margin-bottom: 4px;
  }
  .cp-value {
    font-size: 16px;
    font-weight: 640;
  }
  .cp-range {
    margin-top: 2px;
  }
  .assumption-list {
    display: flex;
    flex-direction: column;
  }
  .assumption-row {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .assumption-row:first-child {
    border-top: none;
  }
  .a-meta {
    margin-top: 2px;
  }
  .target-badge {
    text-transform: capitalize;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .edit-form {
    display: flex;
    gap: 10px;
    align-items: flex-end;
    flex-wrap: wrap;
    margin-top: 8px;
    padding: 10px;
    background: var(--surface-2);
    border-radius: var(--r-sm);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field .input {
    width: 100px;
  }
  .event-form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 10px;
    margin-bottom: 12px;
  }
  .line {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .line:first-of-type {
    border-top: none;
  }
</style>
