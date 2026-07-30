<script lang="ts">
  import { onMount } from "svelte";
  import { api, colorFor, formatMoney, type Schemas } from "./api";
  import { KINDS, showsInstitution } from "./accountMeta";
  import { providerInitials, providerLabel } from "./providerMeta";
  import Icon from "./Icon.svelte";

  let {
    kind,
    accounts,
    currencies,
    baseCurrency,
    onclose,
    onchanged,
  }: {
    kind: Schemas["ProviderKind"];
    accounts: Schemas["Account"][];
    currencies: Schemas["Currency"][];
    baseCurrency: string;
    onclose: () => void;
    /** Something was linked/added: the parent should reload and surface `notice`. */
    onchanged: (notice: string) => void;
  } = $props();

  type LinkFormState = {
    target: string; // "new" or a stringified existing account id
    name: string;
    kind: Schemas["AccountKind"];
    currency: string;
    institution: string;
  };
  type GroupFormState = { target: string; name: string; currency: string; institution: string };

  let discovered = $state<Schemas["ProviderAccount"][]>([]);
  let discovering = $state(false);
  let discoverError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let linkForms = $state<Record<string, LinkFormState>>({});
  let groupForms = $state<Record<string, GroupFormState>>({});
  let busy = $state<string | null>(null);
  /** Row whose link form is open — only one at a time, so the list stays scannable. */
  let openRow = $state<string | null>(null);

  // Manual-import (payload) connect form.
  let pf = $state({ name: "", account_id: 0 });

  const label = $derived(providerLabel(kind.kind));

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
  const rowCount = $derived(brokerageGroups.length + singleAccounts.length);

  function apiErrorMessage(e: unknown, fallback: string): string {
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  onMount(() => {
    if (accounts.length) pf.account_id = accounts[0].id;
    if (kind.supports_account_discovery) discover();
  });

  async function discover() {
    discovering = true;
    discoverError = null;
    const { data, error: e } = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: kind.kind } },
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
    discovering = false;
  }

  async function linkAccount(a: Schemas["ProviderAccount"]) {
    const f = linkForms[a.external_id];
    if (!f) return;
    busy = a.external_id;
    error = null;
    const body: Schemas["LinkProviderAccount"] =
      f.target === "new"
        ? {
            kind: kind.kind,
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
            kind: kind.kind,
            external_id: a.external_id,
            name: `${label} — ${a.name}`,
            existing_account_id: Number(f.target),
          };
    const { error: e } = await api.POST("/api/providers/link", { body });
    if (e) {
      error = apiErrorMessage(e, "Failed to link account.");
    } else {
      discovered = discovered.filter((d) => d.external_id !== a.external_id);
      openRow = null;
      onchanged(`Linked ${a.name}.`);
    }
    busy = null;
  }

  async function linkGroup(g: { key: string; members: Schemas["ProviderAccount"][] }) {
    const f = groupForms[g.key];
    if (!f) return;
    busy = g.key;
    error = null;
    const members = g.members.map((m) => ({
      external_id: m.external_id,
      name: `${label} — ${m.name}`,
    }));
    const body: Schemas["LinkProviderGroup"] =
      f.target === "new"
        ? {
            kind: kind.kind,
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
        : { kind: kind.kind, members, existing_account_id: Number(f.target) };
    const { error: e } = await api.POST("/api/providers/link-group", { body });
    if (e) {
      error = apiErrorMessage(e, "Failed to link brokerage account.");
    } else {
      const ids = new Set(g.members.map((m) => m.external_id));
      discovered = discovered.filter((d) => !ids.has(d.external_id));
      openRow = null;
      onchanged(
        `Linked ${g.members.length} wallet${g.members.length === 1 ? "" : "s"} into one brokerage account.`,
      );
    }
    busy = null;
  }

  async function addProvider() {
    if (!pf.name.trim() || !pf.account_id) return;
    busy = "payload";
    error = null;
    const { error: e } = await api.POST("/api/providers", {
      body: { name: pf.name, kind: kind.kind, account_id: pf.account_id, enabled: true },
    });
    busy = null;
    if (e) {
      error = apiErrorMessage(e, "Failed to add connection.");
      return;
    }
    const added = pf.name;
    pf.name = "";
    onchanged(`Added ${added}.`);
    onclose();
  }

  function toggleRow(key: string) {
    openRow = openRow === key ? null : key;
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  // The settings nav (a filtered/animated ancestor) is a containing block for
  // `position: fixed`, which would trap the overlay inside it — reparent to <body>.
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }
</script>

<svelte:window onkeydown={onKey} />

<div
  class="overlay"
  use:portal
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) onclose();
  }}
>
  <div class="modal" role="dialog" aria-modal="true" aria-label="Connect {label}">
    <header class="head">
      <span class="avatar" style="background:{colorFor(kind.kind)}">
        {providerInitials(kind.kind)}
      </span>
      <div class="col" style="min-width:0;gap:2px">
        <h2>{label}</h2>
        <p class="sub">{kind.description}</p>
      </div>
      <button class="close" onclick={onclose} aria-label="Close">
        <Icon name="x" size={18} />
      </button>
    </header>

    <div class="body">
      {#if error}<div class="error-banner">{error}</div>{/if}

      {#if kind.supports_account_discovery}
        {#if discovering}
          <div class="row" style="justify-content:center;padding:32px">
            <span class="spinner"></span>
          </div>
        {:else if discoverError}
          <div class="error-banner">{discoverError}</div>
          <div class="row" style="justify-content:center;margin-top:12px">
            <button class="btn btn-sm" onclick={discover}>Try again</button>
          </div>
        {:else if rowCount === 0}
          <div class="empty">No accounts found to link.</div>
        {:else}
          <p class="hint">
            {rowCount} account{rowCount === 1 ? "" : "s"} found. Pick one to bring into Sure.
          </p>

          {#each brokerageGroups as g (g.key)}
            {@const f = groupForms[g.key]}
            {@const open = openRow === g.key}
            <div class="row-card" class:open>
              <button class="row-head" onclick={() => toggleRow(g.key)} aria-expanded={open}>
                <span class="col" style="min-width:0;gap:3px;text-align:left">
                  <span class="name ell">{g.institution ?? g.members[0]?.name}</span>
                  <span class="meta">
                    Brokerage · {g.members.length} wallet{g.members.length === 1 ? "" : "s"}
                  </span>
                </span>
                <span class="cta">{open ? "Cancel" : "Link"}</span>
              </button>

              {#if open && f}
                <div class="row-body">
                  <ul class="wallets">
                    {#each g.members as m (m.external_id)}
                      <li>
                        <span class="ell">{m.name}</span>
                        <span class="amount">{formatMoney(m.balance_minor, m.currency_code)}</span>
                      </li>
                    {/each}
                  </ul>
                  <div class="fields">
                    <label class="field wide">
                      Link to
                      <select class="select" bind:value={f.target}>
                        <option value="new">Create a new brokerage account</option>
                        {#each accounts as acc (acc.id)}
                          <option value={String(acc.id)}>Attach to “{acc.name}”</option>
                        {/each}
                      </select>
                    </label>
                    {#if f.target === "new"}
                      <label class="field">
                        Account name
                        <input class="input" bind:value={f.name} />
                      </label>
                      <label class="field">
                        Currency
                        <select class="select" bind:value={f.currency}>
                          {#each currencies as c (c.code)}<option value={c.code}>{c.code}</option>{/each}
                        </select>
                      </label>
                    {/if}
                  </div>
                  <div class="actions">
                    <button
                      class="btn btn-primary btn-sm"
                      onclick={() => linkGroup(g)}
                      disabled={busy === g.key || (f.target === "new" && !f.name.trim())}
                    >
                      {busy === g.key ? "Linking…" : "Link brokerage account"}
                    </button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}

          {#each singleAccounts as a (a.external_id)}
            {@const f = linkForms[a.external_id]}
            {@const open = openRow === a.external_id}
            <div class="row-card" class:open>
              <button class="row-head" onclick={() => toggleRow(a.external_id)} aria-expanded={open}>
                <span class="col" style="min-width:0;gap:3px;text-align:left">
                  <span class="name ell">{a.name}</span>
                  <span class="meta ell">
                    {[
                      a.institution,
                      formatMoney(a.balance_minor, a.currency_code),
                      a.supports_transactions ? null : "balance only",
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                </span>
                <span class="cta">{open ? "Cancel" : "Link"}</span>
              </button>

              {#if open && f}
                <div class="row-body">
                  <div class="fields">
                    <label class="field wide">
                      Link to
                      <select class="select" bind:value={f.target}>
                        <option value="new">Create a new account</option>
                        {#each accounts as acc (acc.id)}
                          <option value={String(acc.id)}>Attach to “{acc.name}”</option>
                        {/each}
                      </select>
                    </label>
                    {#if f.target === "new"}
                      <label class="field">
                        Account name
                        <input class="input" bind:value={f.name} />
                      </label>
                      <label class="field">
                        Type
                        <select class="select" bind:value={f.kind}>
                          {#each KINDS as kk (kk.value)}<option value={kk.value}>{kk.label}</option>{/each}
                        </select>
                      </label>
                      <label class="field">
                        Currency
                        <select class="select" bind:value={f.currency}>
                          {#each currencies as c (c.code)}<option value={c.code}>{c.code}</option>{/each}
                        </select>
                      </label>
                      {#if showsInstitution(f.kind)}
                        <label class="field">
                          Institution
                          <input class="input" placeholder="e.g. ANZ" bind:value={f.institution} />
                        </label>
                      {/if}
                    {/if}
                  </div>
                  <div class="actions">
                    <button
                      class="btn btn-primary btn-sm"
                      onclick={() => linkAccount(a)}
                      disabled={busy === a.external_id || (f.target === "new" && !f.name.trim())}
                    >
                      {busy === a.external_id ? "Linking…" : "Link account"}
                    </button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        {/if}
      {/if}

      {#if kind.accepts_payload}
        {#if kind.supports_account_discovery}<div class="divider"></div>{/if}
        {#if accounts.length === 0}
          <!-- A pasted-rows connection has nowhere to import to until an account exists. -->
          <div class="empty">Add an account first — imported rows need somewhere to land.</div>
        {:else}
          <p class="hint">
            Name the connection and choose the account its rows land in. You can then paste rows from
            the connection's <strong>Import</strong> button; re-imports skip duplicates.
          </p>
          <div class="fields">
            <label class="field">
              Connection name
              <input class="input" placeholder="e.g. ANZ everyday CSV" bind:value={pf.name} />
            </label>
            <label class="field">
              Import into
              <select class="select" bind:value={pf.account_id}>
                {#each accounts as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
              </select>
            </label>
          </div>
          <div class="actions">
            <button
              class="btn btn-primary btn-sm"
              onclick={addProvider}
              disabled={busy === "payload" || !pf.name.trim() || !pf.account_id}
            >
              {busy === "payload" ? "Adding…" : "Add connection"}
            </button>
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: max(24px, 5vh) 16px;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(2px);
  }
  /* Fixed-height dialog with an internally scrolling body, so a long discovery list
     never stretches the page behind it. */
  .modal {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 560px;
    max-height: 100%;
    border-radius: var(--r-lg);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.35);
  }
  .head {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 18px 20px 14px;
    border-bottom: 1px solid var(--border);
  }
  .head h2 {
    font-size: 17px;
    font-weight: 600;
  }
  .sub {
    margin: 0;
    font-size: 13px;
    line-height: 1.45;
    color: var(--text-muted);
  }
  .close {
    all: unset;
    margin-left: auto;
    flex: none;
    display: inline-flex;
    padding: 6px;
    border-radius: var(--r-sm);
    cursor: pointer;
    color: var(--text-faint);
  }
  .close:hover {
    background: var(--hover);
    color: var(--text);
  }
  .body {
    padding: 16px 20px 20px;
    overflow-y: auto;
  }
  .body .error-banner {
    margin-bottom: 12px;
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
  }
  .hint {
    margin: 0 0 12px;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text-muted);
  }

  /* One discovered account per card: collapsed to a scannable name + summary line, with
     the create/attach fields revealed only for the row being linked. */
  .row-card {
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface);
  }
  .row-card + .row-card {
    margin-top: 8px;
  }
  .row-card.open {
    border-color: var(--border-strong);
  }
  .row-head {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    padding: 12px 14px;
    cursor: pointer;
    border-radius: var(--r);
  }
  .row-head:hover {
    background: var(--hover);
  }
  .row-head:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .name {
    font-size: 14px;
    font-weight: 550;
  }
  .meta {
    font-size: 12px;
    color: var(--text-faint);
  }
  .cta {
    flex: none;
    margin-left: auto;
    padding: 4px 12px;
    border-radius: 999px;
    border: 1px solid var(--border-strong);
    font-size: 13px;
    font-weight: 550;
    color: var(--text-muted);
  }
  .row-head:hover .cta {
    border-color: var(--accent);
    color: var(--text);
  }
  .row-body {
    padding: 4px 14px 14px;
  }

  .wallets {
    list-style: none;
    margin: 0 0 14px;
    padding: 10px 0 0;
    border-top: 1px solid var(--border);
    font-size: 13px;
    color: var(--text-muted);
  }
  .wallets li {
    display: flex;
    gap: 12px;
    padding: 3px 0;
  }
  .amount {
    margin-left: auto;
    flex: none;
    font-variant-numeric: tabular-nums;
  }

  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 12px;
  }
  .fields .wide {
    grid-column: 1 / -1;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 14px;
  }
  .divider {
    border-top: 1px solid var(--border);
    margin: 18px 0;
  }
</style>
