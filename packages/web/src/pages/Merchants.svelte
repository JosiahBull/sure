<script lang="ts">
  import { onMount } from "svelte";
  import { api, colorFor, type Schemas } from "../lib/api";

  let merchants = $state<Schemas["Merchant"][]>([]);
  let categories = $state<Schemas["Category"][]>([]);
  let error = $state<string | null>(null);
  let loading = $state(true);

  let showAdd = $state(false);
  let editing = $state<number | null>(null);
  let confirmDelete = $state<number | null>(null);
  let delError = $state<string | null>(null);

  type Form = { name: string; category_id: number | ""; note: string };
  const blank = (): Form => ({ name: "", category_id: "", note: "" });
  let af = $state<Form>(blank());
  let ef = $state<Form>(blank());

  const catById = $derived(new Map(categories.map((c) => [c.id, c])));

  async function load() {
    loading = true;
    const [m, c] = await Promise.all([
      api.GET("/api/merchants", {}),
      api.GET("/api/categories", {}),
    ]);
    merchants = m.data ?? [];
    categories = c.data ?? [];
    loading = false;
  }
  onMount(load);

  function body(f: Form): Schemas["SaveMerchant"] {
    return {
      name: f.name.trim(),
      category_id: f.category_id === "" ? null : Number(f.category_id),
      note: f.note.trim() === "" ? null : f.note.trim(),
    };
  }

  // 409 => the case-insensitive unique name is taken; otherwise fall back to the API's
  // message so validation (422) errors still surface something readable.
  function msgFor(e: unknown, status: number | undefined, fallback: string): string {
    if (status === 409) return "A merchant with that name already exists.";
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  function openAdd() {
    af = blank();
    showAdd = true;
    editing = null;
    confirmDelete = null;
    error = null;
  }

  async function addMerchant() {
    if (!af.name.trim()) return;
    error = null;
    const { error: e, response } = await api.POST("/api/merchants", { body: body(af) });
    if (e) {
      error = msgFor(e, response?.status, "Failed to add merchant.");
      return;
    }
    showAdd = false;
    af = blank();
    load();
  }

  function startEdit(m: Schemas["Merchant"]) {
    editing = m.id;
    ef = { name: m.name, category_id: m.category_id ?? "", note: m.note ?? "" };
    showAdd = false;
    confirmDelete = null;
    error = null;
  }

  async function saveEdit(id: number) {
    if (!ef.name.trim()) return;
    error = null;
    const { error: e, response } = await api.PUT("/api/merchants/{id}", {
      params: { path: { id } },
      body: body(ef),
    });
    if (e) {
      error = msgFor(e, response?.status, "Failed to save merchant.");
      return;
    }
    editing = null;
    load();
  }

  function askDelete(id: number) {
    confirmDelete = id;
    delError = null;
    editing = null;
  }
  async function del(id: number) {
    delError = null;
    const { error: e } = await api.DELETE("/api/merchants/{id}", { params: { path: { id } } });
    if (e) {
      delError = (e as { error?: { message?: string } }).error?.message ?? "Couldn't delete this merchant.";
      return;
    }
    confirmDelete = null;
    load();
  }
</script>

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <h1 style="font-size:20px">Merchants</h1>
  <button class="btn btn-primary btn-sm" onclick={() => (showAdd ? (showAdd = false) : openAdd())}>
    {showAdd ? "Close" : "+ New merchant"}
  </button>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if showAdd}
  <section class="card" style="margin-bottom:14px">
    <h2>New merchant</h2>
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px">
      <label class="field">Name
        <input class="input" bind:value={af.name} placeholder="e.g. Netflix" />
      </label>
      <label class="field">Default category
        <select class="select" bind:value={af.category_id}>
          <option value="">— none —</option>
          {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
        </select>
      </label>
      <label class="field">Note
        <input class="input" bind:value={af.note} placeholder="optional" />
      </label>
    </div>
    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:14px">
      <button class="btn btn-sm" onclick={() => (showAdd = false)}>Cancel</button>
      <button class="btn btn-primary btn-sm" onclick={addMerchant} disabled={!af.name.trim()}>Add</button>
    </div>
  </section>
{/if}

{#if loading}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  <section class="card">
    <div class="card-title">
      <h2 style="margin-bottom:0">Merchants · {merchants.length}</h2>
    </div>
    {#if merchants.length === 0}
      <div class="empty">No merchants yet — add one to categorise transactions automatically.</div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Merchant</th>
            <th style="text-align:right">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each merchants as m (m.id)}
            {@const cat = m.category_id != null ? catById.get(m.category_id) : null}
            <tr>
              <td>
                <div class="row" style="gap:10px;min-width:0">
                  <span class="avatar" style="background:{colorFor(m.id)}">{m.name.charAt(0).toUpperCase()}</span>
                  <div class="col" style="min-width:0;gap:2px">
                    <div class="row" style="gap:6px;min-width:0">
                      <span class="ell">{m.name}</span>
                      {#if cat}<span class="badge">{cat.name}</span>{/if}
                    </div>
                    {#if m.note}<span class="small faint ell">{m.note}</span>{/if}
                  </div>
                </div>
              </td>
              <td>
                <div class="row" style="gap:6px;justify-content:flex-end">
                  <button class="btn btn-sm" onclick={() => (editing === m.id ? (editing = null) : startEdit(m))}>
                    {editing === m.id ? "Close" : "Edit"}
                  </button>
                  <button class="btn btn-sm btn-danger" aria-label="Delete {m.name}" onclick={() => askDelete(m.id)}>✕</button>
                </div>
              </td>
            </tr>
            {#if confirmDelete === m.id}
              <tr>
                <td colspan="2" style="border-bottom:none">
                  <div class="confirm">
                    <div class="small">Delete <strong>{m.name}</strong>? This can't be undone.</div>
                    {#if delError}<div class="error-banner" style="margin-top:8px">{delError}</div>{/if}
                    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
                      <button class="btn btn-sm" onclick={() => (confirmDelete = null)}>Cancel</button>
                      <button class="btn btn-sm btn-danger" onclick={() => del(m.id)}>Delete</button>
                    </div>
                  </div>
                </td>
              </tr>
            {/if}
            {#if editing === m.id}
              <tr>
                <td colspan="2" style="border-bottom:none">
                  <div class="edit-panel">
                    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px">
                      <label class="field">Name
                        <input class="input" bind:value={ef.name} />
                      </label>
                      <label class="field">Default category
                        <select class="select" bind:value={ef.category_id}>
                          <option value="">— none —</option>
                          {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
                        </select>
                      </label>
                      <label class="field">Note
                        <input class="input" bind:value={ef.note} placeholder="optional" />
                      </label>
                    </div>
                    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:12px">
                      <button class="btn btn-sm" onclick={() => (editing = null)}>Cancel</button>
                      <button class="btn btn-primary btn-sm" onclick={() => saveEdit(m.id)} disabled={!ef.name.trim()}>Save</button>
                    </div>
                  </div>
                </td>
              </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {/if}
  </section>
{/if}

<style>
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
    flex: 0 0 auto;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 13px;
    font-weight: 650;
    color: #fff;
  }
  .confirm {
    padding: 12px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
  .edit-panel {
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
</style>
