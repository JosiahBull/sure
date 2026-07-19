<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "./api";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  let snapshot = $state<Schemas["BrokerageSnapshot"] | null>(null);
  let busy = $state<null | "revalue" | "backfill" | "import">(null);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let fileInput = $state<HTMLInputElement | null>(null);

  async function load() {
    const { data } = await api.GET("/api/accounts/{id}/brokerage", {
      params: { path: { id: accountId } },
    });
    snapshot = data ?? null;
  }
  onMount(load);

  async function revalue() {
    busy = "revalue";
    error = null;
    const { error: e } = await api.POST("/api/accounts/{id}/brokerage/revalue", {
      params: { path: { id: accountId } },
    });
    if (e) error = "Revalue failed.";
    else notice = "Revalued.";
    await load();
    onchange?.();
    busy = null;
  }

  async function backfill() {
    busy = "backfill";
    error = null;
    const { data, error: e } = await api.POST("/api/accounts/{id}/brokerage/backfill", {
      params: { path: { id: accountId } },
    });
    if (e) error = "Backfill failed.";
    else if (data) notice = `Backfilled ${data.days} day(s) of history.`;
    await load();
    onchange?.();
    busy = null;
  }

  async function importZip(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    busy = "import";
    error = null;
    notice = null;
    // A binary upload doesn't fit the JSON client, so post the raw zip bytes directly to
    // the same-origin API (dev proxies /api to the backend), like Settings' config export.
    try {
      const res = await fetch(`/api/accounts/${accountId}/brokerage/import`, {
        method: "POST",
        headers: { "Content-Type": "application/zip" },
        body: file,
      });
      if (!res.ok) {
        const body = await res.json().catch(() => null);
        error = body?.error?.message ?? "Import failed — is this a Sharesies export zip?";
      } else {
        const r = (await res.json()) as Schemas["BrokerageImportResult"];
        const bits = [
          `${r.transactions_imported} transactions`,
          `${r.holdings_imported} holdings`,
          `${r.dividends_imported} dividends`,
        ];
        notice = `Imported ${bits.join(", ")}. Valuation history is backfilling in the background.`;
        if (r.warnings.length) notice += ` (${r.warnings.length} record(s) skipped)`;
        await load();
        onchange?.();
      }
    } catch {
      error = "Import failed — could not reach the server.";
    }
    if (fileInput) fileInput.value = "";
    busy = null;
  }
</script>

<div class="brokerage">
  {#if error}<div class="error-banner" style="margin-bottom:8px">{error}</div>{/if}
  {#if notice}<div class="badge" style="margin-bottom:8px">{notice}</div>{/if}

  {#if snapshot}
    <div class="row spread" style="margin-bottom:8px">
      <span class="muted small">
        Portfolio value <strong class="tabular" style="color:var(--text)"
          >{formatMoney(snapshot.total_value_minor, snapshot.currency_code)}</strong
        >
      </span>
      <div class="row" style="gap:8px">
        <button class="btn btn-sm" onclick={() => fileInput?.click()} disabled={busy !== null}>
          {busy === "import" ? "Importing…" : "Import zip"}
        </button>
        <button class="btn btn-sm" onclick={revalue} disabled={busy !== null}>
          {busy === "revalue" ? "…" : "Revalue"}
        </button>
        <button class="btn btn-sm" onclick={backfill} disabled={busy !== null}>
          {busy === "backfill" ? "…" : "Backfill"}
        </button>
        <input
          bind:this={fileInput}
          type="file"
          accept=".zip,application/zip"
          style="display:none"
          onchange={importZip}
        />
      </div>
    </div>

    {#if snapshot.positions.length}
      <table class="holdings">
        <thead>
          <tr><th>Holding</th><th class="num">Units</th><th class="num">Price</th><th class="num">Value</th></tr>
        </thead>
        <tbody>
          {#each snapshot.positions as p (p.ticker + p.exchange)}
            <tr>
              <td>
                <strong>{p.ticker}</strong>
                {#if p.name}<span class="faint small"> · {p.name}</span>{/if}
              </td>
              <td class="num tabular">{p.quantity.toLocaleString(undefined, { maximumFractionDigits: 4 })}</td>
              <td class="num tabular">
                {#if p.price}{formatMoney(Math.round(Number(p.price) * 100), p.currency_code)}{:else}<span class="faint">—</span>{/if}
              </td>
              <td class="num tabular">
                {#if p.market_value_minor != null}{formatMoney(p.market_value_minor, p.currency_code)}{:else}<span class="faint">—</span>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <div class="small faint" style="padding:6px 0">No holdings yet — import a Sharesies export to populate them.</div>
    {/if}

    {#if snapshot.wallets.length}
      <div class="wallets">
        <span class="faint small">Cash wallets</span>
        {#each snapshot.wallets as w (w.currency_code)}
          <span class="wallet tabular">{formatMoney(w.amount_minor, w.currency_code)}</span>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .brokerage {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  table.holdings {
    width: 100%;
    border-collapse: collapse;
    font-size: 14px;
  }
  table.holdings th {
    text-align: left;
    font-weight: 550;
    color: var(--text-muted);
    padding: 4px 8px;
    border-bottom: 1px solid var(--border);
  }
  table.holdings td {
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  table.holdings tr:last-child td {
    border-bottom: none;
  }
  .num {
    text-align: right;
  }
  .wallets {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
  .wallet {
    background: var(--surface-2);
    border-radius: 999px;
    padding: 2px 10px;
    font-size: 13px;
  }
</style>
