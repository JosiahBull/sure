<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatDate, type Schemas } from "../lib/api";

  type Rule = Schemas["Rule"];
  type Category = Schemas["Category"];
  type Merchant = Schemas["Merchant"];

  let rules = $state<Rule[]>([]);
  let categories = $state<Category[]>([]);
  let merchants = $state<Merchant[]>([]);
  let runs = $state<Schemas["RuleRun"][]>([]);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let showAdd = $state(false);
  let preview = $state<Schemas["RulePreview"] | null>(null);

  let form = $state({
    name: "",
    expression: "",
    set_category_id: "" as number | "",
    set_merchant_id: "" as number | "",
    overwrite_manual: false,
    stop_on_match: false,
  });

  const catName = $derived(new Map(categories.map((c) => [c.id, c.name])));
  const merchName = $derived(new Map(merchants.map((m) => [m.id, m.name])));

  async function load() {
    const [r, c, m, ru] = await Promise.all([
      api.GET("/api/rules", {}),
      api.GET("/api/categories", {}),
      api.GET("/api/merchants", {}),
      api.GET("/api/rules/runs", {}),
    ]);
    rules = r.data ?? [];
    categories = c.data ?? [];
    merchants = m.data ?? [];
    runs = ru.data ?? [];
  }
  onMount(load);

  async function doPreview() {
    error = null;
    preview = null;
    const { data, error: e } = await api.POST("/api/rules/preview", {
      body: { expression: form.expression },
    });
    if (e) {
      error = "Invalid expression — check the syntax.";
      return;
    }
    preview = data ?? null;
  }

  async function save() {
    error = null;
    const { error: e } = await api.POST("/api/rules", {
      body: {
        name: form.name,
        expression: form.expression,
        set_category_id: form.set_category_id === "" ? null : form.set_category_id,
        set_merchant_id: form.set_merchant_id === "" ? null : form.set_merchant_id,
        overwrite_manual: form.overwrite_manual,
        stop_on_match: form.stop_on_match,
        priority: 0,
        enabled: true,
      },
    });
    if (e) {
      error = "Failed to save rule (name and a valid expression are required).";
      return;
    }
    form.name = "";
    form.expression = "";
    form.set_category_id = "";
    form.set_merchant_id = "";
    preview = null;
    showAdd = false;
    load();
  }

  async function runRule(id: number) {
    const { data } = await api.POST("/api/rules/{id}/run", { params: { path: { id } } });
    if (data) notice = `Matched ${data.matched}, changed ${data.changed}.`;
    load();
  }
  async function runAll() {
    const { data } = await api.POST("/api/rules/run", {});
    if (data) notice = `Ran all rules — changed ${data.changed}.`;
    load();
  }
  async function del(id: number) {
    await api.DELETE("/api/rules/{id}", { params: { path: { id } } });
    load();
  }
  async function undo(runId: number) {
    await api.POST("/api/rules/runs/{run_id}/undo", { params: { path: { run_id: runId } } });
    notice = "Run undone.";
    load();
  }
</script>

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <h1 style="font-size:20px">Rules</h1>
  <div class="row" style="gap:8px">
    <button class="btn btn-sm" onclick={runAll} disabled={rules.length === 0}>Run all</button>
    <button class="btn btn-primary btn-sm" onclick={() => (showAdd = !showAdd)}>
      {showAdd ? "Close" : "+ New rule"}
    </button>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}
{#if notice}<div class="badge" style="margin-bottom:12px">{notice}</div>{/if}

{#if showAdd}
  <section class="card" style="margin-bottom:14px">
    <div class="grid" style="grid-template-columns:1fr 1fr;gap:12px">
      <label class="field">Name<input class="input" bind:value={form.name} placeholder="Groceries" /></label>
      <label class="field">Set category
        <select class="select" bind:value={form.set_category_id}>
          <option value="">— none —</option>
          {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
        </select>
      </label>
      <label class="field">Set merchant
        <select class="select" bind:value={form.set_merchant_id}>
          <option value="">— none —</option>
          {#each merchants as m}<option value={m.id}>{m.name}</option>{/each}
        </select>
      </label>
    </div>
    <label class="field" style="margin-top:12px">Condition (Zen expression)
      <textarea
        class="mono"
        rows="2"
        bind:value={form.expression}
        placeholder="is_expense and contains(lower(description), 'countdown')"
      ></textarea>
    </label>
    <details class="hint">
      <summary class="small muted">Available fields & examples</summary>
      <div class="small faint" style="margin-top:6px;line-height:1.7">
        <code>amount</code>, <code>amount_minor</code>, <code>is_income</code>, <code>is_expense</code>,
        <code>description</code>, <code>merchant</code>, <code>merchant_id</code>, <code>currency</code>, <code>account</code>,
        <code>account_kind</code>, <code>category_id</code>, <code>is_one_off</code>,
        <code>year</code>, <code>month</code>, <code>day</code>.<br />
        e.g. <code>amount_minor &lt; -100000</code> ·
        <code>account_kind == 'credit_card' and month == 12</code>
      </div>
    </details>
    <div class="row spread wrap" style="margin-top:12px;gap:10px">
      <div class="row" style="gap:14px">
        <label class="switch"><input type="checkbox" bind:checked={form.overwrite_manual} /><span class="track"></span><span>Overwrite manual</span></label>
        <label class="switch"><input type="checkbox" bind:checked={form.stop_on_match} /><span class="track"></span><span>Stop on match</span></label>
      </div>
      <div class="row" style="gap:8px">
        <button class="btn" onclick={doPreview}>Preview</button>
        <button class="btn btn-primary" onclick={save}>Save rule</button>
      </div>
    </div>
    {#if preview}
      <div class="badge" style="margin-top:12px">
        Matches {preview.matched} transaction{preview.matched === 1 ? "" : "s"}
        {#if preview.sample.length}· e.g. {preview.sample.map((s) => s.description || "—").slice(0, 3).join(", ")}{/if}
      </div>
    {/if}
  </section>
{/if}

<section class="card" style="margin-bottom:14px">
  <h2>Active rules</h2>
  {#if rules.length === 0}
    <div class="empty">No rules yet.</div>
  {:else}
    {#each rules as r (r.id)}
      <div class="rule">
        <div class="row spread">
          <div style="min-width:0">
            <strong>{r.name}</strong>
            {#if r.set_category_id}<span class="badge" style="margin-left:6px">→ {catName.get(r.set_category_id) ?? "?"}</span>{/if}
            {#if r.set_merchant_id}<span class="badge" style="margin-left:6px">merchant: {merchName.get(r.set_merchant_id) ?? "?"}</span>{/if}
          </div>
          <div class="row" style="gap:8px">
            <button class="btn btn-sm" onclick={() => runRule(r.id)}>Run</button>
            <button class="btn btn-sm btn-danger" onclick={() => del(r.id)}>✕</button>
          </div>
        </div>
        <code class="small faint">{r.expression}</code>
      </div>
    {/each}
  {/if}
</section>

<section class="card">
  <h2>Audit log</h2>
  {#if runs.length === 0}
    <div class="empty">Runs will appear here.</div>
  {:else}
    <table class="table">
      <thead><tr><th>When</th><th>Kind</th><th>Matched</th><th>Changed</th><th></th></tr></thead>
      <tbody>
        {#each runs.slice(0, 25) as run (run.id)}
          <tr>
            <td class="faint small">{formatDate(run.created_at)}</td>
            <td>{run.kind === "all" ? "All rules" : "Single rule"}</td>
            <td class="tabular">{run.matched}</td>
            <td class="tabular">{run.changed}</td>
            <td style="text-align:right">
              {#if run.undone}
                <span class="badge">undone</span>
              {:else if run.changed > 0}
                <button class="btn btn-sm" onclick={() => undo(run.id)}>Undo</button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<style>
  .rule {
    padding: 11px 0;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .rule:last-child {
    border-bottom: none;
  }
  .hint {
    margin-top: 8px;
  }
  code {
    font-family: var(--mono);
  }
</style>
