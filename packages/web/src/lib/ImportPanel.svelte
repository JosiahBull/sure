<script lang="ts">
  // The one import surface. Every file that comes into Sure comes through here.
  //
  // It replaces four panels that each did a bit of this — a multi-account ASB upload, a
  // per-account ASB import, a myIR upload, a Sharesies zip — and disagreed on everything a
  // person would expect to be the same: two of them previewed, one could be undone, each took a
  // different set of extensions, and three promised drag-and-drop that none of them implemented.
  //
  // Two steps on purpose. Picking files previews them — what the file is, how many rows, where
  // each is going, whether the balances agree — and only then does a second request commit. The
  // preview runs the same code path as the commit up to one branch, so it can't describe an
  // import that wouldn't happen, and the commit sends every assignment back explicitly, so what
  // was on screen is exactly what runs.
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "./api";
  import { uploadBody } from "./zip";

  let {
    accountId,
    currency = "NZD",
    onchange,
  }: { accountId?: number; currency?: string; onchange?: () => void } = $props();

  type Result = Schemas["ImportResult"];
  type Item = Schemas["ImportItem"];
  type Source = Schemas["ImportSource"];

  /** Why the server thinks an item belongs to an account, in words. */
  const MATCH_LABEL: Record<string, string> = {
    assigned: "you chose it",
    previous_import: "imported here before",
    account_number: "matches the stored account number",
    account_name: "the number appears in the account name",
    only_candidate: "the only account this can go to",
    transaction_history: "its transactions match this account's",
  };

  const SOURCE_LABEL: Record<Source, string> = {
    asb_csv: "ASB transaction export",
    myir_sls: "myIR student-loan export",
    sharesies_zip: "Sharesies export",
    csv_upload: "transaction CSV",
  };

  let busy = $state<null | "preview" | "refresh" | "import" | "undo">(null);
  let error = $state<string | null>(null);
  let preview = $state<Result | null>(null);
  let done = $state<Result | null>(null);
  let pending = $state<File[]>([]);
  let fileInput = $state<HTMLInputElement | null>(null);
  let dragging = $state(false);
  /** Chosen account per source account, keyed so a re-render can't lose it. */
  let chosen = $state<Record<string, number | null>>({});
  let accounts = $state<Schemas["Account"][]>([]);
  // On by default, matching the endpoint: without it an imported history starts from nothing,
  // because an account reads as 0 before its earliest transaction.
  let openingBalance = $state(true);
  /** Set only after a failed detect, so the person can say what the file is. */
  let sourceOverride = $state<Source | "">("");
  let history = $state<Schemas["ImportRecord"][]>([]);
  let confirmUndo = $state<Source | null>(null);
  let notice = $state<string | null>(null);
  /** What an undo couldn't take back — a backfilled valuation series outlives its holdings. */
  let undoWarnings = $state<string[]>([]);

  async function load() {
    const [a, h] = await Promise.all([
      api.GET("/api/accounts", {}),
      api.GET("/api/imports", {
        params: { query: accountId === undefined ? {} : { account_id: accountId } },
      }),
    ]);
    accounts = (a.data ?? []).filter((x) => !x.archived);
    history = h.data ?? [];
  }
  onMount(load);

  /**
   * A binary upload doesn't fit the JSON client (openapi-fetch serialises bodies), so post the
   * bytes straight to the same-origin API — dev proxies `/api` to the backend.
   */
  async function post(files: File[], dryRun: boolean, assign?: string) {
    const built = await uploadBody(files);
    if ("error" in built) throw new Error(built.error);

    const params = new URLSearchParams({
      dry_run: String(dryRun),
      opening_balance: String(openingBalance),
    });
    if (assign) params.set("assign", assign);
    if (sourceOverride) params.set("source", sourceOverride);
    const res = await fetch(`/api/import?${params}`, {
      method: "POST",
      headers: { "Content-Type": built.contentType },
      body: built.body,
    });
    if (!res.ok) {
      const body = await res.json().catch(() => null);
      throw new Error(body?.error?.message ?? "Import failed.");
    }
    return (await res.json()) as Result;
  }

  async function choose(files: File[]) {
    if (files.length === 0) return;
    busy = "preview";
    error = null;
    done = null;
    preview = null;
    // Held before the request, not after it succeeds: a failed *detect* is the case where the
    // source picker below offers a retry, and it has to have the same files to retry with.
    pending = files;
    try {
      await load();
      const result = await post(files, true);
      // Pre-scoped to one account: the row this panel sits in *is* the answer, whatever the
      // routing tiers worked out — which is what the old per-account endpoints did by having
      // the account in their path.
      chosen = Object.fromEntries(
        result.items.map((x) => [x.source_account, accountId ?? x.account_id ?? null]),
      );
      preview = result;
      // Pre-scoped, so the account was decided by where this panel is rather than by the routing
      // tiers — and the row count depends on *that* account's cutover, so it has to be re-asked.
      if (accountId !== undefined) await refresh();
    } catch (e) {
      error = e instanceof Error ? e.message : "Could not read that file.";
    }
    if (fileInput) fileInput.value = ""; // so re-picking the same file fires `change` again
    busy = null;
  }

  /**
   * Re-run the preview against the assignments now on screen.
   *
   * Not cosmetic: how many rows an item contributes depends on the *target account's* cutover,
   * so until an account is chosen the count can only be "all of them". Picking one can only
   * reduce it. Re-asking keeps the number on the button equal to the number the commit will
   * import, which is the whole point of previewing.
   */
  async function refresh() {
    if (pending.length === 0 || busy === "refresh" || busy === "import") return;
    busy = "refresh";
    try {
      preview = await post(pending, true, assignments);
    } catch (e) {
      error = e instanceof Error ? e.message : "Could not re-read that file.";
    }
    busy = null;
  }

  async function commit() {
    if (pending.length === 0) return;
    busy = "import";
    error = null;
    try {
      done = await post(pending, false, assignments);
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
    pending = [];
    sourceOverride = "";
  }

  async function undo(source: Source) {
    if (accountId === undefined) return;
    busy = "undo";
    error = null;
    notice = null;
    undoWarnings = [];
    const { data, error: e } = await api.DELETE("/api/import/{account_id}/{source}", {
      params: { path: { account_id: accountId, source } },
    });
    if (e) {
      error = (e as { error?: { message?: string } })?.error?.message ?? "Could not undo.";
    } else if (data) {
      const extras = data.extras
        .filter((x) => x.skipped > 0)
        .map((x) => `${x.skipped} ${x.kind}`);
      const also = extras.length ? `, and ${extras.join(" and ")}` : "";
      // Names the figure, because "removed" with no number can't be told from "did nothing" —
      // and a second Remove on the same source legitimately deletes 0.
      notice = data.deleted || extras.length
        ? `Removed ${data.deleted} transaction${data.deleted === 1 ? "" : "s"}${also}.`
        : "Nothing to remove — that importer's rows are already gone.";
      undoWarnings = data.warnings;
      await load();
      onchange?.();
    }
    confirmUndo = null;
    busy = null;
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    choose([...(e.dataTransfer?.files ?? [])]);
  }

  /** Every assignment on screen, so the commit can't drift from the preview. */
  const assignments = $derived(
    Object.entries(chosen)
      .filter(([, id]) => id !== null)
      .map(([source, id]) => `${source}:${id}`)
      .join(","),
  );
  const ready = $derived(Object.values(chosen).filter((id) => id !== null).length);
  const rowsFor = (x: Item) => (chosen[x.source_account] === null ? 0 : x.would_import);
  const totalRows = $derived((preview?.items ?? []).reduce((n, x) => n + rowsFor(x), 0));
  /** Only worth asking about when a source in this upload can actually offer one. */
  const offersOpening = $derived(
    (preview?.items ?? []).some((x) => x.reconciliation?.implied_opening_minor != null),
  );
  /** The distinct sources that have written to this account — one undo each, and no more. */
  const undoableSources = $derived([...new Set(history.map((h) => h.source))]);
  /** With several accounts in one upload, an unlabelled warning belongs to nobody. */
  const attributed = (items: Item[]) =>
    items.flatMap((x) => x.warnings.map((w) => `${x.source_account}: ${w}`));

  const money = (minor: number | null | undefined, ccy: string) =>
    minor === null || minor === undefined ? "—" : formatMoney(minor, ccy);
  const currencyOf = (id: number | null) =>
    accounts.find((a) => a.id === id)?.currency_code ?? currency;
  const nameOf = (id: number) => accounts.find((a) => a.id === id)?.name ?? `account ${id}`;
  /** The accounts a source can legitimately go to, so the picker can't offer a wrong one. */
  const targetsFor = (source: Source) =>
    accounts.filter((a) => ACCEPTS[source](a.kind));
  const BANK_KINDS = ["cash", "bank", "savings", "credit_card", "revolving_credit"];
  const ACCEPTS: Record<Source, (kind: string) => boolean> = {
    asb_csv: (k) => BANK_KINDS.includes(k),
    myir_sls: (k) => k === "student_loan",
    sharesies_zip: (k) => k === "brokerage",
    csv_upload: () => true,
  };
</script>

<div class="import">
  {#if error}
    <div class="error-banner" style="margin-bottom:10px">{error}</div>
    <!-- A file the sniff couldn't place is the one error worth offering a next step for: the
         upload is probably fine and only its kind is in question. -->
    <div class="row wrap" style="gap:8px;align-items:center;margin-bottom:10px">
      <span class="small faint">Not what you expected? Say what the file is:</span>
      <select class="select" bind:value={sourceOverride} onchange={() => choose(pending)}>
        <option value="">Work it out from the file</option>
        {#each Object.entries(SOURCE_LABEL) as [value, label]}
          <option {value}>{label}</option>
        {/each}
      </select>
    </div>
  {/if}
  {#if notice}<div class="ok-banner">{notice}</div>{/if}
  {#each undoWarnings as w}<div class="warn-banner">{w}</div>{/each}

  {#if done}
    {@const imported = done.items.reduce((n, x) => n + x.imported, 0)}
    {@const into = done.items.filter((x) => x.imported > 0).length}
    <div class="ok-banner">
      {#if imported}
        Imported {imported} transaction{imported === 1 ? "" : "s"} from your
        {SOURCE_LABEL[done.source]}{into > 1 ? ` across ${into} accounts` : ""}. They're
        uncategorised — run your rules from Settings → Rules to sort them.
      {:else}
        Nothing new — every row in this {SOURCE_LABEL[done.source]} was already here.
      {/if}
    </div>
    {#each done.warnings as w}<div class="warn-banner">{w}</div>{/each}
    <div class="scroller">
      <table class="table">
        <thead>
          <tr>
            <th>From</th><th>Went to</th><th class="num">Imported</th>
            <th class="num">Already here</th><th class="num">Held back</th>
          </tr>
        </thead>
        <tbody>
          {#each done.items as x}
            <tr>
              <td class="mono small">{x.source_account}</td>
              <td>{x.account_name ?? "— not imported"}</td>
              <td class="num tabular">{x.imported}</td>
              <td class="num tabular faint">{x.skipped}</td>
              <td class="num tabular faint">{x.held_back}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#each attributed(done.items) as w}<div class="warn-banner">{w}</div>{/each}
    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
      <button class="btn btn-sm" onclick={() => (done = null)}>Done</button>
    </div>
  {:else if preview}
    <div class="row spread wrap" style="gap:12px;align-items:baseline;margin-bottom:10px">
      <div>
        <div style="font-weight:600">
          {SOURCE_LABEL[preview.source]} · {preview.items.length} account{preview.items.length === 1
            ? ""
            : "s"}
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
            <th>From</th>
            <th>Import into</th>
            <th class="num">Rows</th>
            <th>Covering</th>
            <th class="num">Balance</th>
          </tr>
        </thead>
        <tbody>
          {#each preview.items as x}
            {@const ccy = currencyOf(chosen[x.source_account])}
            <tr>
              <td>
                <div class="mono small">{x.source_account}</div>
                {#if x.label}<div class="small faint">{x.label}</div>{/if}
                {#if x.sources.length}
                  <div class="small faint ell" title={x.sources.join(", ")}>
                    {x.sources.join(", ")}
                  </div>
                {/if}
              </td>
              <td>
                <select
                  class="select"
                  bind:value={chosen[x.source_account]}
                  onchange={refresh}
                  disabled={busy !== null || accountId !== undefined}
                >
                  <option value={null}>— skip this one —</option>
                  {#each targetsFor(preview.source) as a}
                    <option value={a.id}>{a.name}</option>
                  {/each}
                </select>
                {#if x.matched_by && chosen[x.source_account] === x.account_id}
                  <div class="small faint">{MATCH_LABEL[x.matched_by] ?? x.matched_by}</div>
                {:else if !x.matched_by && chosen[x.source_account] === null}
                  <div class="small warn-text">nothing matched — choose one</div>
                {/if}
              </td>
              <td class="num">
                <div class="tabular">{rowsFor(x)}</div>
                {#if x.held_back > 0}
                  <div class="small faint">
                    {x.held_back} held back{x.cutover ? ` from ${x.cutover}` : ""}
                  </div>
                {/if}
                {#each x.extras as extra}
                  <div class="small faint">+{extra.imported || extra.skipped} {extra.kind}</div>
                {/each}
              </td>
              <td class="small">
                {#if x.covered_from && x.covered_to}{x.covered_from} → {x.covered_to}{:else}—{/if}
              </td>
              <td class="num small">
                {#if x.reconciliation}
                  {@const r = x.reconciliation}
                  <div class="tabular">{money(r.ledger_balance_minor, ccy)}</div>
                  {#if r.account_balance_minor !== null}
                    {#if r.account_balance_minor === r.ledger_balance_minor}
                      <div class="small pos">✓ matches</div>
                    {:else}
                      <div class="small faint tabular">
                        {money(r.account_balance_minor, ccy)} here
                      </div>
                    {/if}
                  {/if}
                  {#if r.opening_balance_minor !== null}
                    <div class="small faint">
                      opens {money(r.opening_balance_minor, ccy)}
                      {#if r.opening_balance_as_of}· {r.opening_balance_as_of}{/if}
                    </div>
                  {/if}
                {:else}
                  <span class="faint">—</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <!-- Only for items that are actually going somewhere: a skipped row's own "nothing matched"
         hint already says the rest, and a dozen copies of it is noise. -->
    {#each attributed(preview.items.filter((x) => chosen[x.source_account] !== null)) as w}
      <div class="warn-banner">{w}</div>
    {/each}

    <div class="row spread wrap" style="gap:8px;margin-top:12px">
      {#if offersOpening}
        <label class="opening small">
          <input
            type="checkbox"
            bind:checked={openingBalance}
            onchange={refresh}
            disabled={busy !== null}
          />
          <span>
            Record each account's opening balance — what it held before its export starts. Without
            it the history begins at zero instead of there.
          </span>
        </label>
      {:else}
        <span></span>
      {/if}
      <div class="row" style="gap:8px">
        <button class="btn btn-sm" onclick={cancel} disabled={busy !== null}>Cancel</button>
        <button
          class="btn btn-sm btn-primary"
          onclick={commit}
          disabled={busy !== null || ready === 0}
        >
          {busy === "import"
            ? "Importing…"
            : `Import ${totalRows}${accountId === undefined ? ` into ${ready} account${ready === 1 ? "" : "s"}` : ""}`}
        </button>
      </div>
    </div>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="drop"
      class:dragging
      ondragover={(e) => {
        e.preventDefault();
        dragging = true;
      }}
      ondragleave={() => (dragging = false)}
      ondrop={onDrop}
    >
      <div class="small faint" style="max-width:64ch;line-height:1.5">
        Drop your exports here, or
        <button class="link" onclick={() => fileInput?.click()} disabled={busy !== null}>
          {busy === "preview" ? "reading…" : "choose files"}</button
        >. A bank transaction <code>.csv</code>, a myIR <code>.xlsx</code>, a Sharesies
        <code>.zip</code>, or several at once — Sure works out what each one is. You'll see what
        it contains, and where it's going, before anything is saved. Re-uploading is free: rows
        already here are skipped, and dates a bank feed already covers are held back so nothing
        is counted twice.
      </div>
      <input
        bind:this={fileInput}
        type="file"
        multiple
        accept=".csv,.xlsx,.zip,text/csv,application/zip,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        style="display:none"
        onchange={(e) => choose([...((e.currentTarget as HTMLInputElement).files ?? [])])}
      />
    </div>

    {#if history.length}
      <!-- "Import history", not "Imported here": it is a log of what was done, and an undo below
           can empty the account without changing a word of it. A heading claiming these rows are
           present would be wrong the moment someone uses one. -->
      <div class="section-label" style="margin-top:14px">Import history</div>
      <div class="scroller">
        <table class="table">
          <thead>
            <tr>
              <th>When</th>
              {#if accountId === undefined}<th>Account</th>{/if}
              <th>From</th>
              <th class="num">Rows</th>
              <th>Covering</th>
            </tr>
          </thead>
          <tbody>
            {#each history as h (h.id)}
              <tr>
                <td class="small">{h.created_at.slice(0, 10)}</td>
                {#if accountId === undefined}<td class="small">{nameOf(h.account_id)}</td>{/if}
                <td class="small">
                  {SOURCE_LABEL[h.source]}
                  {#if h.source_account}
                    <div class="mono small faint">{h.source_account}</div>
                  {/if}
                </td>
                <td class="num tabular">{h.imported}</td>
                <td class="small">
                  {#if h.covered_from && h.covered_to}{h.covered_from} → {h.covered_to}{:else}—{/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      <!-- One control per *source*, not per row above, because that is what undo does. A button on
           every log row would imply four separate undos where there is one: overlapping uploads
           share their content-derived ids, so a later upload's rows were skipped rather than
           written and there is nothing of it on its own to take back. -->
      {#if accountId !== undefined && undoableSources.length}
        <div class="row wrap" style="gap:8px;align-items:center;margin-top:10px">
          <span class="small faint">Take an importer's rows back out:</span>
          {#each undoableSources as source}
            <button
              class="btn btn-sm btn-danger"
              onclick={() => (confirmUndo = source)}
              disabled={busy !== null}
            >
              Remove {SOURCE_LABEL[source]}
            </button>
          {/each}
        </div>
      {/if}

      {#if confirmUndo}
        <div class="confirm">
          <div class="small">
            Remove everything the {SOURCE_LABEL[confirmUndo]} importer added to this account? Rows
            from your bank feed and anything you entered by hand are left alone. This takes back
            <strong>every</strong> import from that source, not just the most recent one —
            overlapping uploads share their rows, so there is no separate "last import" to undo.
          </div>
          <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
            <button class="btn btn-sm" onclick={() => (confirmUndo = null)}>Cancel</button>
            <button class="btn btn-sm btn-danger" onclick={() => undo(confirmUndo!)}>
              {busy === "undo" ? "Removing…" : "Remove"}
            </button>
          </div>
        </div>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .import {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  /* The drop target has to look like one before anything is dragged over it, or nobody tries. */
  .drop {
    border: 1px dashed var(--border);
    border-radius: var(--r);
    padding: 18px 14px;
    text-align: center;
    transition: background 120ms ease, border-color 120ms ease;
  }
  .drop.dragging {
    border-color: var(--accent, var(--positive));
    background: color-mix(in srgb, var(--positive) 8%, transparent);
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--text);
    text-decoration: underline;
    cursor: pointer;
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
  /* A dozen accounts × five columns doesn't fit a narrow window; scroll the table rather than
     letting the page scroll sideways. */
  .scroller {
    overflow-x: auto;
  }
  .num {
    text-align: right;
  }
  .import :global(.select) {
    max-width: 22ch;
  }
  .confirm {
    margin-top: 10px;
  }
  code {
    background: var(--surface-2);
    border-radius: 4px;
    padding: 0 4px;
  }
</style>
