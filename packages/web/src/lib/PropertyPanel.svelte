<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "./api";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  let position = $state<Schemas["EquityPosition"] | null>(null);
  let accounts = $state<Schemas["Account"][]>([]);
  let valueInput = $state("");
  let linkId = $state<number | "">("");
  let busy = $state(false);

  async function load() {
    const [p, a] = await Promise.all([
      api.GET("/api/accounts/{id}/equity-position", { params: { path: { id: accountId } } }),
      api.GET("/api/accounts", {}),
    ]);
    position = p.data ?? null;
    accounts = a.data ?? [];
  }
  onMount(load);

  // Liabilities not already secured against this asset — candidates to link.
  const linkable = $derived(
    accounts.filter((a) => a.class === "liability" && a.secured_by_account_id !== accountId)
  );
  const ccy = $derived(position?.currency ?? "NZD");
  const pct = $derived(Math.round(position?.paid_off_pct ?? 0));

  async function setValue() {
    const major = parseFloat(valueInput.replace(/[^0-9.-]/g, ""));
    if (isNaN(major)) return;
    busy = true;
    await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: accountId } },
      body: { as_of: new Date().toISOString().slice(0, 10), value_minor: Math.round(major * 100) },
    });
    valueInput = "";
    await load();
    onchange?.();
    busy = false;
  }

  async function link(id: number) {
    await api.PUT("/api/accounts/{id}/secured-by", {
      params: { path: { id } },
      body: { secured_by_account_id: accountId },
    });
    linkId = "";
    load();
  }
  async function unlink(id: number) {
    await api.PUT("/api/accounts/{id}/secured-by", { params: { path: { id } }, body: { secured_by_account_id: null } });
    load();
  }
</script>

<div class="property">
  {#if position}
    <div class="row spread" style="margin-bottom:8px">
      <div class="stat" style="gap:2px">
        <div class="value tabular" style="font-size:20px">{pct}% <span class="faint" style="font-size:13px">paid off</span></div>
        <div class="small faint">
          value {formatMoney(position.value_minor, ccy)} · owe {formatMoney(position.total_debt_minor, ccy)} · equity
          <strong style="color:var(--text)">{formatMoney(position.equity_minor, ccy)}</strong>
        </div>
      </div>
    </div>
    <div class="bar"><span style="width:{pct}%"></span></div>

    <div class="row wrap" style="gap:8px;margin:12px 0">
      <input class="input tabular grow" style="min-width:120px" placeholder="Set home value (e.g. 850000)" bind:value={valueInput} />
      <button class="btn btn-sm" onclick={setValue} disabled={busy || !valueInput.trim()}>Set value</button>
    </div>

    <div class="small muted" style="margin-bottom:4px">Secured loans</div>
    {#each position.liabilities as l (l.account_id)}
      <div class="row spread line">
        <span>{l.name} <span class="badge">{l.kind.replace(/_/g, " ")}</span></span>
        <div class="row" style="gap:8px">
          <span class="tabular neg">{formatMoney(l.balance_minor, ccy)}</span>
          <button class="btn btn-sm" onclick={() => unlink(l.account_id)} title="Unlink">✕</button>
        </div>
      </div>
    {/each}
    {#if position.liabilities.length === 0}
      <div class="small faint">No loans linked yet.</div>
    {/if}

    {#if linkable.length}
      <div class="row" style="gap:8px;margin-top:10px">
        <select class="select" style="width:auto" bind:value={linkId}>
          <option value="">Link a loan…</option>
          {#each linkable as a}<option value={a.id}>{a.name}</option>{/each}
        </select>
        <button class="btn btn-sm btn-primary" onclick={() => linkId !== "" && link(linkId)} disabled={linkId === ""}>Link</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .property {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  .bar {
    height: 8px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--positive);
    border-radius: 999px;
  }
  .line {
    padding: 7px 0;
    border-top: 1px solid var(--border);
  }
</style>
