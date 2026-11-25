<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api, type Schemas } from "./api";
  import { categoryOptions } from "./categories";
  import { providerLabel } from "./providerMeta";

  let { categories }: { categories: Schemas["Category"][] } = $props();
  type Mapping = Schemas["CategoryMapping"];
  let mappings = $state<Mapping[]>([]);
  let providerKinds = $state<string[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let saving = $state(false);
  let editing = $state<number | "new" | null>(null);
  let search = $state("");
  let sourceFilter = $state("");
  let unmappedOnly = $state(false);
  let source = $state("akahu");
  let group = $state("");
  let name = $state("");
  let destination = $state<number | "">("");
  let searchInput: HTMLInputElement;
  const options = $derived(categoryOptions(categories));
  const labels = $derived(new Map(options.map(o => [o.id, o.label])));
  const sources = $derived([...new Set([...providerKinds, "asb", "sharesies", "csv-upload", "myir", ...mappings.map(m => m.source)])].sort());
  const sourceLabel = (value: string): string =>
    ({ "csv-upload": "CSV upload", asb: "ASB CSV", myir: "myIR" } as Record<string, string>)[value] ?? providerLabel(value);
  const visible = $derived(mappings.filter(m =>
    (!sourceFilter || m.source === sourceFilter) &&
    (!unmappedOnly || m.category_id === null) &&
    `${m.source} ${m.source_group} ${m.source_name} ${labels.get(m.category_id ?? 0) ?? ""}`.toLowerCase().includes(search.toLowerCase())
  ));

  function message(e: unknown): string {
    return (e as { error?: { message?: string } })?.error?.message ?? "Could not save the mapping. Try again.";
  }
  async function load() {
    loading = true;
    error = null;
    try {
      const { data, error: e } = await api.GET("/api/category-mappings", {});
      if (e) { error = message(e); return; }
      mappings = data ?? [];
    } catch { error = "Could not load import mappings. Check the connection and try again."; }
    finally { loading = false; }
  }
  onMount(() => {
    void load();
    void api.GET("/api/provider-kinds", {}).then(r => { providerKinds = r.data?.map(k => k.kind) ?? []; }).catch(() => {});
  });

  function edit(mapping?: Mapping) {
    error = null;
    editing = mapping?.id ?? "new";
    source = mapping?.source ?? (sourceFilter || "akahu");
    group = mapping?.source_group ?? "";
    name = mapping?.source_name ?? "";
    destination = mapping?.category_id ?? "";
  }
  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!source.trim() || !name.trim()) { error = "Source and source category are required."; return; }
    saving = true;
    error = null;
    const body: Schemas["SaveCategoryMapping"] = {
      source: source.trim(), source_group: group.trim(), source_name: name.trim(),
      category_id: destination === "" ? null : Number(destination),
    };
    try {
      const result = editing === "new"
        ? await api.POST("/api/category-mappings", { body })
        : await api.PUT("/api/category-mappings/{id}", { params: { path: { id: Number(editing) } }, body });
      if (result.error) { error = message(result.error); return; }
      const savedId = result.data!.id;
      editing = null;
      await load();
      saving = false;
      await tick();
      const button = document.getElementById(`mapping-edit-${savedId}`);
      if (button) button.focus(); else searchInput?.focus();
    } catch { error = "Could not save the mapping. Check the connection and try again."; }
    finally { saving = false; }
  }
</script>

<p class="muted mapping-help">Used when no merchant default or transaction rule applies. Changes apply to future imports.</p>
<div class="row spread wrap" style="gap:12px;margin:16px 0">
  <div class="row wrap" style="gap:12px;flex:1">
    <label class="field search">Search mappings<input class="input" type="search" bind:this={searchInput} bind:value={search} /></label>
    <label class="field">Source<select class="select" bind:value={sourceFilter}>
      <option value="">All sources</option>
      {#each sources as value}<option value={value}>{sourceLabel(value)}</option>{/each}
    </select></label>
    <label class="unmapped"><input type="checkbox" bind:checked={unmappedOnly} /> Unmapped only</label>
  </div>
  <button class="btn btn-primary btn-sm" onclick={() => edit()} disabled={saving}>Add mapping</button>
</div>

{#snippet form(isNew: boolean)}
  <form class="mapping-form" onsubmit={save} aria-label={isNew ? "New import mapping" : `Edit mapping for ${name}`}>
    {#if isNew}
      <div class="form-fields">
        <label class="field">Source<select class="select" bind:value={source} required>
          {#each sources as value}<option value={value}>{sourceLabel(value)}</option>{/each}
        </select></label>
        <label class="field">Source group<input class="input" bind:value={group} maxlength="200" placeholder="Optional" /></label>
        <label class="field">Source category<input class="input" bind:value={name} required maxlength="200" /></label>
      </div>
    {/if}
    <label class="field">Your category<select class="select" bind:value={destination} disabled={saving}>
      <option value="">Leave uncategorised</option>
      {#each options as option (option.id)}<option value={option.id}>{option.label}</option>{/each}
    </select></label>
    <div class="row" style="justify-content:flex-end;gap:8px;margin-top:12px">
      <button class="btn btn-sm" type="button" onclick={() => { editing = null; error = null; }} disabled={saving}>Cancel</button>
      <button class="btn btn-primary btn-sm" type="submit" disabled={saving}>{saving ? "Saving…" : "Save mapping"}</button>
    </div>
  </form>
{/snippet}

{#if error}<div class="error-banner" role="alert" style="margin-bottom:12px">{error}
  {#if editing === null}<button class="btn btn-sm" onclick={load}>Try again</button>{/if}
</div>{/if}
{#if editing === "new"}<section class="card" style="margin-bottom:16px">{@render form(true)}</section>{/if}

{#if loading}
  <div class="empty" role="status">Loading import mappings…</div>
{:else if !error || mappings.length > 0}
  <section class="card mapping-list" aria-label="Import mappings">
    {#if visible.length === 0}
      <div class="empty">{mappings.length === 0 ? "Import transactions to discover source categories, or add a mapping." : "No mappings match these filters."}</div>
    {:else}
      <div class="mapping-head" aria-hidden="true"><span>Source category</span><span>Your category</span><span></span></div>
      {#each visible as mapping (mapping.id)}
        <div class="mapping-row">
          <div class="source-category">
            <span class="small muted">{sourceLabel(mapping.source)}{mapping.source_group ? ` / ${mapping.source_group}` : ""}</span>
            <span>{mapping.source_name}</span>
          </div>
          <span class="destination" class:muted={mapping.category_id === null}><span class="mobile-label muted">Your category</span>{labels.get(mapping.category_id ?? 0) ?? "Unmapped"}</span>
          <button id={`mapping-edit-${mapping.id}`} class="btn btn-sm" aria-label={`Edit mapping for ${mapping.source_name}`} onclick={() => edit(mapping)} disabled={saving}>Edit</button>
        </div>
        {#if editing === mapping.id}<div class="inline-form">{@render form(false)}</div>{/if}
      {/each}
    {/if}
  </section>
{/if}

<style>
  .mapping-help { max-width:65ch; font-size:14px; line-height:1.5; }
  .search { flex:1; min-width:160px; max-width:340px; }
  .unmapped { display:flex; align-items:center; gap:7px; min-height:44px; padding-top:16px; font-size:14px; }
  .form-fields { display:grid; grid-template-columns:repeat(auto-fit,minmax(160px,1fr)); gap:12px; margin-bottom:12px; }
  .mapping-list { padding:0; overflow:hidden; }
  .mapping-row { display:grid; grid-template-columns:minmax(0,1.5fr) minmax(0,1fr) auto; align-items:center; gap:16px; padding:13px 18px; border-bottom:1px solid var(--border); }
  .mapping-row:last-child { border-bottom:0; }
  .mapping-head { display:grid; grid-template-columns:minmax(0,1.5fr) minmax(0,1fr) 44px; gap:16px; padding:12px 18px; border-bottom:1px solid var(--border); font-size:13px; color:var(--text-muted); }
  .mobile-label { display:none; font-size:12px; }
  .source-category { display:flex; flex-direction:column; gap:4px; overflow-wrap:anywhere; }
  .destination { font-size:14px; overflow-wrap:anywhere; }
  .inline-form { padding:16px 18px; background:var(--surface-2); border-bottom:1px solid var(--border); }
  @media (max-width:560px) {
    .mapping-row { grid-template-columns:minmax(0,1fr) auto; gap:8px 12px; padding:14px; }
    .mapping-head { display:none; }
    .mobile-label { display:block; margin-bottom:3px; }
    .source-category { grid-column:1; }
    .destination { grid-column:1; }
    .mapping-row > button { grid-column:2; grid-row:1 / 3; }
    .unmapped { padding-top:0; }
    .inline-form { padding:14px; }
  }
</style>
