<script lang="ts">
  import { onMount } from "svelte";
  import { api, colorFor, formatDate, type Schemas } from "../lib/api";
  import { providerInitials, providerLabel } from "../lib/providerMeta";
  import ProviderConnectModal from "../lib/ProviderConnectModal.svelte";
  import StaleFeedNotice from "../lib/StaleFeedNotice.svelte";

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

  /**
   * What the badge on a connection says, from the last sync the server recorded.
   *
   * The row existing is not the same fact as the feed working, and this page used to conflate
   * them: every connection wore a green "Connected" for as long as it existed, so a bank the
   * upstream had retired — one whose balance had silently stopped moving — looked exactly like
   * one that synced an hour ago. `last_sync` is the server's answer to "what actually happened
   * last time", and these four states are the whole of it.
   *
   * `disconnected` is the one that needs a person. It is not a bad minute at the bank: the
   * connection behind the account was removed, expired, or re-authorised, and a re-authorisation
   * issues a new account id — so no amount of retrying repairs it, and the only fix is to link
   * the account again. Hence the explanation and the button rather than just a red dot.
   *
   * The `switch` has no `default` on purpose, and `strict` makes that load-bearing: a fourth
   * `SyncOutcome` regenerated into `schema.d.ts` fails `pnpm check` here with "Function lacks
   * ending return statement", which is this file's version of the exhaustive-match rule the Rust
   * side follows. A `default` would instead render the new state as whatever it fell back to.
   */
  type Health = { label: string; tone: "ok" | "idle" | "warn" | "bad"; detail?: string };

  function health(p: Schemas["Provider"]): Health {
    const last = p.last_sync;
    if (!last) return { label: "Not synced yet", tone: "idle" };
    switch (last.status) {
      case "ok":
        return { label: "Connected", tone: "ok" };
      case "disconnected":
        return { label: "Disconnected", tone: "bad", detail: last.detail ?? undefined };
      case "error":
        return { label: "Sync failing", tone: "warn", detail: last.detail ?? undefined };
    }
  }

  /** Re-link a retired connection: the same discovery dialog, opened on its kind. */
  function reconnect(p: Schemas["Provider"]) {
    const k = kindOf.get(p.kind);
    if (k) openConnect(k);
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
    notice = null;
    const { data, error: e } = await api.POST("/api/providers/{id}/sync", {
      params: { path: { id } },
      body: {},
    });
    if (e) error = apiErrorMessage(e, "Sync failed.");
    // Ahead of the cooldown check on purpose: a *replayed* disconnected run is still a
    // disconnected connection, and "already up to date" would be a comfortable lie about an
    // account that has stopped arriving at all.
    else if (data?.status === "disconnected") {
      // A 200 the user must not read as success. The server answers `Ok` deliberately — a
      // retired account is a state, not a failed request — so the wording has to carry the
      // difference the status code no longer does.
      error = data.detail ?? "This account is no longer connected upstream.";
    }
    // `fresh: false` means the cooldown was in force and this is the previous run coming back,
    // not a sync that just happened. Reporting its counts as new would claim an import that
    // did not occur — and the counts of a busy first sync would keep being re-announced on
    // every press.
    else if (data?.fresh === false)
      notice = `Already up to date — last synced ${formatDate(data.created_at)}. Not re-fetched.`;
    else if (data) notice = `Imported ${data.imported}, skipped ${data.skipped}.`;
    syncing = null;
    load();
  }

  /**
   * Sync every pollable connection, and keep going past the ones that cannot be.
   *
   * A household has several banks and they fail independently, so one dead connection must not
   * decide the fate of the run. This used to overwrite a single `error` per iteration and then
   * suppress the summary entirely if any of them had set it — so a run where three of four
   * connections imported normally reported one message about the fourth, and nothing at all
   * about the work that succeeded. Now every connection is counted and the summary always says
   * what happened to all of them.
   */
  async function syncAll() {
    syncingAll = true;
    error = null;
    notice = null;
    let imported = 0;
    let synced = 0;
    let skippedByCooldown = 0;
    const disconnected: string[] = [];
    const failed: string[] = [];


    for (const p of autoSyncable) {
      const { data, error: e } = await api.POST("/api/providers/{id}/sync", {
        params: { path: { id: p.id } },
        body: {},
      });
      // Same precedence as `runSync`, and for the same reason.
      if (e) failed.push(p.name);
      else if (data?.status === "disconnected") disconnected.push(p.name);
      else if (data?.fresh === false) skippedByCooldown += 1;
      else if (data) {
        synced += 1;
        imported += data.imported;
      }
    }

    // `synced` is counted from the runs that actually happened rather than derived by
    // subtracting from `autoSyncable.length`, now that a connection can leave the loop three
    // other ways: "Synced 3" must never mean "asked about 3 and contacted none".
    const plural = (n: number) => (n === 1 ? "" : "s");
    const upToDate = skippedByCooldown > 0 ? ` · ${skippedByCooldown} already up to date` : "";
    notice = `Synced ${synced} connection${plural(synced)} · ${imported} imported${upToDate}.`;
    // Both lists are named rather than counted: "1 disconnected" out of four connections sends
    // the user hunting for which one, and the badges below are the only other place it is said.
    const trouble = [
      disconnected.length > 0 ? `Disconnected: ${disconnected.join(", ")}.` : null,
      failed.length > 0 ? `Failed: ${failed.join(", ")}.` : null,
    ].filter(Boolean);
    if (trouble.length > 0) error = trouble.join(" ");


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

<!-- Above the list rather than only on the rows: the same notice the overview carries, so the
     two pages cannot end up describing the state differently. No `href` here — this *is* the
     page it would point at. -->
<StaleFeedNotice {providers} />

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
        {@const state = health(p)}
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
              <span class="badge {state.tone}">{state.label}</span>
              {#if k?.accepts_payload}
                <!-- A manual source has nothing to poll, so its rows arrive as a file — which is
                     the Import page's job, for every source at once. This link carries the
                     connection's account so the upload lands where the connection points. -->
                <a class="btn btn-sm" href={`#/settings/import?account=${p.account_id}`}>Import</a>
              {:else if state.tone === "bad"}
                <!-- No "Sync now" here: the account id this connection holds is gone upstream,
                     so a sync can only fail again. The button that helps is the one that starts
                     a fresh discovery. -->
                <button class="btn btn-sm btn-primary" onclick={() => reconnect(p)}>Reconnect</button>
              {:else}
                <button class="btn btn-sm" onclick={() => runSync(p.id)} disabled={syncing === p.id}>
                  {syncing === p.id ? "Syncing…" : "Sync now"}
                </button>
              {/if}
              <button class="btn btn-sm btn-danger" aria-label="Remove {p.name}" onclick={() => (confirmDelete = confirmDelete === p.id ? null : p.id)}>✕</button>
            </div>
          </div>
          {#if state.detail}
            <!-- The server's own words. For a disconnection they are the only place the fix is
                 spelled out; for a failing sync they are the upstream's message, which is what
                 makes "sync failing" diagnosable instead of just alarming. -->
            <p class="small detail" class:bad={state.tone === "bad"}>{state.detail}</p>
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
  /* The four connection states, in the order they escalate. `idle` keeps the default badge
     colours: "not synced yet" is a fact about a new connection, not a problem with it. */
  .badge.ok {
    color: var(--positive);
    background: color-mix(in srgb, var(--positive) 12%, transparent);
  }
  .badge.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .badge.bad {
    color: var(--negative);
    background: color-mix(in srgb, var(--negative) 12%, transparent);
  }
  .detail {
    margin: 10px 0 0;
    color: var(--text-muted);
    line-height: 1.45;
  }
  .detail.bad {
    color: var(--negative);
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
