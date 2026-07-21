<script lang="ts">
  import { onMount } from "svelte";
  import { balances, refresh } from "./balances.svelte";
  import { groupByKind, type PanelTab } from "./balanceGroups";
  import { formatMoney } from "./api";
  import { navigate } from "./router.svelte";
  import Icon from "./Icon.svelte";

  const TABS: { key: PanelTab; label: string }[] = [
    { key: "all", label: "All" },
    { key: "assets", label: "Assets" },
    { key: "debts", label: "Debts" },
  ];

  let activeTab = $state<PanelTab>("all");
  let expandedKinds = $state(new Set<string>());

  onMount(() => {
    if (!balances.data) refresh();
  });

  const currency = $derived(balances.data?.currency ?? "NZD");
  const grouped = $derived(groupByKind(balances.data?.accounts ?? [], activeTab));

  function toggleKind(kind: string) {
    const next = new Set(expandedKinds);
    if (next.has(kind)) next.delete(kind);
    else next.add(kind);
    expandedKinds = next;
  }

  function goToAccount(accountId: number) {
    navigate(`/transactions?account=${accountId}`);
  }
</script>

<nav class="panel">
  <div class="row" style="gap:4px;margin-bottom:10px">
    {#each TABS as t}
      <button
        type="button"
        class="chip"
        class:active={activeTab === t.key}
        onclick={() => (activeTab = t.key)}
      >
        {t.label}
      </button>
    {/each}
  </div>

  {#if balances.loading && !balances.data}
    <div class="row" style="justify-content:center;padding:24px"><span class="spinner"></span></div>
  {:else if balances.error}
    <div class="error-banner small">{balances.error}</div>
  {:else if grouped.groups.length === 0}
    <div class="empty small">No accounts yet.</div>
  {:else}
    <ul class="kind-list">
      {#each grouped.groups as g (g.kind)}
        <li>
          <button type="button" class="kind-row" onclick={() => toggleKind(g.kind)}>
            <span class="row" style="gap:6px;min-width:0">
              <Icon name={expandedKinds.has(g.kind) ? "chevron-down" : "chevron-right"} size={14} />
              <span class="ell">{g.label}</span>
            </span>
            <span class="col" style="align-items:flex-end;flex:none">
              <span class="tabular">{formatMoney(g.totalMinor, currency)}</span>
              <span class="small faint">{g.weightPct.toFixed(1)}%</span>
            </span>
          </button>
          {#if expandedKinds.has(g.kind)}
            <ul class="acct-list">
              {#each g.accounts as a (a.account_id)}
                <li>
                  <button type="button" class="acct-row" onclick={() => goToAccount(a.account_id)}>
                    <span class="ell">{a.name}</span>
                    <span class="tabular small">{formatMoney(a.value_minor, a.currency_code)}</span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</nav>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .kind-list,
  .acct-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .kind-row,
  .acct-row {
    all: unset;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    width: 100%;
    padding: 7px 8px;
    border-radius: var(--r-sm);
    cursor: pointer;
    font-size: 13.5px;
  }
  .kind-row:hover,
  .acct-row:hover {
    background: var(--hover);
  }
  .acct-list {
    padding-left: 20px;
  }
  .acct-row {
    padding: 5px 8px;
    color: var(--text-muted);
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
</style>
