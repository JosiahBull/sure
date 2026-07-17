<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatDate, formatMoney, type Schemas } from "../lib/api";
  import { theme, setTheme, resolvedTheme, type ThemePref } from "../lib/theme.svelte";
  import { KINDS, showsInstitution } from "../lib/accountMeta";

  const THEMES: { key: ThemePref; label: string; glyph: string }[] = [
    { key: "auto", label: "Auto", glyph: "◐" },
    { key: "light", label: "Light", glyph: "☀" },
    { key: "dark", label: "Dark", glyph: "☾" },
  ];

  let currencies = $state<Schemas["Currency"][]>([]);
  let settings = $state<Schemas["Settings"] | null>(null);
  let accounts = $state<Schemas["Account"][]>([]);
  let providers = $state<Schemas["Provider"][]>([]);
  let providerKinds = $state<Schemas["ProviderKind"][]>([]);
  let crons = $state<Schemas["Cron"][]>([]);
  let merchants = $state<Schemas["Merchant"][]>([]);
  let categories = $state<Schemas["Category"][]>([]);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const accountName = $derived(new Map(accounts.map((a) => [a.id, a.name])));
  const categoryName = $derived(new Map(categories.map((c) => [c.id, c.name])));

  // merchant + backup UI state
  let mf = $state({ name: "", category_id: "" as number | "" });
  let importText = $state("");

  // provider import UI
  let pf = $state({ name: "", account_id: 0 });
  let openImport = $state<number | null>(null);
  let csvText = $state<Record<number, string>>({});
  let syncing = $state<number | null>(null);

  // Akahu discovery/linking UI
  // `target` is "new" or a stringified existing account id — kept as a plain string
  // since HTML <select> values are always strings.
  type LinkFormState = {
    target: string;
    name: string;
    kind: Schemas["AccountKind"];
    currency: string;
    institution: string;
  };
  let discovered = $state<Schemas["ProviderAccount"][]>([]);
  let discovering = $state(false);
  let discoverError = $state<string | null>(null);
  let linkForms = $state<Record<string, LinkFormState>>({});
  let linking = $state<string | null>(null);

  // cron form
  let cf = $state({
    name: "",
    account_id: 0,
    kind: "appreciation",
    rate: "1",
    amount: "",
    start_date: new Date().toISOString().slice(0, 10),
  });

  async function load() {
    const [c, s, a, p, pk, cr, m, cat] = await Promise.all([
      api.GET("/api/currencies", {}),
      api.GET("/api/settings", {}),
      api.GET("/api/accounts", {}),
      api.GET("/api/providers", {}),
      api.GET("/api/provider-kinds", {}),
      api.GET("/api/crons", {}),
      api.GET("/api/merchants", {}),
      api.GET("/api/categories", {}),
    ]);
    currencies = c.data ?? [];
    settings = s.data ?? null;
    accounts = a.data ?? [];
    providers = p.data ?? [];
    providerKinds = pk.data ?? [];
    crons = cr.data ?? [];
    merchants = m.data ?? [];
    categories = cat.data ?? [];
    if (accounts.length) {
      if (!pf.account_id) pf.account_id = accounts[0].id;
      if (!cf.account_id) cf.account_id = accounts[0].id;
    }
  }
  onMount(load);

  function apiErrorMessage(e: unknown, fallback: string): string {
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  async function setBase(code: string) {
    await api.PUT("/api/settings", { body: { base_currency_code: code } });
    notice = "Base currency updated.";
    load();
  }

  async function addProvider() {
    if (!pf.name.trim() || !pf.account_id) return;
    await api.POST("/api/providers", {
      body: { name: pf.name, kind: "csv", account_id: pf.account_id, enabled: true },
    });
    pf.name = "";
    load();
  }
  async function runSync(id: number, payload?: string) {
    syncing = id;
    error = null;
    const { data, error: e } = await api.POST("/api/providers/{id}/sync", {
      params: { path: { id } },
      body: payload === undefined ? {} : { payload },
    });
    if (e) {
      error = apiErrorMessage(
        e,
        payload === undefined ? "Sync failed." : "Import failed — check the CSV columns (date, amount, …).",
      );
    } else if (data) {
      notice = `Imported ${data.imported}, skipped ${data.skipped}.`;
    }
    syncing = null;
    openImport = null;
    load();
  }
  async function delProvider(id: number) {
    await api.DELETE("/api/providers/{id}", { params: { path: { id } } });
    load();
  }

  // Pure lookup only — populated eagerly in `discoverAkahu`, never as a side effect of
  // rendering. Writing to `linkForms` while it's being read inside the `{#each}` block
  // (as this used to do) trips Svelte 5's unsafe-mutation guard and silently aborts that
  // block's render, which is why the list appeared empty despite the API returning data.
  function linkFormFor(a: Schemas["ProviderAccount"]): LinkFormState {
    return (
      linkForms[a.external_id] ?? {
        target: "new",
        name: a.name,
        kind: a.kind_hint,
        currency: a.currency_code,
        institution: a.institution ?? "",
      }
    );
  }

  async function discoverAkahu() {
    discovering = true;
    discoverError = null;
    const { data, error: e } = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: "akahu" } },
    });
    if (e) {
      discoverError = apiErrorMessage(
        e,
        "Discovery failed — check AKAHU_APP_TOKEN / AKAHU_USER_TOKEN are set.",
      );
      discovered = [];
    } else {
      discovered = data ?? [];
      for (const a of discovered) {
        linkForms[a.external_id] ??= {
          target: "new",
          name: a.name,
          kind: a.kind_hint,
          currency: a.currency_code,
          institution: a.institution ?? "",
        };
      }
    }
    discovering = false;
  }

  async function linkAkahuAccount(a: Schemas["ProviderAccount"]) {
    const f = linkFormFor(a);
    linking = a.external_id;
    error = null;
    const body: Schemas["LinkProviderAccount"] =
      f.target === "new"
        ? {
            kind: "akahu",
            external_id: a.external_id,
            name: `Akahu — ${a.name}`,
            new_account: {
              name: f.name,
              kind: f.kind,
              currency_code: f.currency,
              institution: f.institution.trim() || null,
              archived: false,
              sort_order: 0,
            },
          }
        : {
            kind: "akahu",
            external_id: a.external_id,
            name: `Akahu — ${a.name}`,
            existing_account_id: Number(f.target),
          };
    const { error: e } = await api.POST("/api/providers/link", { body });
    if (e) {
      error = apiErrorMessage(e, "Failed to link account.");
    } else {
      notice = `Linked ${a.name}.`;
      discovered = discovered.filter((d) => d.external_id !== a.external_id);
    }
    linking = null;
    load();
  }

  async function addCron() {
    if (!cf.name.trim() || !cf.account_id) return;
    const fixed = cf.kind === "fixed_transaction";
    const body: Schemas["SaveCron"] = {
      name: cf.name,
      account_id: cf.account_id,
      kind: cf.kind,
      start_date: cf.start_date,
      enabled: true,
      rate_bps: fixed ? null : Math.round(parseFloat(cf.rate || "0") * 100),
      amount_minor: fixed ? Math.round(parseFloat(cf.amount || "0") * 100) : null,
    };
    const { error: e } = await api.POST("/api/crons", { body });
    if (e) {
      error = "Failed to add scheduled adjustment.";
      return;
    }
    cf.name = "";
    load();
  }
  async function runCron(id: number) {
    const { data } = await api.POST("/api/crons/{id}/run", { params: { path: { id } } });
    if (data) notice = `Applied ${data.applied} period(s).`;
    load();
  }
  async function delCron(id: number) {
    await api.DELETE("/api/crons/{id}", { params: { path: { id } } });
    load();
  }

  async function addMerchant() {
    if (!mf.name.trim()) return;
    const { error: e } = await api.POST("/api/merchants", {
      body: { name: mf.name, category_id: mf.category_id === "" ? null : mf.category_id },
    });
    if (e) {
      error = "Failed to add merchant — the name may already exist.";
      return;
    }
    mf.name = "";
    load();
  }
  async function delMerchant(id: number) {
    await api.DELETE("/api/merchants/{id}", { params: { path: { id } } });
    load();
  }

  async function exportConfig() {
    const { data } = await api.GET("/api/config/export", {});
    if (!data) return;
    const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `sure-config-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(url);
    notice = "Config exported.";
  }
  async function importConfig() {
    error = null;
    let parsed: unknown;
    try {
      parsed = JSON.parse(importText);
    } catch {
      error = "That isn't valid JSON.";
      return;
    }
    // The snapshot body is an opaque JSON blob (serde_json::Value on the backend).
    const { error: e } = await api.POST("/api/config/import", { body: parsed as never });
    if (e) {
      error = "Import failed — check the snapshot.";
      return;
    }
    notice = "Config imported.";
    importText = "";
    load();
  }
</script>

<h1 style="font-size:20px;margin-bottom:14px">Settings</h1>
{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}
{#if notice}<div class="badge" style="margin-bottom:12px">{notice}</div>{/if}

<div class="grid" style="gap:14px">
  <section class="card">
    <h2>Appearance</h2>
    <p class="muted small" style="margin-top:0">
      Auto follows your device{theme.pref === "auto"
        ? ` — currently ${resolvedTheme()}`
        : ""}.
    </p>
    <div class="segmented" role="group" aria-label="Theme">
      {#each THEMES as t}
        <button
          type="button"
          class="seg"
          class:active={theme.pref === t.key}
          aria-pressed={theme.pref === t.key}
          onclick={() => setTheme(t.key)}
        >
          <span aria-hidden="true">{t.glyph}</span>
          {t.label}
        </button>
      {/each}
    </div>
  </section>

  <section class="card">
    <h2>Base currency</h2>
    <p class="muted small" style="margin-top:0">Reports are normalised into this currency.</p>
    <div class="row" style="gap:10px">
      <select
        class="select"
        style="width:auto"
        value={settings?.base_currency_code ?? "NZD"}
        onchange={(e) => setBase(e.currentTarget.value)}
      >
        {#each currencies as c}<option value={c.code}>{c.code} — {c.name}</option>{/each}
      </select>
    </div>
  </section>

  <section class="card">
    <h2>Bank feeds (Akahu)</h2>
    <p class="muted small" style="margin-top:0">
      Requires <code>AKAHU_APP_TOKEN</code> / <code>AKAHU_USER_TOKEN</code> in the server's
      environment. Discovers your connected NZ bank accounts and links each one to a new or
      existing local account; syncs automatically every 6h, or on demand from the Providers
      list below.
    </p>
    <div class="row" style="margin-bottom:12px">
      <button class="btn btn-primary" onclick={discoverAkahu} disabled={discovering}>
        {discovering ? "Discovering…" : "Discover accounts"}
      </button>
    </div>
    {#if discoverError}<div class="error-banner" style="margin-bottom:12px">{discoverError}</div>{/if}
    {#each discovered as a (a.external_id)}
      {@const f = linkFormFor(a)}
      <div class="line">
        <div class="row spread">
          <span>
            {#if a.institution}<span class="faint small">{a.institution} —</span>{/if}
            {a.name}
            <span class="badge">{a.currency_code}</span>
            <span class="faint small">{formatMoney(a.balance_minor, a.currency_code)}</span>
            {#if !a.supports_transactions}<span class="faint small">(balance only — no transaction history)</span>{/if}
          </span>
        </div>
        <div class="row wrap" style="gap:10px;margin-top:8px">
          <select class="select" style="width:auto" bind:value={f.target}>
            <option value="new">Create new account</option>
            {#each accounts as acc}<option value={String(acc.id)}>Attach to "{acc.name}"</option>{/each}
          </select>
          {#if f.target === "new"}
            <input class="input" style="min-width:120px" placeholder="Name" bind:value={f.name} />
            <select class="select" style="width:auto" bind:value={f.kind}>
              {#each KINDS as k}<option value={k.value}>{k.label}</option>{/each}
            </select>
            <select class="select" style="width:auto" bind:value={f.currency}>
              {#each currencies as c}<option value={c.code}>{c.code}</option>{/each}
            </select>
            {#if showsInstitution(f.kind)}
              <input class="input" style="min-width:120px" placeholder="Institution (e.g. ANZ)" bind:value={f.institution} />
            {/if}
          {/if}
          <button
            class="btn btn-primary btn-sm"
            onclick={() => linkAkahuAccount(a)}
            disabled={linking === a.external_id || (f.target === "new" && !f.name.trim())}
          >
            {linking === a.external_id ? "Linking…" : "Link"}
          </button>
        </div>
      </div>
    {/each}
    {#if !discovering && discovered.length === 0 && !discoverError}
      <div class="small faint">No undiscovered accounts yet — click "Discover accounts".</div>
    {/if}
  </section>

  <section class="card">
    <h2>Providers</h2>
    <p class="muted small" style="margin-top:0">
      Every connected data source, CSV or bank feed. Add a CSV connection below, then paste
      rows to import (columns: date, amount, description, [merchant], [external_id]).
      Re-imports dedupe automatically.
    </p>
    <div class="row wrap" style="gap:10px;margin-bottom:12px">
      <input class="input grow" style="min-width:140px" placeholder="Connection name" bind:value={pf.name} />
      <select class="select" style="width:auto" bind:value={pf.account_id}>
        {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
      </select>
      <button class="btn btn-primary" onclick={addProvider}>Add</button>
    </div>
    {#each providers as p (p.id)}
      {@const acceptsPayload = providerKinds.find((k) => k.kind === p.kind)?.accepts_payload}
      <div class="line">
        <div class="row spread">
          <span>{p.name} <span class="badge">{p.kind}</span> <span class="faint small">→ {accountName.get(p.account_id) ?? "?"}</span></span>
          <div class="row" style="gap:8px">
            {#if acceptsPayload}
              <button class="btn btn-sm" onclick={() => (openImport = openImport === p.id ? null : p.id)}>Import</button>
            {:else}
              <button class="btn btn-sm" onclick={() => runSync(p.id)} disabled={syncing === p.id}>
                {syncing === p.id ? "Syncing…" : "Sync now"}
              </button>
            {/if}
            <button class="btn btn-sm btn-danger" onclick={() => delProvider(p.id)}>✕</button>
          </div>
        </div>
        {#if openImport === p.id}
          <textarea
            class="mono"
            rows="4"
            style="margin-top:8px"
            placeholder={"date,amount,description,external_id\n2026-01-05,-12.50,Coffee,c1"}
            bind:value={csvText[p.id]}
          ></textarea>
          <div class="row" style="justify-content:flex-end;margin-top:8px">
            <button class="btn btn-primary btn-sm" onclick={() => runSync(p.id, csvText[p.id] ?? "")} disabled={syncing === p.id}>
              {syncing === p.id ? "Importing…" : "Run import"}
            </button>
          </div>
        {/if}
      </div>
    {/each}
  </section>

  <section class="card">
    <h2>Scheduled adjustments</h2>
    <p class="muted small" style="margin-top:0">
      e.g. a house that appreciates 1%/yr, or a recurring subscription. Applied monthly and
      recorded so each period can be undone.
    </p>
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(120px,1fr));gap:10px;margin-bottom:12px">
      <input class="input" placeholder="Name" bind:value={cf.name} />
      <select class="select" bind:value={cf.account_id}>
        {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
      </select>
      <select class="select" bind:value={cf.kind}>
        <option value="appreciation">Appreciation</option>
        <option value="depreciation">Depreciation</option>
        <option value="interest">Interest</option>
        <option value="fixed_transaction">Fixed transaction</option>
      </select>
      {#if cf.kind === "fixed_transaction"}
        <input class="input tabular" placeholder="Amount (−spend)" bind:value={cf.amount} />
      {:else}
        <input class="input tabular" placeholder="Rate %/yr" bind:value={cf.rate} />
      {/if}
      <input class="input" type="date" bind:value={cf.start_date} />
      <button class="btn btn-primary" onclick={addCron}>Add</button>
    </div>
    {#each crons as c (c.id)}
      <div class="line row spread">
        <span>
          {c.name} <span class="badge">{c.kind}</span>
          {#if c.last_run_on}<span class="faint small">last: {formatDate(c.last_run_on)}</span>{/if}
        </span>
        <div class="row" style="gap:8px">
          <button class="btn btn-sm" onclick={() => runCron(c.id)}>Run</button>
          <button class="btn btn-sm btn-danger" onclick={() => delCron(c.id)}>✕</button>
        </div>
      </div>
    {/each}
    {#if crons.length === 0}<div class="small faint">None yet.</div>{/if}
  </section>

  <section class="card">
    <h2>Merchants</h2>
    <p class="muted small" style="margin-top:0">
      Custom payees you can assign to transactions or have rules assign automatically.
      A default category is a hint for future automation.
    </p>
    <div class="row wrap" style="gap:10px;margin-bottom:12px">
      <input class="input grow" style="min-width:140px" placeholder="Merchant name" bind:value={mf.name} />
      <select class="select" style="width:auto" bind:value={mf.category_id}>
        <option value="">Default category…</option>
        {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
      </select>
      <button class="btn btn-primary" onclick={addMerchant}>Add</button>
    </div>
    {#each merchants as m (m.id)}
      <div class="line row spread">
        <span>
          {m.name}
          {#if m.category_id}<span class="badge" style="margin-left:6px">→ {categoryName.get(m.category_id) ?? "?"}</span>{/if}
        </span>
        <button class="btn btn-sm btn-danger" onclick={() => delMerchant(m.id)}>✕</button>
      </div>
    {/each}
    {#if merchants.length === 0}<div class="small faint">None yet.</div>{/if}
  </section>

  <section class="card">
    <h2>Backup &amp; restore</h2>
    <p class="muted small" style="margin-top:0">
      Export the whole configuration and data as JSON (handy for rapid dev iteration), or
      paste a snapshot to restore. Import <strong>replaces everything</strong>.
    </p>
    <div class="row" style="margin-bottom:10px">
      <button class="btn btn-primary" onclick={exportConfig}>Export JSON</button>
    </div>
    <textarea
      class="mono"
      rows="3"
      placeholder="Paste a snapshot JSON here to restore…"
      bind:value={importText}
    ></textarea>
    <div class="row" style="justify-content:flex-end;margin-top:8px">
      <button class="btn btn-danger" onclick={importConfig} disabled={!importText.trim()}>
        Import &amp; replace
      </button>
    </div>
  </section>
</div>

<style>
  .line {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .line:first-of-type {
    border-top: none;
  }

  /* Theme picker — three-way segmented control */
  .segmented {
    display: inline-flex;
    padding: 3px;
    gap: 3px;
    border-radius: var(--r);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
  }
  .seg {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 16px;
    border: none;
    border-radius: calc(var(--r) - 4px);
    background: transparent;
    color: var(--text-muted);
    font-family: inherit;
    font-size: 14px;
    font-weight: 550;
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .seg:hover {
    color: var(--text);
  }
  .seg.active {
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 650;
  }
</style>
