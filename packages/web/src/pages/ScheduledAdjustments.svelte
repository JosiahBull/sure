<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatDate, type Schemas } from "../lib/api";

  let accounts = $state<Schemas["Account"][]>([]);
  let crons = $state<Schemas["Cron"][]>([]);
  let error = $state<string | null>(null);

  let cf = $state({
    name: "",
    account_id: 0,
    kind: "appreciation",
    rate: "1",
    amount: "",
    start_date: new Date().toISOString().slice(0, 10),
  });

  async function load() {
    const [a, cr] = await Promise.all([api.GET("/api/accounts", {}), api.GET("/api/crons", {})]);
    accounts = a.data ?? [];
    crons = cr.data ?? [];
    if (accounts.length && !cf.account_id) cf.account_id = accounts[0].id;
  }
  onMount(load);

  async function addCron() {
    if (!cf.name.trim() || !cf.account_id) return;
    const fixed = cf.kind === "fixed_transaction";
    const body: Schemas["SaveCron"] = {
      name: cf.name,
      account_id: cf.account_id,
      kind: cf.kind,
      start_date: cf.start_date,
      enabled: true,
      rate_bps: fixed ? null : Math.round(parseFloat(cf.rate || "0") * 100),
      amount_minor: fixed ? Math.round(parseFloat(cf.amount || "0") * 100) : null,
    };
    const { error: e } = await api.POST("/api/crons", { body });
    if (e) {
      error = "Failed to add scheduled adjustment.";
      return;
    }
    cf.name = "";
    load();
  }
  async function runCron(id: number) {
    await api.POST("/api/crons/{id}/run", { params: { path: { id } } });
    load();
  }
  async function delCron(id: number) {
    await api.DELETE("/api/crons/{id}", { params: { path: { id } } });
    load();
  }
</script>

<h1 style="font-size:20px;margin-bottom:14px">Scheduled adjustments</h1>
{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

<section class="card">
  <p class="muted small" style="margin-top:0">
    e.g. a house that appreciates 1%/yr, or a recurring subscription. Applied monthly and
    recorded so each period can be undone.
  </p>
  <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(120px,1fr));gap:10px;margin-bottom:12px">
    <input class="input" placeholder="Name" bind:value={cf.name} />
    <select class="select" bind:value={cf.account_id}>
      {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
    </select>
    <select class="select" bind:value={cf.kind}>
      <option value="appreciation">Appreciation</option>
      <option value="depreciation">Depreciation</option>
      <option value="interest">Interest</option>
      <option value="fixed_transaction">Fixed transaction</option>
    </select>
    {#if cf.kind === "fixed_transaction"}
      <input class="input tabular" placeholder="Amount (−spend)" bind:value={cf.amount} />
    {:else}
      <input class="input tabular" placeholder="Rate %/yr" bind:value={cf.rate} />
    {/if}
    <input class="input" type="date" bind:value={cf.start_date} />
    <button class="btn btn-primary" onclick={addCron}>Add</button>
  </div>
  {#each crons as c (c.id)}
    <div class="line row spread">
      <span>
        {c.name} <span class="badge">{c.kind}</span>
        {#if c.last_run_on}<span class="faint small">last: {formatDate(c.last_run_on)}</span>{/if}
      </span>
      <div class="row" style="gap:8px">
        <button class="btn btn-sm" onclick={() => runCron(c.id)}>Run</button>
        <button class="btn btn-sm btn-danger" onclick={() => delCron(c.id)}>✕</button>
      </div>
    </div>
  {/each}
  {#if crons.length === 0}<div class="small faint">None yet.</div>{/if}
</section>

<style>
  .line {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .line:first-of-type {
    border-top: none;
  }
</style>
