<script lang="ts">
  import { onMount } from "svelte";
  import { api, colorFor, formatDate, formatMoney, type Schemas } from "../lib/api";
  import { KINDS, showsInstitution } from "../lib/accountMeta";

  let providerKinds = $state<Schemas["ProviderKind"][]>([]);
  let providers = $state<Schemas["Provider"][]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let currencies = $state<Schemas["Currency"][]>([]);
  let baseCurrency = $state("NZD");
  let loading = $state(true);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const accountName = $derived(new Map(accounts.map((a) => [a.id, a.name])));
  const kindOf = $derived(new Map(providerKinds.map((k) => [k.kind, k])));

  // --- catalog "connect" flow --------------------------------------------------
  // Only one catalog card's connect panel is open at a time.
  let openKind = $state<string | null>(null);

  // Manual-import (payload) connect form, reused per payload kind.
  let pf = $state({ name: "", account_id: 0 });

  // Existing-connection actions.
  let openImport = $state<number | null>(null);
  let csvText = $state<Record<number, string>>({});
  let syncing = $state<number | null>(null);
  let syncingAll = $state(false);
  let confirmDelete = $state<number | null>(null);

  // --- account discovery / linking --------------------------------------------
  type LinkFormState = {
    target: string; // "new" or a stringified existing account id
    name: string;
    kind: Schemas["AccountKind"];
    currency: string;
    institution: string;
  };
  type GroupFormState = { target: string; name: string; currency: string; institution: string };
  let discovered = $state<Schemas["ProviderAccount"][]>([]);
  let discoveringKind = $state<string | null>(null);
  let discoverError = $state<string | null>(null);
  let linkForms = $state<Record<string, LinkFormState>>({});
  let groupForms = $state<Record<string, GroupFormState>>({});
  let linking = $state<string | null>(null);
  let linkingGroup = $state<string | null>(null);

  // A brokerage platform (e.g. Sharesies) surfaces one upstream account per currency
  // wallet; group them by institution and link together into a single Brokerage account.
  const brokerageGroups = $derived.by(() => {
    const groups = new Map<
      string,
      { key: string; institution: string | null; members: Schemas["ProviderAccount"][] }
    >();
    for (const a of discovered) {
      if (a.kind_hint !== "brokerage") continue;
      const key = a.institution ?? a.external_id;
      let g = groups.get(key);
      if (!g) {
        g = { key, institution: a.institution ?? null, members: [] };
        groups.set(key, g);
      }
      g.members.push(a);
    }
    return [...groups.values()];
  });
  const singleAccounts = $derived(discovered.filter((a) => a.kind_hint !== "brokerage"));

  const autoSyncable = $derived(providers.filter((p) => !kindOf.get(p.kind)?.accepts_payload));

  function providerLabel(kind: string): string {
    return kind.length <= 3 ? kind.toUpperCase() : kind.charAt(0).toUpperCase() + kind.slice(1);
  }
  const initials = (kind: string) => kind.slice(0, 2).toUpperCase();
  function apiErrorMessage(e: unknown, fallback: string): string {
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  async function load() {
    const [pk, p, a, c, s] = await Promise.all([
      api.GET("/api/provider-kinds", {}),
      api.GET("/api/providers", {}),
      api.GET("/api/accounts", {}),
      api.GET("/api/currencies", {}),
      api.GET("/api/settings", {}),
    ]);
    providerKinds = pk.data ?? [];
    providers = p.data ?? [];
    accounts = a.data ?? [];
    currencies = c.data ?? [];
    baseCurrency = s.data?.base_currency_code ?? "NZD";
    if (accounts.length && !pf.account_id) pf.account_id = accounts[0].id;
    loading = false;
  }
  onMount(load);

  function toggleKind(k: Schemas["ProviderKind"]) {
    if (openKind === k.kind) {
      openKind = null;
      return;
    }
    openKind = k.kind;
    error = null;
    discovered = [];
    discoverError = null;
    if (accounts.length && !pf.account_id) pf.account_id = accounts[0].id;
    if (k.supports_account_discovery) discover(k.kind);
  }

  async function discover(kind: string) {
    discoveringKind = kind;
    discoverError = null;
    const { data, error: e } = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind } },
    });
    if (e) {
      discoverError = apiErrorMessage(e, "Discovery failed — check this provider's credentials.");
      discovered = [];
    } else {
      discovered = data ?? [];
      // Seed the link/group forms eagerly here — never as a side effect of rendering, which
      // trips Svelte 5's unsafe-mutation guard and silently aborts the {#each} render.
      for (const a of discovered) {
        if (a.kind_hint === "brokerage") {
          const key = a.institution ?? a.external_id;
          groupForms[key] ??= {
            target: "new",
            name: a.institution ?? a.name,
            currency: baseCurrency,
            institution: a.institution ?? "",
          };
        } else {
          linkForms[a.external_id] ??= {
            target: "new",
            name: a.name,
            kind: a.kind_hint,
            currency: a.currency_code,
            institution: a.institution ?? "",
          };
        }
      }
    }
    discoveringKind = null;
  }

  // Pure lookup — the form is seeded in discover(); this just reads it back.
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

  async function linkAccount(kind: string, a: Schemas["ProviderAccount"]) {
    const f = linkFormFor(a);
    linking = a.external_id;
    error = null;
    const label = providerLabel(kind);
    const body: Schemas["LinkProviderAccount"] =
      f.target === "new"
        ? {
            kind,
            external_id: a.external_id,
            name: `${label} — ${a.name}`,
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
            kind,
            external_id: a.external_id,
            name: `${label} — ${a.name}`,
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

  async function linkGroup(
    kind: string,
    g: { key: string; members: Schemas["ProviderAccount"][] },
  ) {
    const f = groupForms[g.key];
    if (!f) return;
    linkingGroup = g.key;
    error = null;
    const label = providerLabel(kind);
    const members = g.members.map((m) => ({ external_id: m.external_id, name: `${label} — ${m.name}` }));
    const body: Schemas["LinkProviderGroup"] =
      f.target === "new"
        ? {
            kind,
            members,
            new_account: {
              name: f.name,
              kind: "brokerage",
              currency_code: f.currency,
              institution: f.institution.trim() || null,
              archived: false,
              sort_order: 0,
            },
          }
        : { kind, members, existing_account_id: Number(f.target) };
    const { error: e } = await api.POST("/api/providers/link-group", { body });
    if (e) {
      error = apiErrorMessage(e, "Failed to link brokerage account.");
    } else {
      notice = `Linked ${g.members.length} wallet${g.members.length === 1 ? "" : "s"} into one brokerage account.`;
      const ids = new Set(g.members.map((m) => m.external_id));
      discovered = discovered.filter((d) => !ids.has(d.external_id));
    }
    linkingGroup = null;
    load();
  }

  async function addProvider(kind: string) {
    if (!pf.name.trim() || !pf.account_id) return;
    error = null;
    const { error: e } = await api.POST("/api/providers", {
      body: { name: pf.name, kind, account_id: pf.account_id, enabled: true },
    });
    if (e) {
      error = apiErrorMessage(e, "Failed to add connection.");
      return;
    }
    notice = `Added ${pf.name}.`;
    pf.name = "";
    openKind = null;
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
        payload === undefined
          ? "Sync failed."
          : "Import failed — check the columns (date, amount, description, …).",
      );
    } else if (data) {
      notice = `Imported ${data.imported}, skipped ${data.skipped}.`;
    }
    syncing = null;
    openImport = null;
    load();
  }

  async function syncAll() {
    syncingAll = true;
    error = null;
    let imported = 0;
    for (const p of autoSyncable) {
      const { data, error: e } = await api.POST("/api/providers/{id}/sync", {
        params: { path: { id: p.id } },
        body: {},
      });
      if (e) error = apiErrorMessage(e, `Sync failed for ${p.name}.`);
      else if (data) imported += data.imported;
    }
    if (!error) notice = `Synced ${autoSyncable.length} connection${autoSyncable.length === 1 ? "" : "s"} · ${imported} imported.`;
    syncingAll = false;
    load();
  }

  async function delProvider(id: number) {
    error = null;
    const { error: e } = await api.DELETE("/api/providers/{id}", { params: { path: { id } } });
    if (e) {
      error = apiErrorMessage(e, "Failed to remove connection.");
      return;
    }
    confirmDelete = null;
    load();
  }
</script>

<div class="row spread wrap" style="margin-bottom:6px;gap:10px">
  <h1 style="font-size:20px">Bank sync</h1>
  <button class="btn btn-sm" onclick={syncAll} disabled={syncingAll || autoSyncable.length === 0}>
    {syncingAll ? "Syncing…" : "↻ Sync all"}
  </button>
</div>
<p class="muted small" style="margin:0 0 16px;max-width:64ch">
  Connect external data sources so transactions, balances and holdings flow into Sure. Feeds with
  account discovery sync on a schedule and on demand; manual sources import when you paste rows.
</p>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}
{#if notice}<div class="badge" style="margin-bottom:12px">{notice}</div>{/if}

{#if loading}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  <div class="section-label">Your connections · {providers.length}</div>
  {#if providers.length === 0}
    <div class="empty">No connections yet — pick a source below to get started.</div>
  {:else}
    <div class="grid" style="gap:10px;margin-bottom:8px">
      {#each providers as p (p.id)}
        {@const k = kindOf.get(p.kind)}
        <div class="card conn">
          <div class="conn-head">
            <div class="row" style="gap:12px;min-width:0">
              <span class="avatar" style="background:{colorFor(p.kind)}">{initials(p.kind)}</span>
              <div class="col" style="min-width:0;gap:2px">
                <div class="row" style="gap:8px;min-width:0">
                  <span class="ell" style="font-weight:600">{p.name}</span>
                  <span class="badge">{providerLabel(p.kind)}</span>
                </div>
                <div class="small faint ell">
                  → {accountName.get(p.account_id) ?? "unknown account"}{p.last_synced_at
                    ? ` · synced ${formatDate(p.last_synced_at)}`
                    : ""}
                </div>
              </div>
            </div>
            <div class="row" style="gap:8px;margin-left:auto;flex:0 0 auto">
              <span class="badge ok">Connected</span>
              {#if k?.accepts_payload}
                <button class="btn btn-sm" onclick={() => (openImport = openImport === p.id ? null : p.id)}>
                  {openImport === p.id ? "Close" : "Import"}
                </button>
              {:else}
                <button class="btn btn-sm" onclick={() => runSync(p.id)} disabled={syncing === p.id}>
                  {syncing === p.id ? "Syncing…" : "Sync now"}
                </button>
              {/if}
              <button class="btn btn-sm btn-danger" aria-label="Remove {p.name}" onclick={() => (confirmDelete = confirmDelete === p.id ? null : p.id)}>✕</button>
            </div>
          </div>
          {#if openImport === p.id}
            <textarea
              class="mono"
              rows="4"
              style="margin-top:10px"
              placeholder={"date,amount,description,external_id\n2026-01-05,-12.50,Coffee,c1"}
              bind:value={csvText[p.id]}
            ></textarea>
            <div class="row" style="justify-content:flex-end;margin-top:8px">
              <button class="btn btn-primary btn-sm" onclick={() => runSync(p.id, csvText[p.id] ?? "")} disabled={syncing === p.id}>
                {syncing === p.id ? "Importing…" : "Run import"}
              </button>
            </div>
          {/if}
          {#if confirmDelete === p.id}
            <div class="confirm">
              <div class="small">Remove the <strong>{p.name}</strong> connection?</div>
              <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
                <button class="btn btn-sm" onclick={() => (confirmDelete = null)}>Cancel</button>
                <button class="btn btn-sm btn-danger" onclick={() => delProvider(p.id)}>Remove</button>
              </div>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  <div class="section-label" style="margin-top:22px">Available · {providerKinds.length}</div>
  {#if providerKinds.length === 0}
    <div class="empty">No providers available.</div>
  {:else}
    <div class="catalog">
      {#each providerKinds as k (k.kind)}
        <div class="card cat-card">
          <div class="row" style="gap:12px;align-items:flex-start">
            <span class="avatar" style="background:{colorFor(k.kind)}">{initials(k.kind)}</span>
            <div class="col grow" style="min-width:0;gap:6px">
              <span style="font-weight:600">{providerLabel(k.kind)}</span>
              <div class="row" style="gap:6px;flex-wrap:wrap">
                {#if k.supports_account_discovery}<span class="badge">Auto-discovery</span>{/if}
                {#if k.accepts_payload}<span class="badge">Manual import</span>{/if}
              </div>
            </div>
          </div>
          <p class="small muted" style="margin:10px 0 12px">{k.description}</p>
          <div class="row" style="justify-content:flex-end">
            <button class="btn btn-sm btn-primary" onclick={() => toggleKind(k)}>
              {#if openKind === k.kind}Close{:else if k.supports_account_discovery}Discover accounts →{:else}Add connection →{/if}
            </button>
          </div>

          {#if openKind === k.kind}
            <div class="flow">
              {#if k.supports_account_discovery}
                {#if discoveringKind === k.kind}
                  <div class="row" style="justify-content:center;padding:20px"><span class="spinner"></span></div>
                {:else}
                  {#if discoverError}<div class="error-banner" style="margin-bottom:10px">{discoverError}</div>{/if}
                  {#each brokerageGroups as g (g.key)}
                    {@const f = groupForms[g.key]}
                    <div class="line">
                      <div>
                        {#if g.institution}<span class="faint small">{g.institution} — </span>{/if}
                        Brokerage account <span class="badge">brokerage</span>
                        <span class="faint small">{g.members.length} wallet{g.members.length === 1 ? "" : "s"}</span>
                      </div>
                      <div class="small faint" style="margin-top:4px">
                        {#each g.members as m (m.external_id)}
                          <span style="margin-right:12px">{m.name} <span class="badge">{m.currency_code}</span> {formatMoney(m.balance_minor, m.currency_code)}</span>
                        {/each}
                      </div>
                      {#if f}
                        <div class="row wrap" style="gap:10px;margin-top:8px">
                          <select class="select" style="width:auto" bind:value={f.target}>
                            <option value="new">Create new brokerage account</option>
                            {#each accounts as acc}<option value={String(acc.id)}>Attach to "{acc.name}"</option>{/each}
                          </select>
                          {#if f.target === "new"}
                            <input class="input" style="min-width:120px" placeholder="Name" bind:value={f.name} />
                            <select class="select" style="width:auto" bind:value={f.currency}>
                              {#each currencies as c}<option value={c.code}>{c.code}</option>{/each}
                            </select>
                          {/if}
                          <button
                            class="btn btn-primary btn-sm"
                            onclick={() => linkGroup(k.kind, g)}
                            disabled={linkingGroup === g.key || (f.target === "new" && !f.name.trim())}
                          >
                            {linkingGroup === g.key ? "Linking…" : "Link as brokerage"}
                          </button>
                        </div>
                      {/if}
                    </div>
                  {/each}
                  {#each singleAccounts as a (a.external_id)}
                    {@const f = linkFormFor(a)}
                    <div class="line">
                      <div>
                        {#if a.institution}<span class="faint small">{a.institution} — </span>{/if}
                        {a.name}
                        <span class="badge">{a.currency_code}</span>
                        <span class="faint small">{formatMoney(a.balance_minor, a.currency_code)}</span>
                        {#if !a.supports_transactions}<span class="faint small">(balance only)</span>{/if}
                      </div>
                      <div class="row wrap" style="gap:10px;margin-top:8px">
                        <select class="select" style="width:auto" bind:value={f.target}>
                          <option value="new">Create new account</option>
                          {#each accounts as acc}<option value={String(acc.id)}>Attach to "{acc.name}"</option>{/each}
                        </select>
                        {#if f.target === "new"}
                          <input class="input" style="min-width:120px" placeholder="Name" bind:value={f.name} />
                          <select class="select" style="width:auto" bind:value={f.kind}>
                            {#each KINDS as kk}<option value={kk.value}>{kk.label}</option>{/each}
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
                          onclick={() => linkAccount(k.kind, a)}
                          disabled={linking === a.external_id || (f.target === "new" && !f.name.trim())}
                        >
                          {linking === a.external_id ? "Linking…" : "Link"}
                        </button>
                      </div>
                    </div>
                  {/each}
                  {#if discovered.length === 0 && !discoverError}
                    <div class="small faint">No accounts found to link.</div>
                  {/if}
                {/if}
              {/if}

              {#if k.accepts_payload}
                <div class="line">
                  <div class="small muted" style="margin-bottom:8px">
                    Add a connection, then use <strong>Import</strong> above to paste rows
                    (columns: date, amount, description, [merchant], [external_id]). Re-imports dedupe.
                  </div>
                  <div class="row wrap" style="gap:10px">
                    <input class="input grow" style="min-width:140px" placeholder="Connection name" bind:value={pf.name} />
                    <select class="select" style="width:auto" bind:value={pf.account_id}>
                      {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
                    </select>
                    <button class="btn btn-primary btn-sm" onclick={() => addProvider(k.kind)} disabled={!pf.name.trim() || !pf.account_id}>Add</button>
                  </div>
                </div>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .section-label {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-faint);
    margin-bottom: 10px;
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
  .avatar {
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 9px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .conn {
    padding: 14px;
  }
  .conn-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px 10px;
  }
  .badge.ok {
    color: var(--positive);
    background: color-mix(in srgb, var(--positive) 12%, transparent);
  }
  .catalog {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 12px;
    align-items: start;
  }
  .cat-card {
    display: flex;
    flex-direction: column;
  }
  .flow {
    margin-top: 12px;
    border-top: 1px solid var(--border);
    padding-top: 4px;
  }
  .line {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .line:first-child {
    border-top: none;
  }
  .confirm {
    margin-top: 10px;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
</style>
