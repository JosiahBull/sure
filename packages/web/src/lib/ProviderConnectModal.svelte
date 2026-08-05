<script lang="ts">
  import { onMount } from "svelte";
  import { api, colorFor, formatMoney, type Schemas } from "./api";
  import {
    FIELDS,
    KINDS,
    buildMetadata,
    isFieldRequired,
    kindToProfile,
    showsInstitution,
    type MetaField,
  } from "./accountMeta";
  import { providerInitials, providerLabel } from "./providerMeta";
  import {
    ensureLoaded as ensurePeopleLoaded,
    ownershipOptions,
    ownershipFromKey,
    defaultOwnershipKey,
  } from "./people.svelte";
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
    /** Who the new account belongs to (an `ownershipKey`); required, like every account. */
    owner: string;
    /** Raw metadata inputs, same shape `AccountForm` keeps and `buildMetadata` consumes. */
    meta: Record<string, string>;
  };

  /**
   * The metadata a newly-created account can't be stored without, for the kind this row is
   * being linked as.
   *
   * Amortising debt is why this exists: a mortgage or loan is projected from its terms, and a
   * bank feed doesn't report them — Akahu can say a mortgage exists without saying what
   * rate it's on or when that rate expires. Asking here is the difference between a linked
   * mortgage that forecasts and one that silently falls back to fitting a trend to a debt.
   * A `student_loan` has no terms to project from, but its two fields (lender and rate) are
   * asked for the ordinary reason: there is a person in this dialog and no feed reports them.
   * Everything else keeps the old behaviour: link now, fill in details later.
   */
  function requiredMetaFields(f: LinkFormState): MetaField[] {
    if (f.target !== "new") return [];
    return FIELDS[kindToProfile(f.kind)].filter((field) => isFieldRequired(field, f.kind, f.meta));
  }

  function metaComplete(f: LinkFormState): boolean {
    return requiredMetaFields(f).every((field) => (f.meta[field.key] ?? "").trim() !== "");
  }
  type GroupFormState = {
    target: string;
    name: string;
    currency: string;
    institution: string;
    owner: string;
  };

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

  /**
   * The key a brokerage platform's wallets are grouped under.
   *
   * Authorisation *and* institution, not institution alone: two people who each connect
   * their own Sharesies would otherwise have every wallet merged into one account.
   *
   * One function, because this is half of a lookup — `groupForms[g.key]`, seeded in
   * `discover` and read back when a row is expanded. Derived separately in those two
   * places, the halves drifted (the authorisation went into the grouping and not the
   * seeding), the lookup missed, and every brokerage row expanded onto an empty box with
   * no form and no way to link it. A single expression cannot disagree with itself.
   */
  const brokerageGroupKey = (a: Schemas["ProviderAccount"]) =>
    `${a.authorisation_id ?? ""}:${a.institution ?? a.external_id}`;

  // A brokerage platform (e.g. Sharesies) surfaces one upstream account per currency
  // wallet; group them and link together into a single Brokerage account.
  const brokerageGroups = $derived.by(() => {
    const groups = new Map<
      string,
      { key: string; institution: string | null; members: Schemas["ProviderAccount"][] }
    >();
    for (const a of discovered) {
      if (a.kind_hint !== "brokerage") continue;
      const key = brokerageGroupKey(a);
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

  /**
   * Discovery grouped by the upstream login it came through (`authorisation_id`).
   *
   * This is the only thing separating one household member's accounts from another's: a
   * feed reports no holder name, and two people banking at the same place share an
   * institution, a kind, and often an account *name* ("Emergency Fund" twice). Grouping by
   * login turns "29 accounts, good luck" into "these 11 are one person's".
   */
  type LoginGroup = {
    key: string;
    institutions: string[];
    brokerage: typeof brokerageGroups;
    singles: Schemas["ProviderAccount"][];
    count: number;
  };
  const loginGroups = $derived.by<LoginGroup[]>(() => {
    const keyOf = (a: Schemas["ProviderAccount"]) => a.authorisation_id ?? "";
    const groups = new Map<string, LoginGroup>();
    const groupFor = (key: string) => {
      let g = groups.get(key);
      if (!g) {
        g = { key, institutions: [], brokerage: [], singles: [], count: 0 };
        groups.set(key, g);
      }
      return g;
    };
    for (const bg of brokerageGroups) {
      const g = groupFor(keyOf(bg.members[0]));
      g.brokerage.push(bg);
      g.count++;
    }
    for (const a of singleAccounts) {
      const g = groupFor(keyOf(a));
      g.singles.push(a);
      g.count++;
    }
    for (const g of groups.values()) {
      const seen = new Set<string>();
      for (const a of [...g.singles, ...g.brokerage.flatMap((b) => b.members)]) {
        if (a.institution && !seen.has(a.institution)) {
          seen.add(a.institution);
          g.institutions.push(a.institution);
        }
      }
    }
    // Biggest first: the everyday-banking login is the one you came here to link.
    return [...groups.values()].sort((a, b) => b.count - a.count);
  });
  /** Whether grouping tells the user anything — one login means it's just a heading. */
  const showLoginGroups = $derived(loginGroups.length > 1);

  /**
   * External ids of accounts the *same* real account is exposed under twice — a joint
   * account is visible from both holders' logins, with its own nickname in each. Linking
   * both would sync one bank account into two Sure accounts and double it in net worth,
   * which is worth a warning rather than a discovery.
   */
  const sharedExternalIds = $derived.by(() => {
    const byNumber = new Map<string, Schemas["ProviderAccount"][]>();
    for (const a of discovered) {
      if (!a.account_number) continue;
      const key = `${a.institution ?? ""}:${a.account_number}`;
      byNumber.set(key, [...(byNumber.get(key) ?? []), a]);
    }
    const shared = new Set<string>();
    for (const rows of byNumber.values()) {
      if (new Set(rows.map((r) => r.authorisation_id ?? "")).size > 1) {
        for (const r of rows) shared.add(r.external_id);
      }
    }
    return shared;
  });

  /** Apply one owner to every not-yet-linked row in a login group, in one click. */
  function setGroupOwner(g: LoginGroup, owner: string) {
    for (const a of g.singles) {
      const f = linkForms[a.external_id];
      if (f) f.owner = owner;
    }
    for (const bg of g.brokerage) {
      const f = groupForms[bg.key];
      if (f) f.owner = owner;
    }
    groupOwner[g.key] = owner;
  }
  let groupOwner = $state<Record<string, string>>({});

  function apiErrorMessage(e: unknown, fallback: string): string {
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  onMount(async () => {
    if (accounts.length) pf.account_id = accounts[0].id;
    // The roster has to be in before `discover` seeds each row's owner default.
    await ensurePeopleLoaded();
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
          // Same key the rows are grouped under — see `brokerageGroupKey`; the row body is
          // only rendered when this lookup resolves, so a key that differs here is a
          // brokerage account that cannot be linked at all.
          groupForms[brokerageGroupKey(a)] ??= {
            target: "new",
            name: a.institution ?? a.name,
            currency: baseCurrency,
            institution: a.institution ?? "",
            owner: defaultOwnershipKey(),
          };
        } else {
          linkForms[a.external_id] ??= {
            target: "new",
            name: a.name,
            kind: a.kind_hint,
            currency: a.currency_code,
            institution: a.institution ?? "",
            // Whose account this is, is the one thing a feed can never tell us — and linking a
            // partner's newly-connected accounts is the case this whole flow exists for, so it
            // is asked here rather than left to a follow-up edit.
            owner: defaultOwnershipKey(),
            // The lender is the one term the feed does know.
            meta: a.institution ? { lender: a.institution } : {},
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
              metadata: buildMetadata(f.kind, f.meta),
              archived: false,
              sort_order: 0,
              ownership: ownershipFromKey(f.owner),
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
              ownership: ownershipFromKey(f.owner),
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
            {rowCount} account{rowCount === 1 ? "" : "s"} found{showLoginGroups
              ? `, across ${loginGroups.length} logins — one group per person who authorised them`
              : ""}. Pick one to bring into Sure.
          </p>

          {#each loginGroups as lg, i (lg.key)}
          {#if showLoginGroups}
            <div class="login-head">
              <div class="col" style="gap:2px;min-width:0">
                <span class="login-title"
                  >{lg.institutions.join(", ") || "Connection"} · login {i + 1}</span
                >
                <span class="meta"
                  >{lg.count} account{lg.count === 1 ? "" : "s"} from one login — usually one
                  person's, apart from any shared ones flagged below</span
                >
              </div>
              <label class="field group-owner">
                Owner for all
                <select
                  class="select"
                  value={groupOwner[lg.key] ?? ""}
                  onchange={(e) => setGroupOwner(lg, (e.currentTarget as HTMLSelectElement).value)}
                >
                  <option value="">Set each below…</option>
                  {#each ownershipOptions() as o (o.key)}<option value={o.key}>{o.label}</option>{/each}
                </select>
              </label>
            </div>
          {/if}

          {#each lg.brokerage as g (g.key)}
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
                      <label class="field">
                        Owner
                        <select class="select" bind:value={f.owner}>
                          {#each ownershipOptions() as o (o.key)}<option value={o.key}>{o.label}</option>{/each}
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

          {#each lg.singles as a (a.external_id)}
            {@const f = linkForms[a.external_id]}
            {@const open = openRow === a.external_id}
            <div class="row-card" class:open>
              <button class="row-head" onclick={() => toggleRow(a.external_id)} aria-expanded={open}>
                <span class="col" style="min-width:0;gap:3px;text-align:left">
                  <span class="name ell">{a.name}</span>
                  <span class="meta ell">
                    {[
                      a.institution,
                      // The account number is what tells two "Emergency Fund"s apart.
                      a.account_number,
                      formatMoney(a.balance_minor, a.currency_code),
                      a.supports_transactions ? null : "balance only",
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                  {#if sharedExternalIds.has(a.external_id)}
                    <span class="shared-flag">
                      Also visible from another login — the same account. Link it once.
                    </span>
                  {/if}
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
                      <label class="field">
                        Owner
                        <select class="select" bind:value={f.owner}>
                          {#each ownershipOptions() as o (o.key)}<option value={o.key}>{o.label}</option>{/each}
                        </select>
                      </label>
                      {#if showsInstitution(f.kind)}
                        <label class="field">
                          Institution
                          <input class="input" placeholder="e.g. ANZ" bind:value={f.institution} />
                        </label>
                      {/if}
                      {#each requiredMetaFields(f) as mf (mf.key)}
                        <label class="field">
                          {mf.label}
                          {#if mf.type === "select"}
                            <select class="select" bind:value={f.meta[mf.key]}>
                              {#each mf.options ?? [] as o (o.value)}
                                <option value={o.value}>{o.label}</option>
                              {/each}
                            </select>
                          {:else}
                            <input
                              class="input"
                              type={mf.type === "date" ? "date" : "text"}
                              inputmode={mf.type === "money" || mf.type === "percent" || mf.type === "int"
                                ? "decimal"
                                : undefined}
                              placeholder={mf.placeholder ?? ""}
                              bind:value={f.meta[mf.key]}
                            />
                          {/if}
                        </label>
                      {/each}
                    {/if}
                  </div>
                  {#if f.target === "new" && requiredMetaFields(f).length > 0}
                    <p class="small faint" style="margin:6px 0 0">
                      <!-- A student loan is the one kind here that isn't projected from terms — it
                           has none — so it gets the honest reason rather than the mortgage's. -->
                      {#if f.kind === "student_loan"}
                        A student loan records who it's with and what rate it's on; the bank feed
                        reports neither.
                      {:else}
                        A {f.kind === "mortgage" ? "mortgage" : "loan"} is projected from its terms,
                        and the bank feed doesn't report them.
                      {/if}
                      Prefer to do this later? Create the account on the Accounts page first, then
                      attach this one to it.
                    </p>
                  {/if}
                  <div class="actions">
                    <button
                      class="btn btn-primary btn-sm"
                      onclick={() => linkAccount(a)}
                      disabled={busy === a.external_id ||
                        (f.target === "new" && (!f.name.trim() || !metaComplete(f)))}
                    >
                      {busy === a.external_id ? "Linking…" : "Link account"}
                    </button>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
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
  /* One band per upstream login — the only thing that separates two people's accounts. */
  .login-head {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    margin: 16px 0 8px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .login-head:first-of-type {
    margin-top: 4px;
  }
  .login-title {
    font-size: 13px;
    font-weight: 650;
  }
  .group-owner {
    margin-left: auto;
    font-size: 12px;
    min-width: 170px;
  }
  /* A joint account shows up under both holders' logins; linking both would sync one bank
     account into two, and count it twice in net worth. */
  .shared-flag {
    font-size: 12px;
    color: var(--negative);
    font-weight: 600;
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
