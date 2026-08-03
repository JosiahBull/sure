<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../lib/api";
  import {
    kindLabel,
    metaSummary,
    showsInstitution,
    remainingBorrowing,
    loanPaidOffPct,
    takesBankCsv,
  } from "../lib/accountMeta";
  import AccountForm from "../lib/AccountForm.svelte";
  import EquityPanel from "../lib/EquityPanel.svelte";
  import PropertyPanel from "../lib/PropertyPanel.svelte";
  import BrokeragePanel from "../lib/BrokeragePanel.svelte";
  import StudentLoanPanel from "../lib/StudentLoanPanel.svelte";
  import AsbImportPanel from "../lib/AsbImportPanel.svelte";
  import AsbUploadPanel from "../lib/AsbUploadPanel.svelte";
  import { navigate } from "../lib/router.svelte";
  import { balances, refresh as refreshBalances } from "../lib/balances.svelte";
  import {
    people,
    refresh as refreshPeople,
    ownershipLabel,
    ownershipColor,
    ownershipOptions,
    ownershipFromKey,
    placeholders,
  } from "../lib/people.svelte";

  let currencies = $state<Schemas["Currency"][]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let showAdd = $state(false);
  // A zip of ASB exports spans accounts, so it belongs to the page rather than to any one
  // account's row — unlike AsbImportPanel, which lives inside a row.
  let showImport = $state(false);
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
    const [, , c, a] = await Promise.all([
      refreshBalances(),
      refreshPeople(),
      api.GET("/api/currencies", {}),
      api.GET("/api/accounts", {}),
    ]);
    currencies = c.data ?? [];
    accounts = a.data ?? [];
    loading = false;
  }
  onMount(load);

  // Accounts predating the household feature were handed to a placeholder person, because the
  // migration couldn't know whose they were and every account has to name someone. This is the
  // prompt to resolve that; it disappears once the placeholder owns nothing (or is renamed,
  // which is the other way of answering — "they were all mine").
  const placeholderIds = $derived(new Set(placeholders().map((p) => p.id)));
  const placeholderOwned = $derived(
    accounts.filter((a) => a.ownership.kind === "person" && placeholderIds.has(a.ownership.person_id)),
  );
  let bulkOwner = $state("");
  let bulkBusy = $state(false);
  let bulkError = $state<string | null>(null);

  async function attributePlaceholderAccounts() {
    if (!bulkOwner || placeholderOwned.length === 0) return;
    bulkBusy = true;
    bulkError = null;
    const { error: e } = await api.POST("/api/accounts/ownership", {
      body: {
        account_ids: placeholderOwned.map((a) => a.id),
        ownership: ownershipFromKey(bulkOwner),
      },
    });
    bulkBusy = false;
    if (e) {
      bulkError =
        (e as { error?: { message?: string } }).error?.message ?? "Couldn't attribute these accounts.";
      return;
    }
    bulkOwner = "";
    load();
  }

  const inClass = (cls: string) => (balances.data?.accounts ?? []).filter((a) => a.class === cls);

  function saved() {
    showAdd = false;
    editing = null;
    load();
  }

  // Jump to the transactions page filtered to just this account (mirrors Dashboard's
  // goToCategory, which does the same for a category slice).
  function goToAccount(accountId: number) {
    navigate(`/transactions?account=${accountId}`);
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
    {#if balances.data}
      <div class="muted small">
        Net worth <strong class="tabular" style="color:var(--text)"
          >{formatMoney(balances.data.total_minor, balances.data.currency)}</strong
        >
      </div>
    {/if}
  </div>
  <div class="row wrap" style="gap:6px">
    <button class="btn btn-sm" onclick={() => ((showImport = !showImport), (showAdd = false), (editing = null))}>
      {showImport ? "Close" : "Import bank exports"}
    </button>
    <button class="btn btn-primary btn-sm" onclick={() => ((showAdd = !showAdd), (showImport = false), (editing = null))}>
      {showAdd ? "Close" : "+ Add account"}
    </button>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if placeholderOwned.length > 0}
  <div class="attribute-banner">
    <div class="col" style="gap:2px;min-width:0">
      <strong class="small"
        >{placeholderOwned.length}
        {placeholderOwned.length === 1 ? "account is" : "accounts are"} still owned by a placeholder</strong
      >
      <span class="small faint">
        Attribute them all at once here, set each one from its Edit form, or rename the
        placeholder on the Household page if they're all one person's.
      </span>
    </div>
    <div class="row" style="gap:8px;margin-left:auto;flex-wrap:wrap">
      <select
        class="select"
        bind:value={bulkOwner}
        aria-label="Attribute the placeholder's accounts to"
      >
        <option value="">Choose an owner…</option>
        {#each ownershipOptions().filter((o) => !placeholderIds.has(Number(o.key.slice(7)))) as o (o.key)}
          <option value={o.key}>{o.label}</option>
        {/each}
      </select>
      <button
        class="btn btn-primary btn-sm"
        disabled={!bulkOwner || bulkBusy}
        onclick={attributePlaceholderAccounts}
      >
        {bulkBusy ? "Saving…" : `Attribute all ${placeholderOwned.length}`}
      </button>
    </div>
    {#if bulkError}<div class="error-banner" style="flex-basis:100%">{bulkError}</div>{/if}
  </div>
{/if}

{#if showImport}
  <AsbUploadPanel onchange={load} />
{/if}

{#if showAdd}
  <AccountForm {currencies} {accounts} onsave={saved} oncancel={() => (showAdd = false)} />
{/if}

{#if loading && !balances.data}
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
            {@const remaining = remainingBorrowing(a.kind, full?.metadata, a.value_minor)}
            {@const paidOffPct = loanPaidOffPct(a.kind, full?.metadata, a.value_minor)}
            <div class="acct">
              <button
                class="acct-link"
                onclick={() => goToAccount(a.account_id)}
                aria-label="View transactions for {a.name}"
              >
                <div class="row" style="gap:8px;min-width:0">
                  <span class="ell">{a.name}</span>
                  <span class="badge">{kindLabel(a.kind)}</span>
                  {#if full && people.list.length > 0}
                    {@const color = ownershipColor(full.ownership)}
                    {@const isPlaceholder =
                      full.ownership.kind === "person" && placeholderIds.has(full.ownership.person_id)}
                    <span
                      class="badge owner"
                      class:placeholder={isPlaceholder}
                      style={color && !isPlaceholder
                        ? `border-color:${color};color:${color}`
                        : undefined}
                    >
                      {ownershipLabel(full.ownership)}
                    </span>
                  {/if}
                </div>
                {#if inst || summary}
                  <div class="small faint ell">
                    {[inst, summary].filter(Boolean).join(" · ")}
                  </div>
                {/if}
              </button>
              <div class="col" style="align-items:flex-end;gap:6px;flex:0 0 auto;margin-left:auto">
                <span class="tabular" class:neg={a.value_minor < 0}>{formatMoney(a.value_minor, a.currency_code)}</span>
                {#if remaining !== null}
                  <span class="small faint tabular">Remaining: {formatMoney(remaining, a.currency_code)}</span>
                {/if}
                {#if paidOffPct !== null}
                  <span class="small faint tabular">{Math.round(paidOffPct)}% repaid</span>
                {/if}
                <div class="row" style="gap:6px">
                  <button class="btn btn-sm" onclick={() => ((editing = editing === a.account_id ? null : a.account_id), (showAdd = false))}>
                    {editing === a.account_id ? "Close" : "Edit"}
                  </button>
                  {#if a.kind === "shares_private" || a.kind === "brokerage" || a.kind === "crypto" || a.kind === "student_loan" || takesBankCsv(a.kind) || a.class === "asset"}
                    <button class="btn btn-sm" onclick={() => (expanded = expanded === a.account_id ? null : a.account_id)}>
                      {expanded === a.account_id
                        ? "Hide"
                        : a.kind === "brokerage"
                          ? "Holdings"
                          : a.kind === "crypto"
                            ? "Value"
                            : a.kind === "student_loan" || takesBankCsv(a.kind)
                              ? "Import"
                              : "Equity"}
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
              {#if a.kind === "brokerage"}
                <BrokeragePanel accountId={a.account_id} onchange={load} />
              {:else if a.kind === "shares_private"}
                <EquityPanel accountId={a.account_id} onchange={load} />
              {:else if a.kind === "student_loan"}
                <StudentLoanPanel accountId={a.account_id} onchange={load} />
              {:else if takesBankCsv(a.kind)}
                <AsbImportPanel accountId={a.account_id} currency={a.currency_code} onchange={load} />
              {:else}
                <PropertyPanel accountId={a.account_id} onchange={load} />
              {/if}
            {/if}
          {/each}
        </section>
      {/if}
    {/each}
    {#if (balances.data?.accounts ?? []).length === 0}
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
  .acct-link {
    all: unset;
    /* `all: unset` resets `display` too (it's not an inherited property), so re-declare
       the flex-column layout `.col` would otherwise provide — relying on cascade order
       between two same-specificity classes for this would be fragile. */
    display: flex;
    flex-direction: column;
    cursor: pointer;
    min-width: 0;
    gap: 2px;
    flex: 1 1 150px;
    border-radius: var(--r);
  }
  .acct-link:hover .ell:first-child {
    text-decoration: underline;
  }
  .acct-link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .owner {
    border: 1px solid var(--border);
    background: transparent;
  }
  /* The one badge that's a to-do rather than a fact — it reads as a gap, not a label. */
  .owner.placeholder {
    border-style: dashed;
    color: var(--text-muted);
  }
  .attribute-banner {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    margin-bottom: 14px;
    padding: 12px 14px;
    border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }
  .confirm {
    margin: 2px 2px 12px;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
</style>
