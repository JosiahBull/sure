<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../lib/api";
  import { people, refresh, personColor, initials, placeholders, type Person } from "../lib/people.svelte";
  import IncomeSection from "./household/IncomeSection.svelte";
  import PaymentsPanel from "./household/PaymentsPanel.svelte";

  let error = $state<string | null>(null);
  let loading = $state(true);
  let showAdd = $state(false);
  let editing = $state<number | null>(null);
  let confirmDelete = $state<number | null>(null);
  let delError = $state<string | null>(null);

  // A small fixed palette rather than a colour picker: these show up as chart series and
  // badges next to money, so they need to stay legible on both themes.
  const SWATCHES = ["#7c5cff", "#12b981", "#f59e0b", "#ef4444", "#0ea5e9", "#ec4899"];

  type Form = { name: string; color: string };
  const blank = (): Form => ({ name: "", color: SWATCHES[people.list.length % SWATCHES.length] });
  let af = $state<Form>(blank());
  let ef = $state<Form>(blank());

  async function load() {
    loading = true;
    await refresh();
    error = people.error;
    loading = false;
  }
  onMount(load);

  function msgFor(e: unknown, status: number | undefined, fallback: string): string {
    if (status === 409) return "Someone in the household already has that name.";
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  function openAdd() {
    af = blank();
    showAdd = true;
    editing = null;
    confirmDelete = null;
    error = null;
  }

  async function addPerson() {
    if (!af.name.trim()) return;
    error = null;
    const { error: e, response } = await api.POST("/api/people", {
      body: { name: af.name.trim(), color: af.color, sort_order: people.list.length },
    });
    if (e) {
      error = msgFor(e, response?.status, "Failed to add this person.");
      return;
    }
    showAdd = false;
    load();
  }

  function startEdit(p: Person) {
    editing = p.id;
    ef = { name: p.name, color: p.color ?? personColor(p) };
    showAdd = false;
    confirmDelete = null;
    error = null;
  }

  async function saveEdit(p: Person) {
    if (!ef.name.trim()) return;
    error = null;
    const { error: e, response } = await api.PUT("/api/people/{id}", {
      params: { path: { id: p.id } },
      body: { name: ef.name.trim(), color: ef.color, sort_order: p.sort_order },
    });
    if (e) {
      error = msgFor(e, response?.status, "Failed to save this person.");
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
    const { error: e } = await api.DELETE("/api/people/{id}", { params: { path: { id } } });
    if (e) {
      // 409 while accounts are still attributed to them — the message names those accounts.
      delError =
        (e as { error?: { message?: string } }).error?.message ?? "Couldn't remove this person.";
      return;
    }
    confirmDelete = null;
    load();
  }
</script>

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <div>
    <h1 style="font-size:20px">Household</h1>
    <div class="muted small">
      Who shares these finances, what each of them earns, and how their pay is recognised when it
      lands.
    </div>
  </div>
  <button class="btn btn-primary btn-sm" onclick={() => (showAdd ? (showAdd = false) : openAdd())}>
    {showAdd ? "Close" : "+ Add person"}
  </button>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if placeholders().length > 0}
  <div class="placeholder-banner small">
    Accounts have to belong to someone, so accounts that predate this feature were given to a
    stand-in rather than guessed at. Rename it below if they're all one person's, or re-attribute
    them from the Accounts page.
  </div>
{/if}

{#if showAdd}
  <section class="card" style="margin-bottom:14px">
    <h2>Add someone</h2>
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px">
      <label class="field"
        >Name
        <input class="input" bind:value={af.name} placeholder="e.g. Sam" />
      </label>
      <div class="field">
        <span>Colour</span>
        <div class="swatches">
          {#each SWATCHES as c}
            <button
              class="swatch"
              class:selected={af.color === c}
              style="background:{c}"
              aria-label="Use colour {c}"
              aria-pressed={af.color === c}
              onclick={() => (af.color = c)}
            ></button>
          {/each}
        </div>
      </div>
    </div>
    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:14px">
      <button class="btn btn-sm" onclick={() => (showAdd = false)}>Cancel</button>
      <button class="btn btn-primary btn-sm" onclick={addPerson} disabled={!af.name.trim()}>
        Add
      </button>
    </div>
  </section>
{/if}

{#if loading}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  <section class="card">
    <div class="card-title">
      <h2 style="margin-bottom:0">People · {people.list.length}</h2>
    </div>
    {#if people.list.length === 0}
      <div class="empty">Nobody yet — add the two of you to start attributing accounts.</div>
    {:else}
      <table class="table">
        <thead>
          <tr>
            <th>Person</th>
            <th style="text-align:right">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each people.list as p (p.id)}
            <tr>
              <td>
                <div class="row" style="gap:10px;min-width:0">
                  <span class="avatar" style="background:{personColor(p)}">{initials(p.name)}</span>
                  <span class="ell">{p.name}</span>
                  {#if p.placeholder}<span class="badge">Placeholder</span>{/if}
                </div>
              </td>
              <td>
                <div class="row" style="gap:6px;justify-content:flex-end">
                  <button
                    class="btn btn-sm"
                    onclick={() => (editing === p.id ? (editing = null) : startEdit(p))}
                  >
                    {editing === p.id ? "Close" : "Edit"}
                  </button>
                  <button
                    class="btn btn-sm btn-danger"
                    aria-label="Remove {p.name}"
                    onclick={() => askDelete(p.id)}>✕</button
                  >
                </div>
              </td>
            </tr>
            {#if confirmDelete === p.id}
              <tr>
                <td colspan="2" style="border-bottom:none">
                  <div class="confirm">
                    <div class="small">
                      Remove <strong>{p.name}</strong> from the household? Accounts attributed to
                      them have to be re-attributed first.
                    </div>
                    {#if delError}
                      <div class="error-banner" style="margin-top:8px">{delError}</div>
                    {/if}
                    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:10px">
                      <button class="btn btn-sm" onclick={() => (confirmDelete = null)}>
                        Cancel
                      </button>
                      <button class="btn btn-sm btn-danger" onclick={() => del(p.id)}>Remove</button>
                    </div>
                  </div>
                </td>
              </tr>
            {/if}
            {#if editing === p.id}
              <tr>
                <td colspan="2" style="border-bottom:none">
                  <div class="edit-panel">
                    <div
                      class="grid"
                      style="grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px"
                    >
                      <label class="field"
                        >Name
                        <input class="input" bind:value={ef.name} />
                      </label>
                      <div class="field">
                        <span>Colour</span>
                        <div class="swatches">
                          {#each SWATCHES as c}
                            <button
                              class="swatch"
                              class:selected={ef.color === c}
                              style="background:{c}"
                              aria-label="Use colour {c}"
                              aria-pressed={ef.color === c}
                              onclick={() => (ef.color = c)}
                            ></button>
                          {/each}
                        </div>
                      </div>
                    </div>
                    <div class="row" style="gap:8px;justify-content:flex-end;margin-top:12px">
                      <button class="btn btn-sm" onclick={() => (editing = null)}>Cancel</button>
                      <button
                        class="btn btn-primary btn-sm"
                        onclick={() => saveEdit(p)}
                        disabled={!ef.name.trim()}
                      >
                        Save
                      </button>
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

  {#if people.list.length > 0}
    <IncomeSection />
    <PaymentsPanel />
  {/if}
{/if}

<style>
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .placeholder-banner {
    margin-bottom: 14px;
    padding: 12px 14px;
    border: 1px solid color-mix(in srgb, var(--accent) 30%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--accent) 6%, transparent);
    color: var(--text-muted);
  }
  .avatar {
    flex: 0 0 auto;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
    font-weight: 650;
    color: #fff;
  }
  .swatches {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .swatch {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    border: 2px solid transparent;
    cursor: pointer;
    padding: 0;
  }
  .swatch.selected {
    border-color: var(--text);
  }
  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
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
