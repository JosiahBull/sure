<script lang="ts">
  import { untrack } from "svelte";
  import { api, type Schemas } from "./api";
  import {
    KINDS,
    FIELDS,
    kindToProfile,
    isLiabilityKind,
    showsInstitution,
    showsCreditLimit,
    buildMetadata,
    metadataToRaw,
    type MetaField,
  } from "./accountMeta";

  let {
    account = null,
    initialKind = null,
    currencies,
    accounts,
    onsave,
    oncancel,
  }: {
    account?: Schemas["Account"] | null;
    /** Pre-selected kind when creating a new account (e.g. picked from the New-account menu). */
    initialKind?: Schemas["AccountKind"] | null;
    currencies: Schemas["Currency"][];
    accounts: Schemas["Account"][];
    onsave: () => void;
    oncancel: () => void;
  } = $props();

  // The parent (re)mounts one form per account, so we snapshot the incoming values once.
  const initial = untrack(() => account);
  const editing = !!initial;

  let name = $state(initial?.name ?? "");
  let kind = $state<Schemas["AccountKind"]>(untrack(() => initial?.kind ?? initialKind ?? "bank"));
  let currency = $state(initial?.currency_code ?? "NZD");
  let institution = $state(initial?.institution ?? "");
  let raw = $state<Record<string, string>>(metadataToRaw(initial?.metadata));
  let securedBy = $state<number | "">(initial?.secured_by_account_id ?? "");
  let error = $state<string | null>(null);
  let busy = $state(false);

  const fields = $derived(
    FIELDS[kindToProfile(kind)].filter((f) => f.key !== "credit_limit_minor" || showsCreditLimit(kind)),
  );
  const showSecured = $derived(isLiabilityKind(kind));
  const showInstitution = $derived(showsInstitution(kind));
  // Assets this liability could be secured against (never itself).
  const assets = $derived(accounts.filter((a) => a.class === "asset" && a.id !== initial?.id));

  const numericHint = (t: MetaField["type"]) =>
    t === "int" ? "numeric" : t === "money" || t === "percent" ? "decimal" : undefined;

  async function save() {
    if (!name.trim()) {
      error = "Account name is required.";
      return;
    }
    busy = true;
    error = null;
    const body = {
      name: name.trim(),
      kind,
      currency_code: currency,
      // Only depository kinds carry an institution; loans use `lender`, shares `broker`.
      institution: showInstitution ? institution.trim() || null : null,
      metadata: buildMetadata(kind, raw),
      archived: initial?.archived ?? false,
      sort_order: initial?.sort_order ?? 0,
    };

    let id = initial?.id;
    if (editing && id !== undefined) {
      const { error: e } = await api.PUT("/api/accounts/{id}", { params: { path: { id } }, body });
      if (e) {
        error = "Failed to save account.";
        busy = false;
        return;
      }
    } else {
      const { data, error: e } = await api.POST("/api/accounts", { body });
      if (e || !data) {
        error = "Failed to create account.";
        busy = false;
        return;
      }
      id = data.id;
    }

    // Keep the secured-against link in sync for liabilities (a separate endpoint).
    if (showSecured && id !== undefined) {
      const target = securedBy === "" ? null : Number(securedBy);
      if (target !== (initial?.secured_by_account_id ?? null)) {
        await api.PUT("/api/accounts/{id}/secured-by", {
          params: { path: { id } },
          body: { secured_by_account_id: target },
        });
      }
    }

    busy = false;
    onsave();
  }
</script>

<section class="card" style="margin-bottom:14px">
  <h2 style="margin-bottom:4px">{editing ? `Edit ${initial?.name}` : "New account"}</h2>

  {#if error}<div class="error-banner" style="margin:8px 0">{error}</div>{/if}

  <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-top:8px">
    <label class="field">Name<input class="input" bind:value={name} /></label>
    <label class="field">Type
      <select class="select" bind:value={kind}>
        {#each KINDS as k}<option value={k.value}>{k.label}</option>{/each}
      </select>
    </label>
    <label class="field">Currency
      <select class="select" bind:value={currency}>
        {#each currencies as c}<option value={c.code}>{c.code}</option>{/each}
      </select>
    </label>
    {#if showInstitution}
      <label class="field">Institution<input class="input" bind:value={institution} placeholder="e.g. ANZ" /></label>
    {/if}
  </div>

  <div class="details">Details</div>
  <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr))">
    {#each fields as f (f.key)}
      <label class="field">{f.label}
        {#if f.type === "textarea"}
          <textarea rows="2" bind:value={raw[f.key]}></textarea>
        {:else if f.type === "select"}
          <select class="select" bind:value={raw[f.key]}>
            {#each f.options ?? [] as o}<option value={o.value}>{o.label}</option>{/each}
          </select>
        {:else if f.type === "date"}
          <input class="input" type="date" bind:value={raw[f.key]} />
        {:else}
          <input
            class="input"
            class:tabular={f.type === "money" || f.type === "percent" || f.type === "int"}
            inputmode={numericHint(f.type)}
            placeholder={f.placeholder ?? ""}
            bind:value={raw[f.key]}
          />
        {/if}
      </label>
    {/each}
  </div>

  {#if showSecured}
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-top:6px">
      <label class="field">Secured against
        <select class="select" bind:value={securedBy}>
          <option value="">Not secured</option>
          {#each assets as a}<option value={a.id}>{a.name}</option>{/each}
        </select>
      </label>
    </div>
  {/if}

  <div class="row" style="justify-content:flex-end;gap:8px;margin-top:14px">
    <button class="btn" onclick={oncancel} disabled={busy}>Cancel</button>
    <button class="btn btn-primary" onclick={save} disabled={busy}>
      {editing ? "Save" : "Create"}
    </button>
  </div>
</section>

<style>
  .details {
    margin: 16px 0 8px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
    border-top: 1px solid var(--border);
    padding-top: 12px;
  }
</style>
