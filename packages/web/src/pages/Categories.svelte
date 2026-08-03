<script lang="ts">
  import { onMount } from "svelte";
  import { api, type Schemas } from "../lib/api";
  import { categoryColor } from "../lib/color";
  import { categoryOptions, MAX_CATEGORY_DEPTH, rootIdOf, subtreeHeight, subtreeIds } from "../lib/categories";
  import { resolvedTheme } from "../lib/theme.svelte";

  type Category = Schemas["Category"];
  type Node = Schemas["CategoryNode"];

  let tree = $state<Node[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);

  let adding = $state(false);
  let editingId = $state<number | null>(null);
  let confirmDelete = $state<number | null>(null);
  let delError = $state<string | null>(null);
  let busy = $state(false);

  type CategoryKind = Schemas["CategoryKind"];

  // Add/edit form fields (one form open at a time, so a single set of state is fine).
  let fName = $state("");
  let fKind = $state<CategoryKind>("expense");
  let fParent = $state<number | "">("");
  let fColor = $state("");
  let fIcon = $state("");

  const KINDS: { value: CategoryKind; label: string }[] = [
    { value: "income", label: "Income" },
    { value: "expense", label: "Expense" },
    { value: "transfer", label: "Transfer" },
  ];

  type Row = { cat: Category; depth: number };
  function flatten(nodes: Node[], depth = 0, out: Row[] = []): Row[] {
    for (const n of nodes) {
      out.push({ cat: n.category, depth });
      flatten(n.children, depth + 1, out);
    }
    return out;
  }
  const rows = $derived(flatten(tree));
  /** The same categories as a flat list, for the ancestor walks in `../lib/categories`. */
  const flatCategories = $derived(rows.map((r) => r.cat));

  /**
   * The categories that could legally be this one's parent. Offering the rest would just
   * hand the user a 422 from `sure_dal::categories::validate`: nesting is capped at
   * MAX_CATEGORY_DEPTH levels, and re-parenting drags the whole subtree along, so what fits
   * depends on how *tall* the category being edited is, not only on where it would land.
   * Its own descendants are excluded as well, since nesting under one is a cycle.
   */
  const descendants = $derived(editingId === null ? new Set<number>() : subtreeIds(flatCategories, editingId));
  const parentOptions = $derived.by(() => {
    const height = editingId === null ? 0 : subtreeHeight(flatCategories, editingId);
    return categoryOptions(flatCategories, { exclude: descendants }).filter(
      (o) => o.depth + 1 + height <= MAX_CATEGORY_DEPTH - 1,
    );
  });

  async function load() {
    loading = true;
    const { data, error: e } = await api.GET("/api/categories/tree", {});
    if (e) error = msgOf(e);
    tree = data ?? [];
    loading = false;
  }
  onMount(load);

  // A category's colour is its own if the user set one, else its *top-level* ancestor's
  // family shade, deepened by how far down the branch it sits — the same rule the money-flow
  // chart uses, so the two agree. Keying off `parent_id` instead would only reach one level
  // up, so a grandchild would start its own family rather than joining its grandparent's.
  const dark = $derived(resolvedTheme() === "dark");
  function colorOf(cat: Category, depth: number): string {
    return cat.color ?? categoryColor({ rootId: rootIdOf(flatCategories, cat.id), depth, dark });
  }

  function msgOf(e: unknown): string {
    return (e as { error?: { message?: string } })?.error?.message ?? "Something went wrong.";
  }

  function resetFields() {
    fName = "";
    fKind = "expense";
    fParent = "";
    fColor = "";
    fIcon = "";
    error = null;
  }
  function openAdd() {
    resetFields();
    editingId = null;
    confirmDelete = null;
    adding = true;
  }
  function openEdit(cat: Category) {
    fName = cat.name;
    fKind = cat.kind;
    fParent = cat.parent_id ?? "";
    fColor = cat.color ?? "";
    fIcon = cat.icon ?? "";
    error = null;
    adding = false;
    confirmDelete = null;
    editingId = cat.id;
  }
  function closeForm() {
    adding = false;
    editingId = null;
    error = null;
  }

  async function save() {
    if (!fName.trim()) {
      error = "Name is required.";
      return;
    }
    busy = true;
    error = null;
    const body: Schemas["SaveCategory"] = {
      name: fName.trim(),
      kind: fKind,
      parent_id: fParent === "" ? null : Number(fParent),
      color: fColor.trim() || null,
      icon: fIcon.trim() || null,
    };
    if (editingId !== null) {
      const { error: e } = await api.PUT("/api/categories/{id}", {
        params: { path: { id: editingId } },
        body,
      });
      if (e) {
        error = msgOf(e);
        busy = false;
        return;
      }
    } else {
      const { error: e } = await api.POST("/api/categories", { body });
      if (e) {
        error = msgOf(e);
        busy = false;
        return;
      }
    }
    busy = false;
    closeForm();
    load();
  }

  function askDelete(id: number) {
    confirmDelete = id;
    delError = null;
    adding = false;
    editingId = null;
  }
  function cancelDelete() {
    confirmDelete = null;
    delError = null;
  }
  async function del(id: number) {
    delError = null;
    const { error: e } = await api.DELETE("/api/categories/{id}", { params: { path: { id } } });
    if (e) {
      delError = msgOf(e);
      return;
    }
    confirmDelete = null;
    load();
  }
</script>

{#snippet formCard(title: string)}
  <section class="card" style="margin-bottom:14px">
    <h2 style="margin-bottom:4px">{title}</h2>
    {#if error}<div class="error-banner" style="margin:8px 0">{error}</div>{/if}
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-top:8px">
      <label class="field">Name<input class="input" bind:value={fName} /></label>
      <label class="field">Kind
        <select class="select" bind:value={fKind}>
          {#each KINDS as k}<option value={k.value}>{k.label}</option>{/each}
        </select>
      </label>
      <label class="field">Parent
        <select class="select" bind:value={fParent}>
          <option value="">No parent</option>
          {#each parentOptions as o (o.id)}
            <option value={o.id}>{o.label}</option>
          {/each}
        </select>
      </label>
      <label class="field">Colour<input class="input" bind:value={fColor} placeholder="#e99537 (auto)" /></label>
      <label class="field">Icon<input class="input" bind:value={fIcon} placeholder="optional" /></label>
    </div>
    <div class="row" style="justify-content:flex-end;gap:8px;margin-top:14px">
      <button class="btn" onclick={closeForm} disabled={busy}>Cancel</button>
      <button class="btn btn-primary" onclick={save} disabled={busy}>
        {editingId !== null ? "Save" : "Create"}
      </button>
    </div>
  </section>
{/snippet}

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <h1 style="font-size:20px">Categories</h1>
  <button class="btn btn-primary btn-sm" onclick={openAdd}>+ New category</button>
</div>

{#if adding}
  {@render formCard("New category")}
{/if}

{#if loading}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  <section class="card" style="padding:0">
    <div class="list-head">Categories · {rows.length}</div>
    {#if rows.length === 0}
      <div class="empty">No categories yet.</div>
    {:else}
      {#each rows as { cat, depth } (cat.id)}
        {@const c = colorOf(cat, depth)}
        <div class="cat-row">
          <div class="cat-main" style="padding-left:{depth * 22}px">
            {#if depth > 0}<span class="arrow" style="color:{c}">↳</span>{/if}
            <span class="pill" style="--c:{c}">
              {#if cat.icon}<span class="pill-icon">{cat.icon}</span>{/if}
              <span class="ell">{cat.name}</span>
            </span>
          </div>
          <div class="row" style="gap:6px;flex:0 0 auto">
            <button class="btn btn-sm" onclick={() => openEdit(cat)}>Edit</button>
            <button class="btn btn-sm btn-danger" aria-label="Delete {cat.name}" onclick={() => askDelete(cat.id)}>✕</button>
          </div>
        </div>
        {#if confirmDelete === cat.id}
          <div class="confirm">
            <div class="small">Delete <strong>{cat.name}</strong> and its sub-categories? This can't be undone.</div>
            {#if delError}<div class="error-banner" style="margin-top:8px">{delError}</div>{/if}
            <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
              <button class="btn btn-sm" onclick={cancelDelete}>Cancel</button>
              <button class="btn btn-sm btn-danger" onclick={() => del(cat.id)}>Delete</button>
            </div>
          </div>
        {/if}
        {#if editingId === cat.id}
          {@render formCard(`Edit ${cat.name}`)}
        {/if}
      {/each}
    {/if}
  </section>
{/if}

<style>
  .list-head {
    padding: 12px 18px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-faint);
    border-bottom: 1px solid var(--border);
  }
  .cat-row {
    display: flex;
    align-items: center;
    gap: 8px 10px;
    padding: 11px 18px;
    border-bottom: 1px solid var(--border);
  }
  .cat-row:last-child {
    border-bottom: none;
  }
  .cat-main {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    flex: 1 1 auto;
  }
  .arrow {
    flex: 0 0 auto;
    opacity: 0.7;
    font-weight: 600;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 100%;
    padding: 4px 12px;
    border-radius: 999px;
    font-size: 14px;
    font-weight: 550;
    color: var(--c);
    background: color-mix(in srgb, var(--c) 16%, transparent);
  }
  .pill-icon {
    flex: 0 0 auto;
    line-height: 1;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .confirm {
    margin: 2px 18px 12px;
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
</style>
