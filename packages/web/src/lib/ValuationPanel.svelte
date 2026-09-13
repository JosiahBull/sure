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
    estimate: "estimated",
  };

  let rows = $state<Valuation[]>([]);
  let showAll = $state(false);
  let amount = $state("");
  let asOf = $state(new Date().toISOString().slice(0, 10));
  let note = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let confirmingDelete = $state<number | null>(null);
  // The row being edited. The form doubles as the editor rather than growing a second one:
  // the fields are identical, and a separate inline form would drift from this one.
  let editingId = $state<number | null>(null);

  // A liability is stored negative, so the field asks for what is *owed* and negates on save.
  // Typing 59020.76 into a field labelled "Balance owed" and having it land as an asset is
  // the single likeliest way to get this wrong.
  const isLiability = $derived(accountClass === "liability");
  const amountLabel = $derived(isLiability ? "Balance owed" : "Value");
  const editing = $derived(editingId !== null);

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

  function startEdit(v: Valuation) {
    editingId = v.id;
    // Shown the way it was entered: a liability's stored value is negative, and the field
    // asks for what is owed.
    amount = String(Math.abs(v.value_minor) / 100);
    asOf = v.as_of;
    note = v.note ?? "";
    error = null;
  }

  function cancelEdit() {
    editingId = null;
    amount = "";
    note = "";
    asOf = new Date().toISOString().slice(0, 10);
    error = null;
  }

  async function save() {
    const major = parseFloat(amount.replace(/[^0-9.-]/g, ""));
    if (isNaN(major)) return;
    busy = true;
    error = null;
    const magnitude = Math.round(Math.abs(major) * 100);
    const body = {
      as_of: asOf,
      value_minor: isLiability ? -magnitude : magnitude,
      note: note.trim() || undefined,
    };
    const { error: e } =
      editingId !== null
        ? await api.PUT("/api/valuations/{id}", { params: { path: { id: editingId } }, body })
        : await api.POST("/api/accounts/{id}/valuations", {
            params: { path: { id: accountId } },
            body,
          });
    busy = false;
    if (e) {
      error =
        (e as { error?: { message?: string } }).error?.message ?? "Couldn't save that value.";
      return;
    }
    cancelEdit();
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
  <div class="value-form">
    <label class="field f-amount">
      <span class="small muted">{amountLabel}</span>
      <input
        class="input tabular"
        placeholder={isLiability ? "59020.76" : "850000"}
        aria-label={amountLabel}
        bind:value={amount}
      />
    </label>
    <label class="field">
      <span class="small muted">As of</span>
      <input class="input" type="date" aria-label="Value as of" bind:value={asOf} />
    </label>
    <label class="field f-note">
      <span class="small muted">Note (optional)</span>
      <input class="input" placeholder="e.g. opening balance from the IR letter" bind:value={note} />
    </label>
    <button class="btn btn-sm btn-primary" onclick={save} disabled={busy || !amount.trim()}>
      {editing ? "Save changes" : `Set ${isLiability ? "balance" : "value"}`}
    </button>
    {#if editing}
      <button class="btn btn-sm" onclick={cancelEdit}>Cancel</button>
    {/if}
  </div>

  {#if editing}
    <p class="small hint">
      Editing the value set on {formatDate(asOf)}. Changing the date moves it.
    </p>
  {/if}

  {#if isLiability}
    <p class="small hint">
      Enter what's owed as a positive number — it's stored as a negative balance.
    </p>
  {/if}

  {#if hasTransactions}
    <!-- Not a footnote: on a transaction-fed account this is usually a surprise. -->
    <div class="warn small">
      This account has transactions. A value pins its balance to the figure you set, from that
      date until the next value — transactions in that window stop counting toward it.
    </div>
  {/if}

  {#if error}<p class="small err">{error}</p>{/if}

  <div class="ledger-head">
    <span class="small muted ledger-title">Values set</span>
    <label class="small faint toggle">
      <input type="checkbox" bind:checked={showAll} aria-label="Show synced and scheduled values" />
      Show synced &amp; scheduled
    </label>
  </div>

  {#each rows as v, i (v.id)}
    <div class="row spread line" class:editing={editingId === v.id}>
      <div class="line-main">
        <span class="tabular">{formatMoney(v.value_minor, v.currency_code ?? currency)}</span>
        <span class="badge">{SOURCE_LABEL[v.source]}</span>
        <!-- At `--text-muted`, not `--text-faint`: this line carries the note, and the faint
             shade fails contrast at 13px against the panel's own fill. -->
        <div class="small line-meta">
          {formatDate(v.as_of)} · {heldUntil(i)}{v.note ? ` · ${v.note}` : ""}
        </div>
      </div>
      {#if v.source === "manual"}
        <div class="line-acts">
          <button
            class="btn btn-sm"
            aria-label="Edit value from {v.as_of}"
            onclick={() => startEdit(v)}>Edit</button
          >
          {#if confirmingDelete === v.id}
            <button class="btn btn-sm btn-danger" onclick={() => remove(v.id)}>Delete?</button>
          {:else}
            <button
              class="btn btn-sm"
              aria-label="Delete value from {v.as_of}"
              onclick={() => (confirmingDelete = v.id)}>✕</button
            >
          {/if}
        </div>
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
    /* `--surface-2` is the same colour as `--bg-elev` in both palettes, and this panel is
       painted `--bg-elev` — so every fill built on it (the source `.badge` on each row, the
       transactions warning below) was drawing nothing at all. Derive the inset from the ink,
       which steps away from the panel in whichever direction the active theme needs. */
    --inset: color-mix(in srgb, var(--text) 5%, transparent);

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
  .badge {
    background: var(--inset);
  }

  .value-form {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 10px;
  }
  /* Floors, not just grow weights: a flex row shrinks its items before it wraps, so without
     these the note input was squeezed to a clipped "e.g. op" on a phone — the width this app
     actually targets — instead of dropping to a line of its own. */
  .value-form .f-amount {
    flex: 1 1 130px;
    min-width: 120px;
  }
  .value-form .f-note {
    flex: 4 1 180px;
    min-width: 160px;
  }

  .hint {
    margin: 6px 0 0;
    color: var(--text-muted);
  }
  .err {
    margin: 6px 0 0;
    color: var(--negative);
  }

  .ledger-head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 4px 12px;
    margin: 12px 0 4px;
  }
  /* Both halves opt out of wrapping so the row wraps as two whole labels: "Values set" was
     breaking across two lines rather than the toggle moving down. */
  .ledger-title,
  .toggle {
    white-space: nowrap;
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
  }

  .line {
    padding: 7px 0;
    border-top: 1px solid var(--border);
    gap: 8px;
  }
  .line.editing {
    background: color-mix(in srgb, var(--accent) 6%, transparent);
    border-radius: var(--r-sm);
  }
  .line-main {
    min-width: 0;
  }
  .line-meta {
    color: var(--text-muted);
  }
  .line-acts {
    display: flex;
    gap: 6px;
  }
  .warn {
    margin-top: 8px;
    padding: 7px 9px;
    border-radius: var(--r);
    background: var(--inset);
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
</style>
