<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../lib/api";
  import EquityPanel from "../lib/EquityPanel.svelte";
  import PropertyPanel from "../lib/PropertyPanel.svelte";

  let balances = $state<Schemas["BalancesReport"] | null>(null);
  let currencies = $state<Schemas["Currency"][]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let showAdd = $state(false);
  let expanded = $state<number | null>(null);

  const KINDS: { value: string; label: string }[] = [
    { value: "bank", label: "Bank" },
    { value: "cash", label: "Cash" },
    { value: "savings", label: "Savings" },
    { value: "credit_card", label: "Credit card" },
    { value: "revolving_credit", label: "Revolving credit" },
    { value: "mortgage", label: "Mortgage" },
    { value: "student_loan", label: "Student loan" },
    { value: "loan", label: "Loan" },
    { value: "vehicle", label: "Vehicle" },
    { value: "real_estate", label: "Real estate" },
    { value: "shares_nz", label: "Shares (NZ)" },
    { value: "shares_us", label: "Shares (US)" },
    { value: "shares_private", label: "Shares (private)" },
    { value: "asset", label: "Other asset" },
    { value: "liability", label: "Other liability" },
  ];
  const CLASSES = [
    { key: "cash", label: "Cash" },
    { key: "investment", label: "Investments" },
    { key: "asset", label: "Assets" },
    { key: "liability", label: "Liabilities" },
  ];
  const kindLabel = (k: string) => KINDS.find((x) => x.value === k)?.label ?? k;

  let form = $state({ name: "", kind: "bank", currency_code: "NZD" });

  async function load() {
    loading = true;
    const [b, c] = await Promise.all([
      api.GET("/api/reports/balances", {}),
      api.GET("/api/currencies", {}),
    ]);
    balances = b.data ?? null;
    currencies = c.data ?? [];
    loading = false;
  }
  onMount(load);

  const inClass = (cls: string) => (balances?.accounts ?? []).filter((a) => a.class === cls);

  async function add() {
    if (!form.name.trim()) {
      error = "Account name is required.";
      return;
    }
    const { error: e } = await api.POST("/api/accounts", {
      body: {
        name: form.name,
        kind: form.kind as Schemas["SaveAccount"]["kind"],
        currency_code: form.currency_code,
        archived: false,
        sort_order: 0,
      },
    });
    if (e) {
      error = "Failed to add account.";
      return;
    }
    form.name = "";
    showAdd = false;
    load();
  }

  async function del(id: number) {
    await api.DELETE("/api/accounts/{id}", { params: { path: { id } } });
    if (expanded === id) expanded = null;
    load();
  }
</script>

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <div>
    <h1 style="font-size:20px">Accounts</h1>
    {#if balances}
      <div class="muted small">
        Net worth <strong class="tabular" style="color:var(--text)"
          >{formatMoney(balances.total_minor, balances.currency)}</strong
        >
      </div>
    {/if}
  </div>
  <button class="btn btn-primary btn-sm" onclick={() => (showAdd = !showAdd)}>
    {showAdd ? "Close" : "+ Add account"}
  </button>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if showAdd}
  <section class="card" style="margin-bottom:14px">
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr))">
      <label class="field">Name<input class="input" bind:value={form.name} /></label>
      <label class="field">Type
        <select class="select" bind:value={form.kind}>
          {#each KINDS as k}<option value={k.value}>{k.label}</option>{/each}
        </select>
      </label>
      <label class="field">Currency
        <select class="select" bind:value={form.currency_code}>
          {#each currencies as c}<option value={c.code}>{c.code}</option>{/each}
        </select>
      </label>
    </div>
    <div class="row" style="justify-content:flex-end;margin-top:12px">
      <button class="btn btn-primary" onclick={add}>Create</button>
    </div>
  </section>
{/if}

{#if loading && !balances}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  <div class="grid" style="gap:14px">
    {#each CLASSES as cls}
      {@const list = inClass(cls.key)}
      {#if list.length}
        <section class="card">
          <h2>{cls.label}</h2>
          {#each list as a (a.account_id)}
            <div class="acct row spread">
              <div class="row" style="gap:10px;min-width:0">
                <span class="ell">{a.name}</span>
                <span class="badge">{kindLabel(a.kind)}</span>
                {#if a.kind === "shares_private" || a.class === "asset"}
                  <button class="btn btn-sm" onclick={() => (expanded = expanded === a.account_id ? null : a.account_id)}>
                    {expanded === a.account_id ? "Hide" : "Equity"}
                  </button>
                {/if}
              </div>
              <div class="row" style="gap:12px">
                <span class="tabular" class:neg={a.value_minor < 0}>{formatMoney(a.value_minor, a.currency_code)}</span>
                <button class="btn btn-sm btn-danger" onclick={() => del(a.account_id)}>✕</button>
              </div>
            </div>
            {#if expanded === a.account_id}
              {#if a.kind === "shares_private"}
                <EquityPanel accountId={a.account_id} onchange={load} />
              {:else}
                <PropertyPanel accountId={a.account_id} onchange={load} />
              {/if}
            {/if}
          {/each}
        </section>
      {/if}
    {/each}
    {#if (balances?.accounts ?? []).length === 0}
      <div class="empty">No accounts yet — add one to get started.</div>
    {/if}
  </div>
{/if}

<style>
  .acct {
    padding: 11px 2px;
    border-bottom: 1px solid var(--border);
    gap: 10px;
  }
  .acct:last-child {
    border-bottom: none;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
