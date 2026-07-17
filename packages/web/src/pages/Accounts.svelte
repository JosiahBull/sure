<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../lib/api";
  import { kindLabel, metaSummary, showsInstitution } from "../lib/accountMeta";
  import AccountForm from "../lib/AccountForm.svelte";
  import EquityPanel from "../lib/EquityPanel.svelte";
  import PropertyPanel from "../lib/PropertyPanel.svelte";

  let balances = $state<Schemas["BalancesReport"] | null>(null);
  let currencies = $state<Schemas["Currency"][]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let showAdd = $state(false);
  let editing = $state<number | null>(null);
  let expanded = $state<number | null>(null);
  let confirmDelete = $state<number | null>(null);
  let delError = $state<string | null>(null);

  const CLASSES = [
    { key: "cash", label: "Cash" },
    { key: "investment", label: "Investments" },
    { key: "asset", label: "Assets" },
    { key: "liability", label: "Liabilities" },
  ];

  const byId = $derived(new Map(accounts.map((a) => [a.id, a])));

  async function load() {
    loading = true;
    const [b, c, a] = await Promise.all([
      api.GET("/api/reports/balances", {}),
      api.GET("/api/currencies", {}),
      api.GET("/api/accounts", {}),
    ]);
    balances = b.data ?? null;
    currencies = c.data ?? [];
    accounts = a.data ?? [];
    loading = false;
  }
  onMount(load);

  const inClass = (cls: string) => (balances?.accounts ?? []).filter((a) => a.class === cls);

  function saved() {
    showAdd = false;
    editing = null;
    load();
  }

  function askDelete(id: number) {
    confirmDelete = id;
    delError = null;
    editing = null; // don't show the edit form and the confirmation at once
  }
  function cancelDelete() {
    confirmDelete = null;
    delError = null;
  }
  async function del(id: number) {
    delError = null;
    const { error: e } = await api.DELETE("/api/accounts/{id}", { params: { path: { id } } });
    if (e) {
      // e.g. 409 when debts are still secured against this asset — keep the panel open.
      delError = (e as { error?: { message?: string } }).error?.message ?? "Couldn't delete this account.";
      return;
    }
    confirmDelete = null;
    if (expanded === id) expanded = null;
    if (editing === id) editing = null;
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
  <button class="btn btn-primary btn-sm" onclick={() => ((showAdd = !showAdd), (editing = null))}>
    {showAdd ? "Close" : "+ Add account"}
  </button>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if showAdd}
  <AccountForm {currencies} {accounts} onsave={saved} oncancel={() => (showAdd = false)} />
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
            {@const full = byId.get(a.account_id)}
            {@const summary = metaSummary(a.kind, full?.metadata)}
            {@const inst = showsInstitution(a.kind) ? full?.institution : null}
            <div class="acct">
              <div class="col" style="min-width:0;gap:2px;flex:1 1 150px">
                <div class="row" style="gap:8px;min-width:0">
                  <span class="ell">{a.name}</span>
                  <span class="badge">{kindLabel(a.kind)}</span>
                </div>
                {#if inst || summary}
                  <div class="small faint ell">
                    {[inst, summary].filter(Boolean).join(" · ")}
                  </div>
                {/if}
              </div>
              <div class="col" style="align-items:flex-end;gap:6px;flex:0 0 auto;margin-left:auto">
                <span class="tabular" class:neg={a.value_minor < 0}>{formatMoney(a.value_minor, a.currency_code)}</span>
                <div class="row" style="gap:6px">
                  <button class="btn btn-sm" onclick={() => ((editing = editing === a.account_id ? null : a.account_id), (showAdd = false))}>
                    {editing === a.account_id ? "Close" : "Edit"}
                  </button>
                  {#if a.kind === "shares_private" || a.class === "asset"}
                    <button class="btn btn-sm" onclick={() => (expanded = expanded === a.account_id ? null : a.account_id)}>
                      {expanded === a.account_id ? "Hide" : "Equity"}
                    </button>
                  {/if}
                  <button class="btn btn-sm btn-danger" aria-label="Delete {a.name}" onclick={() => askDelete(a.account_id)}>✕</button>
                </div>
              </div>
            </div>
            {#if confirmDelete === a.account_id}
              <div class="confirm">
                <div class="small">Delete <strong>{a.name}</strong> and its transactions? This can't be undone.</div>
                {#if delError}<div class="error-banner" style="margin-top:8px">{delError}</div>{/if}
                <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
                  <button class="btn btn-sm" onclick={cancelDelete}>Cancel</button>
                  <button class="btn btn-sm btn-danger" onclick={() => del(a.account_id)}>Delete</button>
                </div>
              </div>
            {/if}
            {#if editing === a.account_id && full}
              <AccountForm account={full} {currencies} {accounts} onsave={saved} oncancel={() => (editing = null)} />
            {/if}
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
    display: flex;
    align-items: center;
    flex-wrap: wrap; /* on narrow screens the value + actions drop below the name */
    gap: 8px 10px;
    padding: 11px 2px;
    border-bottom: 1px solid var(--border);
  }
  .acct:last-child {
    border-bottom: none;
  }
  .col {
    display: flex;
    flex-direction: column;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .confirm {
    margin: 2px 2px 12px;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
</style>
