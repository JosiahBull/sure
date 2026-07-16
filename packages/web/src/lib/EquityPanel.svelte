<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "./api";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  let equity = $state<Schemas["AccountEquity"] | null>(null);
  let showGrant = $state(false);
  let busy = $state(false);
  let grant = $state({
    company: "",
    grant_date: new Date().toISOString().slice(0, 10),
    quantity: "",
    strike: "",
    unit_value: "",
  });

  async function load() {
    const { data } = await api.GET("/api/accounts/{id}/equity", {
      params: { path: { id: accountId } },
    });
    equity = data ?? null;
  }
  onMount(load);

  async function addGrant() {
    const q = parseInt(grant.quantity, 10);
    if (!grant.company.trim() || !q) return;
    await api.POST("/api/accounts/{id}/equity-grants", {
      params: { path: { id: accountId } },
      body: {
        company: grant.company,
        grant_date: grant.grant_date,
        quantity: q,
        strike_minor: Math.round(parseFloat(grant.strike || "0") * 100),
        vest_months: 48,
        cliff_months: 12,
        unit_value_minor: grant.unit_value ? Math.round(parseFloat(grant.unit_value) * 100) : null,
      },
    });
    showGrant = false;
    grant.company = "";
    grant.quantity = "";
    load();
  }

  async function revalue() {
    busy = true;
    await api.POST("/api/accounts/{id}/equity/revalue", { params: { path: { id: accountId } } });
    await load();
    onchange?.();
    busy = false;
  }

  const pct = (g: Schemas["VestingStatus"]) => (g.quantity ? Math.round((g.vested / g.quantity) * 100) : 0);
</script>

<div class="equity">
  {#if equity}
    <div class="row spread" style="margin-bottom:8px">
      <span class="muted small">
        Vested value <strong class="tabular" style="color:var(--text)"
          >{formatMoney(equity.total_intrinsic_minor, equity.currency_code)}</strong
        >
      </span>
      <div class="row" style="gap:8px">
        <button class="btn btn-sm" onclick={() => (showGrant = !showGrant)}>+ Grant</button>
        <button class="btn btn-sm" onclick={revalue} disabled={busy}>Revalue</button>
      </div>
    </div>

    {#each equity.grants as g (g.grant_id)}
      <div class="grant">
        <div class="row spread">
          <strong>{g.company}</strong>
          <span class="tabular small">{formatMoney(g.intrinsic_value_minor, g.currency_code)}</span>
        </div>
        <div class="bar"><span style="width:{pct(g)}%"></span></div>
        <div class="row spread small faint">
          <span>{g.vested.toLocaleString()} / {g.quantity.toLocaleString()} vested ({pct(g)}%)</span>
          <span>{g.exercised.toLocaleString()} exercised · {g.vested_unexercised.toLocaleString()} exercisable</span>
        </div>
      </div>
    {/each}
    {#if equity.grants.length === 0}
      <div class="small faint" style="padding:6px 0">No grants yet.</div>
    {/if}

    {#if showGrant}
      <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(110px,1fr));margin-top:10px">
        <label class="field">Company<input class="input" bind:value={grant.company} /></label>
        <label class="field">Grant date<input class="input" type="date" bind:value={grant.grant_date} /></label>
        <label class="field">Quantity<input class="input tabular" bind:value={grant.quantity} /></label>
        <label class="field">Strike<input class="input tabular" placeholder="0.00" bind:value={grant.strike} /></label>
        <label class="field">Unit value<input class="input tabular" placeholder="0.00" bind:value={grant.unit_value} /></label>
        <div style="display:flex;align-items:flex-end">
          <button class="btn btn-primary" style="width:100%" onclick={addGrant}>Add</button>
        </div>
      </div>
    {/if}
  {/if}
</div>

<style>
  .equity {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  .grant {
    padding: 8px 0;
    border-top: 1px solid var(--border);
  }
  .bar {
    height: 6px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
    margin: 6px 0;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: 999px;
  }
</style>
