<script lang="ts">
  import { onMount } from "svelte";
  import { api, colorFor, formatDate, type Schemas } from "../lib/api";
  import { providerInitials, providerLabel } from "../lib/providerMeta";
  import ProviderConnectModal from "../lib/ProviderConnectModal.svelte";

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

  // The connect/discovery flow lives in a modal rather than inline in the catalog card —
  // the card column is far too narrow for the link forms, and inline expansion pushed the
  // rest of the page down.
  let connectKind = $state<Schemas["ProviderKind"] | null>(null);

  // Existing-connection actions.
  let syncing = $state<number | null>(null);
  let syncingAll = $state(false);
  let confirmDelete = $state<number | null>(null);

  const autoSyncable = $derived(providers.filter((p) => !kindOf.get(p.kind)?.accepts_payload));

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
    loading = false;
  }
  onMount(load);

  function openConnect(k: Schemas["ProviderKind"]) {
    error = null;
    notice = null;
    connectKind = k;
  }

  async function runSync(id: number) {
    syncing = id;
    error = null;
    const { data, error: e } = await api.POST("/api/providers/{id}/sync", {
      params: { path: { id } },
      body: {},
    });
    if (e) error = apiErrorMessage(e, "Sync failed.");
    else if (data) notice = `Imported ${data.imported}, skipped ${data.skipped}.`;
    syncing = null;
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
  account discovery sync on a schedule and on demand; a manual source has nothing to poll, so its
  rows come in through <a href="#/settings/import">Import</a> as a file.
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
              <span class="avatar" style="background:{colorFor(p.kind)}">{providerInitials(p.kind)}</span>
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
                <!-- A manual source has nothing to poll, so its rows arrive as a file — which is
                     the Import page's job, for every source at once. This link carries the
                     connection's account so the upload lands where the connection points. -->
                <a class="btn btn-sm" href={`#/settings/import?account=${p.account_id}`}>Import</a>
              {:else}
                <button class="btn btn-sm" onclick={() => runSync(p.id)} disabled={syncing === p.id}>
                  {syncing === p.id ? "Syncing…" : "Sync now"}
                </button>
              {/if}
              <button class="btn btn-sm btn-danger" aria-label="Remove {p.name}" onclick={() => (confirmDelete = confirmDelete === p.id ? null : p.id)}>✕</button>
            </div>
          </div>
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
            <span class="avatar" style="background:{colorFor(k.kind)}">{providerInitials(k.kind)}</span>
            <div class="col grow" style="min-width:0;gap:6px">
              <span style="font-weight:600">{providerLabel(k.kind)}</span>
              <div class="row" style="gap:6px;flex-wrap:wrap">
                {#if k.supports_account_discovery}<span class="badge">Auto-discovery</span>{/if}
                {#if k.accepts_payload}<span class="badge">Manual import</span>{/if}
              </div>
            </div>
          </div>
          <p class="small muted grow" style="margin:10px 0 12px">{k.description}</p>
          <div class="row" style="justify-content:flex-end">
            <button class="btn btn-sm btn-primary" onclick={() => openConnect(k)}>
              {k.supports_account_discovery ? "Find accounts" : "Add connection"} →
            </button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
{/if}

{#if connectKind}
  <ProviderConnectModal
    kind={connectKind}
    {accounts}
    {currencies}
    {baseCurrency}
    onclose={() => (connectKind = null)}
    onchanged={(msg) => {
      notice = msg;
      load();
    }}
  />
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
  .confirm {
    margin-top: 10px;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
</style>
