<script lang="ts">
  // Import a zip of ASB exports covering several accounts at once — one file per account is
  // how ASB exports, so a household with a chequing account and half a dozen savings pots
  // would otherwise repeat the per-account import a dozen times.
  //
  // Nothing inside an export names a Sure account, so the shape is preview-then-assign:
  // the upload comes back described account by account, with a target pre-selected wherever
  // the server could prove one (a previous import of the same ASB account, a stored account
  // number, a number in the account's name). The user confirms or corrects, and the commit
  // sends every assignment explicitly — so what was on screen is exactly what runs.
  import { api, formatMoney, type Schemas } from "./api";
  import { takesBankCsv } from "./accountMeta";

  let { onchange }: { onchange?: () => void } = $props();

  type Export = Schemas["AsbImportResult"];
  type Result = Schemas["AsbUploadResult"];

  let busy = $state<null | "preview" | "refresh" | "import">(null);
  let error = $state<string | null>(null);
  let preview = $state<Result | null>(null);
  let done = $state<Result | null>(null);
  let pending = $state<File | null>(null);
  let fileInput = $state<HTMLInputElement | null>(null);
  /** Chosen account per ASB account number, keyed so a re-render can't lose it. */
  let chosen = $state<Record<string, number | null>>({});
  let accounts = $state<Schemas["Account"][]>([]);
  // On by default, matching the endpoint: without it an imported history starts from nothing,
  // because an account reads as 0 before its earliest transaction.
  let openingBalance = $state(true);

  const MATCH_LABEL: Record<string, string> = {
    assigned: "you chose it",
    previous_import: "imported here before",
    account_number: "matches the stored account number",
    account_name: "the number appears in the account name",
  };

  async function loadAccounts() {
    const { data } = await api.GET("/api/accounts", {});
    accounts = (data ?? []).filter((a) => takesBankCsv(a.kind) && !a.archived);
  }

  const contentType = (file: File) =>
    file.name.toLowerCase().endsWith(".zip") ? "application/zip" : "text/csv";

  /** A binary upload doesn't fit the JSON client, so post the raw bytes directly. */
  async function post(file: File, dryRun: boolean, assign?: string) {
    const params = new URLSearchParams({
      dry_run: String(dryRun),
      opening_balance: String(openingBalance),
    });
    if (assign) params.set("assign", assign);
    const res = await fetch(`/api/asb/import?${params}`, {
      method: "POST",
      headers: { "Content-Type": contentType(file) },
      body: file,
    });
    if (!res.ok) {
      const body = await res.json().catch(() => null);
      throw new Error(body?.error?.message ?? "Import failed — are these ASB exports?");
    }
    return (await res.json()) as Result;
  }

  async function pick(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    busy = "preview";
    error = null;
    done = null;
    preview = null;
    try {
      await loadAccounts();
      const result = await post(file, true);
      chosen = Object.fromEntries(result.exports.map((x) => [x.asb_account, x.account_id ?? null]));
      preview = result;
      pending = file;
    } catch (e) {
      error = e instanceof Error ? e.message : "Could not read that file.";
    }
    if (fileInput) fileInput.value = "";
    busy = null;
  }

  /**
   * Re-run the preview against the assignments now on screen.
   *
   * Not cosmetic: how many rows an export contributes depends on the *target account's*
   * cutover, so until an account is chosen the count can only be "all of them". Picking one
   * can only ever reduce it. Re-asking keeps the number on the button equal to the number
   * the commit will import, which is the whole point of previewing.
   */
  async function refresh() {
    if (!pending || busy) return;
    busy = "refresh";
    try {
      const result = await post(pending, true, assignments);
      // The user's choices win — they may have changed one while this was in flight.
      preview = result;
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
      done = await post(pending, false, assignments);
      preview = null;
      pending = null;
      onchange?.();
    } catch (e) {
      error = e instanceof Error ? e.message : "Import failed.";
    }
    busy = null;
  }

  function cancel() {
    preview = null;
    pending = null;
    error = null;
  }

  /** Every assignment the user is looking at, so the commit can't drift from the preview. */
  const assignments = $derived(
    Object.entries(chosen)
      .filter(([, id]) => id !== null)
      .map(([asb, id]) => `${asb}:${id}`)
      .join(","),
  );
  const ready = $derived(Object.values(chosen).filter((id) => id !== null).length);
  const rowsFor = (x: Export) => (chosen[x.asb_account] === null ? 0 : x.would_import);
  const totalRows = $derived(
    (preview?.exports ?? []).reduce((n, x) => n + rowsFor(x), 0),
  );

  /** Each export's warnings, tagged with the account they belong to. */
  const attributed = (exports: Export[]) =>
    exports.flatMap((x) => x.warnings.map((w) => `${x.asb_account}: ${w}`));

  const money = (minor: number | null | undefined, ccy: string) =>
    minor === null || minor === undefined ? "—" : formatMoney(minor, ccy);
  const currencyOf = (id: number | null) =>
    accounts.find((a) => a.id === id)?.currency_code ?? "NZD";
</script>

<div class="upload">
  {#if error}<div class="error-banner" style="margin-bottom:10px">{error}</div>{/if}

  {#if done}
    <div class="ok-banner">
      Imported {done.exports.reduce((n, x) => n + x.imported, 0)} transactions across
      {done.exports.filter((x) => x.imported > 0).length} account(s). They're uncategorised —
      run your rules from Settings → Rules to sort them.
    </div>
    {#each done.warnings as w}<div class="warn-banner">{w}</div>{/each}
    <table class="table">
      <thead>
        <tr><th>ASB account</th><th>Went to</th><th class="num">Imported</th><th class="num">Already here</th></tr>
      </thead>
      <tbody>
        {#each done.exports as x}
          <tr>
            <td class="mono small">{x.asb_account}</td>
            <td>{x.account_name ?? "— not imported"}</td>
            <td class="num tabular">{x.imported}</td>
            <td class="num tabular faint">{x.skipped}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#each attributed(done.exports) as w}<div class="warn-banner">{w}</div>{/each}
    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
      <button class="btn btn-sm" onclick={() => (done = null)}>Done</button>
    </div>
  {:else if preview}
    <div class="row spread wrap" style="gap:12px;align-items:baseline;margin-bottom:10px">
      <div>
        <div style="font-weight:600">
          {preview.exports.length} account{preview.exports.length === 1 ? "" : "s"} in this upload
        </div>
        <div class="small faint">
          Check each one is going to the right place. Nothing is saved until you import.
        </div>
      </div>
      <div class="stat" style="gap:2px;text-align:right">
        <div class="value tabular" style="font-size:20px" class:faint={busy === "refresh"}>
          {totalRows}
        </div>
        <div class="small faint">{busy === "refresh" ? "recounting…" : "to import"}</div>
      </div>
    </div>
    {#each preview.warnings as w}<div class="warn-banner">{w}</div>{/each}

    <div class="scroller">
      <table class="table">
        <thead>
          <tr>
            <th>ASB account</th>
            <th>Import into</th>
            <th class="num">Rows</th>
            <th>Covering</th>
            <th class="num">Closing balance</th>
            <th class="num">Opening balance</th>
          </tr>
        </thead>
        <tbody>
          {#each preview.exports as x}
            {@const ccy = currencyOf(chosen[x.asb_account])}
            <tr>
              <td>
                <div class="mono small">{x.asb_account}</div>
                {#if x.product}<div class="small faint">{x.product}</div>{/if}
                <div class="small faint ell" title={x.sources.join(", ")}>{x.sources.join(", ")}</div>
              </td>
              <td>
                <select
                  class="select"
                  bind:value={chosen[x.asb_account]}
                  onchange={refresh}
                  disabled={busy !== null}
                >
                  <option value={null}>— skip this one —</option>
                  {#each accounts as a}
                    <option value={a.id}>{a.name}</option>
                  {/each}
                </select>
                {#if x.matched_by && chosen[x.asb_account] === x.account_id}
                  <div class="small faint">{MATCH_LABEL[x.matched_by] ?? x.matched_by}</div>
                {:else if !x.matched_by && chosen[x.asb_account] === null}
                  <div class="small warn-text">nothing matched — choose one</div>
                {/if}
              </td>
              <td class="num">
                <div class="tabular">{rowsFor(x)}</div>
                {#if x.held_back > 0}
                  <div class="small faint">{x.held_back} held back{x.cutover ? ` from ${x.cutover}` : ""}</div>
                {/if}
              </td>
              <td class="small">
                {#if x.covered_from && x.covered_to}{x.covered_from} → {x.covered_to}{:else}—{/if}
              </td>
              <td class="num small">
                <div class="tabular">{money(x.ledger_balance_minor, ccy)}</div>
                {#if x.account_balance_minor !== null}
                  {#if x.account_balance_minor === x.ledger_balance_minor}
                    <div class="small pos">✓ matches</div>
                  {:else}
                    <div class="small faint tabular">{money(x.account_balance_minor, ccy)} here</div>
                  {/if}
                {/if}
              </td>
              <td class="num small">
                {#if x.opening_balance_minor !== null}
                  <div class="tabular">{money(x.opening_balance_minor, ccy)}</div>
                  <div class="small faint">as at {x.opening_balance_as_of}</div>
                {:else if x.implied_opening_minor !== null && !openingBalance}
                  <div class="faint">not recorded</div>
                {:else}
                  <div class="faint">—</div>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <!-- Only for exports that are actually going somewhere: the row's own "nothing matched"
         hint already says the rest, and a dozen copies of it is noise. Attributed, because
         with several accounts an unlabelled warning belongs to nobody. -->
    {#each attributed(preview.exports.filter((x) => chosen[x.asb_account] !== null)) as w}
      <div class="warn-banner">{w}</div>
    {/each}

    <div class="row spread wrap" style="gap:8px;margin-top:12px">
      <label class="opening small">
        <input type="checkbox" bind:checked={openingBalance} onchange={refresh} disabled={busy !== null} />
        <span>
          Record each account's opening balance — what it held before its export starts. Without
          it the history begins at zero instead of there.
        </span>
      </label>
      <div class="row" style="gap:8px">
      <button class="btn btn-sm" onclick={cancel} disabled={busy !== null}>Cancel</button>
      <button
        class="btn btn-sm btn-primary"
        onclick={commit}
        disabled={busy !== null || ready === 0}
      >
        {busy === "import" ? "Importing…" : `Import ${totalRows} into ${ready} account${ready === 1 ? "" : "s"}`}
      </button>
      </div>
    </div>
  {:else}
    <div class="row spread wrap" style="gap:12px">
      <div class="small faint" style="max-width:60ch;line-height:1.5">
        In ASB FastNet, export each account's transactions as <strong>CSV</strong> with the
        <code>YYYY/MM/DD</code> date format, put the files in one <code>.zip</code>, and drop it
        here. You'll assign each export to an account before anything is saved. Dates your bank
        feed already covers are held back, and re-uploading is free.
      </div>
      <button class="btn btn-sm btn-primary" onclick={() => fileInput?.click()} disabled={busy !== null}>
        {busy === "preview" ? "Reading…" : "Choose .zip or .csv"}
      </button>
      <input
        bind:this={fileInput}
        type="file"
        accept=".csv,.zip,text/csv,application/zip"
        style="display:none"
        onchange={pick}
      />
    </div>
  {/if}
</div>

<style>
  /* Sits under the page header like AccountForm does, so it reads as that button's panel. */
  .upload {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin-bottom: 12px;
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
  .warn-text {
    color: var(--warn);
  }
  .opening {
    display: flex;
    gap: 6px;
    align-items: baseline;
    max-width: 58ch;
    cursor: pointer;
    color: var(--text-faint);
  }
  /* A dozen accounts × five columns doesn't fit a narrow window; scroll the table rather
     than letting the page scroll sideways. */
  .scroller {
    overflow-x: auto;
  }
  .num {
    text-align: right;
  }
  .upload :global(.select) {
    max-width: 22ch;
  }
  code {
    background: var(--surface-2);
    border-radius: 4px;
    padding: 0 4px;
  }
</style>
