<script lang="ts">
  // Bulk upload of an ASB "Export transactions" CSV, mirroring StudentLoanPanel's myIR
  // import. Akahu only serves an account about two years of history; ASB's own export
  // reaches seven, so this is how an account's history gets extended past the feed.
  //
  // Two steps on purpose. Picking a file previews it — row count, date range, the cutover
  // the backend derived, and whether the export's closing balance matches the one Sure
  // holds — and only then does a second request commit. A few thousand rows going into
  // real money data is worth a look first, and the preview is the same code path as the
  // commit, so it can't describe an import that wouldn't happen.
  import { onMount } from "svelte";
  import { api, formatMoney } from "./api";

  let {
    accountId,
    currency = "NZD",
    onchange,
  }: { accountId: number; currency?: string; onchange?: () => void } = $props();

  type Preview = {
    rows_total: number;
    would_import: number;
    held_back: number;
    cutover: string | null;
    asb_account: string;
    sources: string[];
    product: string | null;
    covered_from: string | null;
    covered_to: string | null;
    ledger_balance_minor: number | null;
    account_balance_minor: number | null;
    implied_opening_minor: number | null;
    opening_balance_minor: number | null;
    opening_balance_as_of: string | null;
    warnings: string[];
  };

  let busy = $state<null | "preview" | "import" | "undo">(null);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let warnings = $state<string[]>([]);
  let preview = $state<Preview | null>(null);
  let pending = $state<File | null>(null);
  let fileInput = $state<HTMLInputElement | null>(null);
  let confirmUndo = $state(false);
  // On by default, matching the endpoint: without it the imported history starts from nothing,
  // because an account reads as 0 before its earliest transaction.
  let openingBalance = $state(true);

  // How much of this account's ledger the importer is responsible for, so the panel can
  // offer to take it back — and so re-uploading reads as topping up, not duplicating.
  let importedCount = $state(0);
  let oldest = $state<string | null>(null);

  async function load() {
    const { data } = await api.GET("/api/transactions", {
      params: { query: { account_id: accountId, limit: 10000 } },
    });
    const rows = (data ?? []).filter((t) => t.provider === `asb#${accountId}`);
    importedCount = rows.length;
    oldest = rows.map((t) => t.posted_at.slice(0, 10)).sort()[0] ?? null;
  }
  onMount(load);

  // Advisory only — the parser tells a zip from a CSV by content — but sending the truth
  // keeps the request honest and any proxy in between from guessing.
  const contentType = (file: File) =>
    file.name.toLowerCase().endsWith(".zip") ? "application/zip" : "text/csv";

  // A binary upload doesn't fit the JSON client, so post the raw bytes directly to the
  // same-origin API (dev proxies /api to the backend), like StudentLoanPanel's xlsx import.
  async function post(file: File, dryRun: boolean) {
    const q = new URLSearchParams({
      dry_run: String(dryRun),
      opening_balance: String(openingBalance),
    });
    const res = await fetch(`/api/accounts/${accountId}/asb/import?${q}`, {
      method: "POST",
      headers: { "Content-Type": contentType(file) },
      body: file,
    });
    if (!res.ok) {
      const body = await res.json().catch(() => null);
      throw new Error(
        body?.error?.message ?? "Import failed — is this an ASB transaction export?",
      );
    }
    return res.json();
  }

  async function pick(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    busy = "preview";
    error = null;
    notice = null;
    warnings = [];
    preview = null;
    try {
      preview = await post(file, true);
      pending = file;
    } catch (e) {
      error = e instanceof Error ? e.message : "Could not read that file.";
    }
    // Cleared so re-picking the same file fires `change` again.
    if (fileInput) fileInput.value = "";
    busy = null;
  }

  /** The opening row is one of the rows the commit writes, so the count has to be re-asked. */
  async function retoggle() {
    if (!pending || busy) return;
    busy = "preview";
    try {
      preview = await post(pending, true);
    } catch (e) {
      error = e instanceof Error ? e.message : "Could not re-read that file.";
    }
    busy = null;
  }

  async function commit() {
    if (!pending) return;
    busy = "import";
    error = null;
    try {
      const r = await post(pending, false);
      const covered =
        r.covered_from && r.covered_to ? `, covering ${r.covered_from} → ${r.covered_to}` : "";
      const already = r.skipped ? ` ${r.skipped} were already here.` : "";
      notice = r.imported
        ? `Imported ${r.imported} transaction${r.imported === 1 ? "" : "s"} from ${r.asb_account}${covered}.${already} They're uncategorised — run your rules from Settings → Rules to sort them.`
        : `Nothing new — all ${r.skipped} rows were already here.`;
      warnings = r.warnings ?? [];
      cancel();
      await load();
      onchange?.();
    } catch (e) {
      error = e instanceof Error ? e.message : "Import failed.";
    }
    busy = null;
  }

  function cancel() {
    preview = null;
    pending = null;
  }

  async function undo() {
    busy = "undo";
    error = null;
    notice = null;
    warnings = [];
    const { data, error: e } = await api.DELETE("/api/accounts/{id}/asb/import", {
      params: { path: { id: accountId } },
    });
    if (e) {
      error = (e as { error?: { message?: string } })?.error?.message ?? "Could not undo.";
    } else if (data) {
      notice = `Removed ${data.deleted} imported transaction${data.deleted === 1 ? "" : "s"}.`;
      await load();
      onchange?.();
    }
    confirmUndo = false;
    busy = null;
  }

  const money = (minor: number | null | undefined) =>
    minor === null || minor === undefined ? "—" : formatMoney(minor, currency);

  // Equal balances are the strongest evidence the export is for this account and reaches
  // all the way back. The backend already warns on a mismatch; this is the reassuring case.
  const reconciles = $derived(
    preview?.ledger_balance_minor !== null &&
      preview?.ledger_balance_minor === preview?.account_balance_minor,
  );
</script>

<div class="asb">
  {#if error}<div class="error-banner" style="margin-bottom:10px">{error}</div>{/if}
  {#if notice}<div class="ok-banner">{notice}</div>{/if}
  {#each warnings as w}
    <div class="warn-banner">{w}</div>
  {/each}

  {#if preview}
    <div class="preview">
      <div class="row spread wrap" style="gap:12px;align-items:baseline">
        <div>
          <div style="font-weight:600">
            ASB {preview.asb_account}{preview.product ? ` (${preview.product})` : ""}
          </div>
          <div class="small faint">
            {preview.rows_total} row{preview.rows_total === 1 ? "" : "s"}
            {#if preview.covered_from && preview.covered_to}
              · {preview.covered_from} → {preview.covered_to}
            {/if}
          </div>
        </div>
        <div class="stat" style="gap:2px;text-align:right">
          <div class="value tabular" style="font-size:20px">{preview.would_import}</div>
          <div class="small faint">to import</div>
        </div>
      </div>

      <dl class="facts small">
        {#if preview.cutover}
          <dt>Held back</dt>
          <dd>
            <span class="tabular">{preview.held_back}</span> row{preview.held_back === 1
              ? ""
              : "s"} from {preview.cutover} — a connected feed already covers those.
          </dd>
        {:else}
          <dt>Held back</dt>
          <dd>Nothing — no other feed posts to this account.</dd>
        {/if}
        <dt>Closing balance</dt>
        <dd>
          <span class="tabular">{money(preview.ledger_balance_minor)}</span> stated
          {#if preview.account_balance_minor !== null}
            · <span class="tabular">{money(preview.account_balance_minor)}</span> here
            {#if reconciles}<span class="pos">✓ matches</span>{/if}
          {/if}
        </dd>
        {#if preview.implied_opening_minor !== null && preview.covered_from}
          <dt>Opening balance</dt>
          <dd>
            <label class="opening">
              <input type="checkbox" bind:checked={openingBalance} onchange={retoggle} disabled={busy !== null} />
              <span>
                Record <span class="tabular">{money(preview.implied_opening_minor)}</span> as at
                {preview.opening_balance_as_of ?? "the day before the first row"}
              </span>
            </label>
            <div class="small faint">
              What the account held before this export starts. Without it the history begins at
              zero instead of here.
            </div>
          </dd>
        {/if}
      </dl>

      {#each preview.warnings as w}
        <div class="warn-banner" style="margin:0 0 10px">{w}</div>
      {/each}

      <div class="row" style="gap:8px;justify-content:flex-end">
        <button class="btn btn-sm" onclick={cancel} disabled={busy !== null}>Cancel</button>
        <button class="btn btn-sm btn-primary" onclick={commit} disabled={busy !== null}>
          {busy === "import" ? "Importing…" : `Import ${preview.would_import}`}
        </button>
      </div>
    </div>
  {:else}
    <div class="row spread" style="gap:12px">
      <div class="stat" style="gap:2px">
        {#if importedCount}
          <div class="value tabular" style="font-size:20px">
            {importedCount}
            <span class="faint" style="font-size:13px">imported</span>
          </div>
          <div class="small faint">reaching back to {oldest}</div>
        {:else}
          <div class="value" style="font-size:20px">No export loaded</div>
          <div class="small faint">Bank feeds only reach back about two years.</div>
        {/if}
      </div>
      <div class="row wrap actions" style="gap:6px">
        {#if importedCount}
          <button
            class="btn btn-sm btn-danger"
            onclick={() => (confirmUndo = true)}
            disabled={busy !== null}
          >
            {busy === "undo" ? "Removing…" : "Remove import"}
          </button>
        {/if}
        <button
          class="btn btn-sm {importedCount ? '' : 'btn-primary'}"
          onclick={() => fileInput?.click()}
          disabled={busy !== null}
        >
          {busy === "preview" ? "Reading…" : "Import ASB CSV"}
        </button>
      </div>
      <input
        bind:this={fileInput}
        type="file"
        accept=".csv,.zip,text/csv,application/zip"
        style="display:none"
        onchange={pick}
      />
    </div>

    {#if confirmUndo}
      <div class="confirm">
        <div class="small">
          Remove all {importedCount} transactions this importer added? Rows from your bank feed
          and anything you entered by hand are left alone.
        </div>
        <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
          <button class="btn btn-sm" onclick={() => (confirmUndo = false)}>Cancel</button>
          <button class="btn btn-sm btn-danger" onclick={undo}>Remove</button>
        </div>
      </div>
    {/if}

    <p class="hint small faint">
      In ASB FastNet, open the account → <strong>Export transactions</strong>, choose
      <strong>CSV</strong> and the <code>YYYY/MM/DD</code> date format, pick as wide a date range
      as it allows, and drop the file here. You'll see what it contains before anything is saved.
      Re-uploading is free: rows already here are skipped, and dates your bank feed already
      covers are held back so nothing is counted twice.
    </p>
  {/if}
</div>

<style>
  /* Same construction as StudentLoanPanel — the account row it expands under sets the
     context, so the panel only needs to read as attached to it. */
  .asb {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
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
  /* In a narrow column, let the row wrap rather than the labels — "Remove import" broken
     over three lines reads as two separate controls. */
  .actions :global(.btn) {
    white-space: nowrap;
  }
  .preview {
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    background: var(--surface);
  }
  /* Label/value pairs rather than a table: three facts, each one line. Stacked by default
     — in a narrow column a label/value grid gives the labels a column as wide as their
     longest word and wraps the values to nothing — and side by side once there's room,
     at the same breakpoint NewAccountModal uses. */
  .facts {
    display: grid;
    gap: 0;
    margin: 12px 0;
    padding: 12px 0 0;
    border-top: 1px solid var(--border);
  }
  .facts dt {
    color: var(--text-faint);
    margin-top: 8px;
  }
  .facts dt:first-child {
    margin-top: 0;
  }
  .facts dd {
    margin: 0;
  }
  @media (min-width: 640px) {
    .facts {
      grid-template-columns: auto 1fr;
      gap: 4px 12px;
    }
    .facts dt {
      white-space: nowrap;
      margin-top: 0;
    }
  }
  .confirm {
    margin-top: 10px;
  }
  .opening {
    display: flex;
    gap: 6px;
    align-items: baseline;
    cursor: pointer;
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
