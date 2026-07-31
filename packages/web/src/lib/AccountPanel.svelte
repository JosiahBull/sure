<script lang="ts">
  import { onMount } from "svelte";
  import { balances, refresh } from "./balances.svelte";
  import { groupByClass, type PanelTab } from "./balanceGroups";
  import { api, formatMoney } from "./api";
  import { activeRange } from "./state.svelte";
  import { navigate } from "./router.svelte";
  import Icon from "./Icon.svelte";
  import NewAccountModal from "./NewAccountModal.svelte";

  const TABS: { key: PanelTab; label: string }[] = [
    { key: "all", label: "All" },
    { key: "assets", label: "Assets" },
    { key: "debts", label: "Debts" },
  ];

  let activeTab = $state<PanelTab>("all");
  let expandedGroups = $state(new Set<string>());
  let showNew = $state(false);

  onMount(() => {
    if (!balances.data) refresh();
  });

  // Per-account values as of the active period's start, so each group can show a signed,
  // coloured change % (like the reference) rather than a static allocation figure. Re-fetched
  // whenever the shared time range changes.
  let baseline = $state<Map<number, number>>(new Map());
  $effect(() => {
    const { from } = activeRange();
    if (!from) {
      baseline = new Map();
      return;
    }
    let cancelled = false;
    api.GET("/api/reports/balances", { params: { query: { to: from } } }).then(({ data }) => {
      if (cancelled) return;
      baseline = new Map((data?.accounts ?? []).map((a) => [a.account_id, a.value_minor]));
    });
    return () => (cancelled = true);
  });

  const currency = $derived(balances.data?.currency ?? "NZD");
  const grouped = $derived(groupByClass(balances.data?.accounts ?? [], activeTab, baseline));

  function toggleGroup(key: string) {
    const next = new Set(expandedGroups);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expandedGroups = next;
  }

  function goToAccount(accountId: number) {
    navigate(`/transactions?account=${accountId}`);
  }

  const newLabel = $derived(activeTab === "debts" ? "debt" : activeTab === "assets" ? "asset" : "account");
</script>

<nav class="panel">
  <div class="seg">
    {#each TABS as t}
      <button
        type="button"
        class="seg-btn"
        class:active={activeTab === t.key}
        onclick={() => (activeTab = t.key)}
      >
        {t.label}
      </button>
    {/each}
  </div>

  <button type="button" class="new-asset" onclick={() => (showNew = true)}>
    <Icon name="plus" size={16} />
    New {newLabel}
  </button>

  {#if balances.loading && !balances.data}
    <div class="row" style="justify-content:center;padding:24px"><span class="spinner"></span></div>
  {:else if balances.error}
    <div class="error-banner small">{balances.error}</div>
  {:else if grouped.groups.length === 0}
    <div class="empty small">No accounts yet.</div>
  {:else}
    <ul class="kind-list">
      {#each grouped.groups as g (g.key)}
        <li>
          <button type="button" class="kind-row" onclick={() => toggleGroup(g.key)}>
            <!-- Chevron/label spacing matches the reference sidebar's group row exactly: a 20px
                 chevron with gap-3 (12px), inside .kind-row's 12px padding. -->
            <span class="row" style="gap:12px;min-width:0">
              <Icon name={expandedGroups.has(g.key) ? "chevron-down" : "chevron-right"} size={20} />
              <span class="ell">{g.label}</span>
            </span>
            <span class="col" style="align-items:flex-end;flex:none">
              <span class="tabular">{formatMoney(g.totalMinor, currency)}</span>
              {#if g.changePct !== null}
                <span
                  class="tabular change"
                  class:pos={g.changeMinor > 0}
                  class:neg={g.changeMinor < 0}
                >{g.changePct.toFixed(1)}%</span>
              {/if}
            </span>
          </button>
          {#if expandedGroups.has(g.key)}
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

{#if showNew}
  <NewAccountModal
    initialTab={activeTab}
    onclose={() => (showNew = false)}
    oncreated={() => {
      showNew = false;
      refresh();
    }}
  />
{/if}

<style>
  .panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  /* Segmented control (All / Assets / Debts) — an inset track with a raised white pill for the
     active tab, matching the reference sidebar rather than a solid accent pill. */
  .seg {
    display: flex;
    gap: 2px;
    padding: 4px;
    margin-bottom: 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }
  .seg-btn {
    all: unset;
    flex: 1;
    text-align: center;
    padding: 5px 0;
    border-radius: 6px;
    font-size: 14px;
    font-weight: 550;
    color: var(--text-muted);
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .seg-btn:hover:not(.active) {
    color: var(--text);
  }
  .seg-btn.active {
    background: var(--surface);
    color: var(--text);
  }

  /* "New asset/account/debt" quick action under the segmented control. */
  .new-asset {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    /* 12px horizontal like every other sidebar row (and like the reference's ghost DS::Link),
       so this row's glyph lines up with the group chevrons below it rather than sitting 4px left. */
    padding: 8px 12px;
    margin-bottom: 6px;
    border-radius: var(--r-sm);
    color: var(--text);
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
  }
  .new-asset :global(svg) {
    color: var(--text-muted);
  }
  .new-asset:hover {
    background: var(--hover);
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
    /* `all: unset` also resets box-sizing to content-box, so width:100% + the horizontal
       padding below would overflow the panel's right edge (and crowd the value against it). */
    box-sizing: border-box;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    width: 100%;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    cursor: pointer;
    font-size: 14px;
    color: var(--text);
  }
  /* Group rows are medium-weight (both the name and the value), matching the reference. */
  .kind-row {
    font-weight: 500;
  }
  .kind-row:hover,
  .acct-row:hover {
    background: var(--hover);
  }
  .acct-list {
    padding-left: 20px;
  }
  .acct-row {
    padding: 6px 12px;
    font-weight: 400;
    color: var(--text-muted);
  }
  /* Period change %: 12px medium, grey at zero, green on a gain, red on a loss (matching the
     reference exactly). */
  .change {
    font-size: 12px;
    font-weight: 500;
    color: var(--text-muted);
  }
  .change.pos {
    color: var(--positive);
  }
  .change.neg {
    color: var(--negative);
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
