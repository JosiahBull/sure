<script lang="ts">
  // Bulk upload of myIR "TAP SLS Transactions" exports, mirroring BrokeragePanel's
  // Sharesies zip import. Akahu reports this account's balance but no transactions, so the
  // ledger behind the cutover comes from here and everything after it is derived from the
  // daily balance feed by the backend's `balance_delta` task.
  //
  // The account row already shows the balance and paid-off %, so this panel reports the one
  // thing it can't: how much ledger is actually loaded, and how far back it reaches.
  import { onMount } from "svelte";
  import { api } from "./api";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  // High enough to cover a full student-loan history in one request (weekly living costs
  // over a degree plus fortnightly repayments is a few hundred rows); `saturated` keeps the
  // count honest rather than silently reporting the cap as the total.
  const LEDGER_LIMIT = 1000;

  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let warnings = $state<string[]>([]);
  let fileInput = $state<HTMLInputElement | null>(null);

  let count = $state(0);
  let saturated = $state(false);
  let oldest = $state<string | null>(null);
  let newest = $state<string | null>(null);

  async function load() {
    // `include_one_off: false` drops the "Opening balance" row every account is seeded with
    // — counting it would make a brand-new loan claim it already has a ledger.
    const { data } = await api.GET("/api/transactions", {
      params: {
        query: { account_id: accountId, limit: LEDGER_LIMIT, include_one_off: false },
      },
    });
    const rows = data ?? [];
    count = rows.length;
    saturated = rows.length === LEDGER_LIMIT;
    const dates = rows.map((t) => t.posted_at.slice(0, 10)).sort();
    oldest = dates[0] ?? null;
    newest = dates[dates.length - 1] ?? null;
  }
  onMount(load);

  async function importExport(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    busy = true;
    error = null;
    notice = null;
    warnings = [];
    // A binary upload doesn't fit the JSON client, so post the raw bytes directly to the
    // same-origin API (dev proxies /api to the backend), like BrokeragePanel's zip import.
    try {
      const res = await fetch(`/api/accounts/${accountId}/student-loan/import`, {
        method: "POST",
        headers: { "Content-Type": "application/zip" },
        body: file,
      });
      if (!res.ok) {
        const body = await res.json().catch(() => null);
        error = body?.error?.message ?? "Import failed — is this a myIR transactions export?";
      } else {
        const r = await res.json();
        const covered =
          r.covered_from && r.covered_to ? `, covering ${r.covered_from} → ${r.covered_to}` : "";
        const already = r.skipped ? ` ${r.skipped} were already here.` : "";
        notice = r.imported
          ? `Imported ${r.imported} transaction${r.imported === 1 ? "" : "s"} from ${r.account_id}${covered}.${already}`
          : `Nothing new — all ${r.skipped} rows were already here.`;
        warnings = r.warnings ?? [];
        await load();
        onchange?.();
      }
    } catch {
      error = "Import failed — could not reach the server.";
    }
    if (fileInput) fileInput.value = "";
    busy = false;
  }
</script>

<div class="student-loan">
  {#if error}<div class="error-banner" style="margin-bottom:10px">{error}</div>{/if}
  {#if notice}<div class="ok-banner">{notice}</div>{/if}
  {#each warnings as w}
    <div class="warn-banner">{w}</div>
  {/each}

  <div class="row spread" style="gap:12px">
    <div class="stat" style="gap:2px">
      {#if count}
        <div class="value tabular" style="font-size:20px">
          {saturated ? `${LEDGER_LIMIT}+` : count}
          <span class="faint" style="font-size:13px">
            transaction{count === 1 ? "" : "s"}
          </span>
        </div>
        <div class="small faint">{oldest} → {newest}</div>
      {:else}
        <div class="value" style="font-size:20px">No ledger yet</div>
        <div class="small faint">Akahu reports this loan's balance, but not its transactions.</div>
      {/if}
    </div>
    <button
      class="btn btn-sm {count ? '' : 'btn-primary'}"
      onclick={() => fileInput?.click()}
      disabled={busy}
    >
      {busy ? "Importing…" : "Import myIR export"}
    </button>
    <input
      bind:this={fileInput}
      type="file"
      accept=".xlsx,.zip,application/zip,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
      style="display:none"
      onchange={importExport}
    />
  </div>

  <!-- The full explanation earns its space only while there's nothing loaded; once there is
       a ledger, the one thing still worth saying is that adding to it is safe. -->
  <p class="hint small faint">
    {#if count}
      Re-uploading is free — overlapping windows are reconciled and rows already here are skipped.
    {:else}
      Export your transactions from myIR and drop the <code>.xlsx</code> here — or a
      <code>.zip</code> of several, since one export only reaches back about two years.
      Re-uploading is free: overlapping windows are reconciled and rows already here are skipped.
    {/if}
  </p>
</div>

<style>
  .student-loan {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  /* Same construction as the global .error-banner, in the success/warning tones — a result
     worth reading, rather than the 12px .badge pill it started as. */
  .ok-banner,
  .warn-banner {
    padding: 8px 12px;
    border-radius: var(--r);
    font-size: 13px;
    margin-bottom: 10px;
  }
  .ok-banner {
    background: color-mix(in srgb, var(--positive) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--positive) 38%, transparent);
    color: var(--positive);
  }
  .warn-banner {
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 38%, transparent);
    color: var(--warn);
  }
  .hint {
    margin: 10px 0 0;
    padding-top: 10px;
    border-top: 1px solid var(--border);
    line-height: 1.5;
  }
  code {
    background: var(--surface-2);
    border-radius: 4px;
    padding: 0 4px;
  }
</style>
