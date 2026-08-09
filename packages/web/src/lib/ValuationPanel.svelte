<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "./api";

  let {
    accountId,
    accountClass,
    currency = "NZD",
    hasTransactions = false,
    onchange,
  }: {
    accountId: number;
    accountClass: Schemas["AccountClass"];
    currency?: string;
    /** Whether the account has a transaction history a valuation would override. */
    hasTransactions?: boolean;
    onchange?: () => void;
  } = $props();

  type Valuation = Schemas["Valuation"];

  // Exhaustive by type: adding a `ValuationSource` variant is a compile error here, which is
  // the frontend's analogue of the exhaustive-match rule the Rust side follows.
  const SOURCE_LABEL: Record<Schemas["ValuationSource"], string> = {
    manual: "manual",
    cron: "scheduled",
    provider: "synced",
    brokerage: "from holdings",
    equity: "from grant",
  };

  let rows = $state<Valuation[]>([]);
  let showAll = $state(false);
  let amount = $state("");
  let asOf = $state(new Date().toISOString().slice(0, 10));
  let note = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let confirmingDelete = $state<number | null>(null);

  // A liability is stored negative, so the field asks for what is *owed* and negates on save.
  // Typing 59020.76 into a field labelled "Balance owed" and having it land as an asset is
  // the single likeliest way to get this wrong.
  const isLiability = $derived(accountClass === "liability");
  const amountLabel = $derived(isLiability ? "Balance owed" : "Value");

  async function load() {
    const { data } = await api.GET("/api/accounts/{id}/valuations", {
      params: { path: { id: accountId }, query: showAll ? {} : { source: "manual" } },
    });
    rows = data ?? [];
  }
  onMount(load);
  $effect(() => {
    showAll;
    load();
  });

  /**
   * A valuation is a level that carries forward: it governs from its date until the next one,
   * and transactions in between stop counting toward the account's value. Saying so on each
   * row turns it from a point into the statement it actually is.
   */
  function heldUntil(index: number): string {
    // `rows` is newest-first, so the *previous* entry is the one that supersedes this.
    const next = rows[index - 1];
    return next ? `held until ${formatDate(next.as_of)}` : "held from here on";
  }

  async function save() {
    const major = parseFloat(amount.replace(/[^0-9.-]/g, ""));
    if (isNaN(major)) return;
    busy = true;
    error = null;
    const magnitude = Math.round(Math.abs(major) * 100);
    const { error: e } = await api.POST("/api/accounts/{id}/valuations", {
      params: { path: { id: accountId } },
      body: {
        as_of: asOf,
        value_minor: isLiability ? -magnitude : magnitude,
        note: note.trim() || undefined,
      },
    });
    busy = false;
    if (e) {
      error = "Couldn't save that value.";
      return;
    }
    amount = "";
    note = "";
    await load();
    onchange?.();
  }

  async function remove(id: number) {
    const { error: e } = await api.DELETE("/api/valuations/{id}", { params: { path: { id } } });
    confirmingDelete = null;
    if (e) {
      error = "Couldn't delete that value.";
      return;
    }
    await load();
    onchange?.();
  }
</script>

<div class="valuations">
  <div class="row wrap" style="gap:8px;align-items:flex-end">
    <label class="field">
      <span class="small muted">{amountLabel}</span>
      <input
        class="input tabular"
        style="min-width:130px"
        placeholder={isLiability ? "59020.76" : "850000"}
        aria-label={amountLabel}
        bind:value={amount}
      />
    </label>
    <label class="field">
      <span class="small muted">As of</span>
      <input class="input" type="date" aria-label="Value as of" bind:value={asOf} />
    </label>
    <label class="field grow">
      <span class="small muted">Note (optional)</span>
      <input class="input" placeholder="e.g. opening balance from the IR letter" bind:value={note} />
    </label>
    <button class="btn btn-sm btn-primary" onclick={save} disabled={busy || !amount.trim()}>
      Set {isLiability ? "balance" : "value"}
    </button>
  </div>

  {#if isLiability}
    <div class="small faint" style="margin-top:6px">
      Enter what's owed as a positive number — it's stored as a negative balance.
    </div>
  {/if}

  {#if hasTransactions}
    <!-- Not a footnote: on a transaction-fed account this is usually a surprise. -->
    <div class="warn small">
      This account has transactions. A value pins its balance to the figure you set, from that
      date until the next value — transactions in that window stop counting toward it.
    </div>
  {/if}

  {#if error}<div class="small" style="color:var(--negative);margin-top:6px">{error}</div>{/if}

  <div class="row spread" style="margin:12px 0 4px">
    <span class="small muted">Values set</span>
    <label class="small faint" style="display:inline-flex;gap:6px;align-items:center;cursor:pointer">
      <input type="checkbox" bind:checked={showAll} aria-label="Show synced and scheduled values" />
      Show synced &amp; scheduled
    </label>
  </div>

  {#each rows as v, i (v.id)}
    <div class="row spread line">
      <div style="min-width:0">
        <span class="tabular">{formatMoney(v.value_minor, v.currency_code ?? currency)}</span>
        <span class="badge">{SOURCE_LABEL[v.source]}</span>
        <div class="small faint">
          {formatDate(v.as_of)} · {heldUntil(i)}{v.note ? ` · ${v.note}` : ""}
        </div>
      </div>
      {#if v.source === "manual"}
        {#if confirmingDelete === v.id}
          <button class="btn btn-sm btn-danger" onclick={() => remove(v.id)}>Delete?</button>
        {:else}
          <button
            class="btn btn-sm"
            aria-label="Delete value from {v.as_of}"
            onclick={() => (confirmingDelete = v.id)}>✕</button
          >
        {/if}
      {:else}
        <!-- Deleting a cron-written valuation would leave its period consumed in `cron_runs`,
             so that month's adjustment could never re-apply. Undo the run instead. -->
        <span
          class="small faint"
          title="Written automatically — undo it where it came from, not here">auto</span
        >
      {/if}
    </div>
  {/each}
  {#if rows.length === 0}
    <div class="small faint">
      {showAll ? "No values recorded yet." : "None set by hand yet."}
    </div>
  {/if}
</div>

<style>
  .valuations {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .line {
    padding: 7px 0;
    border-top: 1px solid var(--border);
    gap: 8px;
  }
  .warn {
    margin-top: 8px;
    padding: 7px 9px;
    border-radius: var(--r);
    background: var(--surface-2);
    color: var(--text-muted);
  }
</style>
