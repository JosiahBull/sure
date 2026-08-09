<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatDate, formatMoney, type Schemas } from "../lib/api";
  import { categoryOptions, qualifiedName } from "../lib/categories";
  import RuleGroup from "../lib/rules/RuleGroup.svelte";
  import {
    emit,
    parse,
    humanize,
    emptyRoot,
    type Group,
    type BuilderRefs,
  } from "../lib/rules/expr";

  type Rule = Schemas["Rule"];
  type Category = Schemas["Category"];
  type Merchant = Schemas["Merchant"];
  type Account = Schemas["Account"];
  type SaveRule = Schemas["SaveRule"];

  // Full AccountKind enum — fallback for the "Account type" field before any accounts exist.
  const ACCOUNT_KINDS = [
    "cash", "bank", "savings", "credit_card", "revolving_credit", "mortgage",
    "student_loan", "loan", "vehicle", "real_estate", "shares_nz", "shares_us",
    "shares_private", "brokerage", "crypto", "asset", "liability",
  ];

  let rules = $state<Rule[]>([]);
  let categories = $state<Category[]>([]);
  let merchants = $state<Merchant[]>([]);
  let accounts = $state<Account[]>([]);
  let runs = $state<Schemas["RuleRun"][]>([]);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  // Editor state.
  let showForm = $state(false);
  let editId = $state<number | null>(null);
  let root = $state<Group>(emptyRoot());
  // Original expression of a rule the visual builder can't represent (e.g. authored via
  // the API). Kept so editing its actions doesn't discard the conditions.
  let unparsed = $state<string | null>(null);
  let form = $state({
    name: "",
    set_category_id: "" as number | "",
    set_merchant_id: "" as number | "",
    set_one_off: "" as "" | "true" | "false",
    overwrite_manual: false,
    stop_on_match: false,
    enabled: true,
    priority: 0,
  });

  // Live preview of what the current expression matches.
  let preview = $state<Schemas["RulePreview"] | null>(null);
  let previewError = $state(false);

  // Audit-log expandable diff: which run is open, and a lazy cache of its changes.
  type AppDetail = Schemas["RuleApplicationDetail"];
  let openRun = $state<number | null>(null);
  let runDetails = $state<Record<number, AppDetail[]>>({});

  const merchName = $derived(new Map(merchants.map((m) => [m.id, m.name])));
  const ruleName = $derived(new Map(rules.map((r) => [r.id, r.name])));

  const refs = $derived<BuilderRefs>({
    categories: categories.map((c) => ({ id: c.id, name: c.name })),
    merchants: merchants.map((m) => ({ id: m.id, name: m.name })),
    accounts: accounts.map((a) => ({ id: a.id, name: a.name })),
    accountKinds: uniq(accounts.map((a) => a.kind), ACCOUNT_KINDS),
    currencies: uniq(accounts.map((a) => a.currency_code), ["NZD"]),
  });

  const builtExpr = $derived(emit(root));
  // What we preview and save: the built tree, falling back to a preserved original when
  // the tree is empty (an unrepresentable rule left untouched).
  const effectiveExpr = $derived(builtExpr || unparsed || "");

  function uniq(xs: string[], fallback: string[]): string[] {
    const s = [...new Set(xs)];
    return s.length ? s : fallback;
  }

  async function load() {
    const [r, c, m, a, ru] = await Promise.all([
      api.GET("/api/rules", {}),
      api.GET("/api/categories", {}),
      api.GET("/api/merchants", {}),
      api.GET("/api/accounts", {}),
      api.GET("/api/rules/runs", {}),
    ]);
    rules = r.data ?? [];
    categories = c.data ?? [];
    merchants = m.data ?? [];
    accounts = a.data ?? [];
    runs = ru.data ?? [];
    runDetails = {}; // invalidate cached diffs; the open run refetches via the effect below
  }
  onMount(load);

  // Lazily load (and cache) the per-transaction changes for whichever run is expanded.
  $effect(() => {
    const id = openRun;
    if (id == null || runDetails[id]) return;
    (async () => {
      const { data } = await api.GET("/api/rules/runs/{run_id}", { params: { path: { run_id: id } } });
      if (openRun === id) runDetails = { ...runDetails, [id]: data ?? [] };
    })();
  });

  // Debounced preview whenever the expression changes while the editor is open.
  $effect(() => {
    const expr = effectiveExpr;
    if (!showForm || !expr) {
      preview = null;
      previewError = false;
      return;
    }
    const handle = setTimeout(async () => {
      const { data, error: e } = await api.POST("/api/rules/preview", { body: { expression: expr } });
      if (expr !== effectiveExpr) return; // superseded while in flight
      previewError = !!e;
      preview = e ? null : (data ?? null);
    }, 300);
    return () => clearTimeout(handle);
  });

  function openCreate() {
    editId = null;
    form = {
      name: "", set_category_id: "", set_merchant_id: "", set_one_off: "",
      overwrite_manual: false, stop_on_match: false, enabled: true, priority: 0,
    };
    root = emptyRoot();
    unparsed = null;
    preview = null;
    error = null;
    showForm = true;
  }

  function openEdit(r: Rule) {
    editId = r.id;
    form = {
      name: r.name,
      set_category_id: r.set_category_id ?? "",
      set_merchant_id: r.set_merchant_id ?? "",
      set_one_off: r.set_one_off == null ? "" : r.set_one_off ? "true" : "false",
      overwrite_manual: r.overwrite_manual,
      stop_on_match: r.stop_on_match,
      enabled: r.enabled,
      priority: r.priority,
    };
    const parsed = parse(r.expression);
    if (parsed) {
      root = parsed;
      unparsed = null;
    } else {
      root = emptyRoot();
      unparsed = r.expression;
    }
    preview = null;
    error = null;
    showForm = true;
  }

  const toSaveBody = (r: Rule): SaveRule => ({
    name: r.name,
    description: r.description ?? null,
    expression: r.expression,
    set_category_id: r.set_category_id ?? null,
    set_merchant_id: r.set_merchant_id ?? null,
    set_one_off: r.set_one_off ?? null,
    overwrite_manual: r.overwrite_manual,
    stop_on_match: r.stop_on_match,
    priority: r.priority,
    enabled: r.enabled,
  });

  async function save() {
    error = null;
    const expression = effectiveExpr;
    if (!form.name.trim()) {
      error = "Give the rule a name.";
      return;
    }
    if (!expression) {
      error = "Add at least one condition.";
      return;
    }
    const body: SaveRule = {
      name: form.name.trim(),
      expression,
      set_category_id: form.set_category_id === "" ? null : Number(form.set_category_id),
      set_merchant_id: form.set_merchant_id === "" ? null : Number(form.set_merchant_id),
      set_one_off: form.set_one_off === "" ? null : form.set_one_off === "true",
      overwrite_manual: form.overwrite_manual,
      stop_on_match: form.stop_on_match,
      priority: form.priority,
      enabled: form.enabled,
    };
    const { error: e } =
      editId == null
        ? await api.POST("/api/rules", { body })
        : await api.PUT("/api/rules/{id}", { params: { path: { id: editId } }, body });
    if (e) {
      error = "Couldn't save — the name and a valid set of conditions are required.";
      return;
    }
    showForm = false;
    load();
  }

  async function toggleEnabled(r: Rule) {
    await api.PUT("/api/rules/{id}", {
      params: { path: { id: r.id } },
      body: { ...toSaveBody(r), enabled: !r.enabled },
    });
    load();
  }

  function summaryOf(r: Rule): string {
    const p = parse(r.expression);
    return p ? humanize(p, refs) : "Custom conditions";
  }

  const toggleRun = (id: number) => (openRun = openRun === id ? null : id);
  // Qualified with its ancestors — at three levels a bare "Power" doesn't say which branch.
  const catLabel = (id: number | null | undefined) =>
    id == null ? "none" : (qualifiedName(categories, id) || `#${id}`);
  const merchLabel = (id: number | null | undefined) =>
    id == null ? "none" : (merchName.get(id) ?? `#${id}`);

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
    if (editId === id) showForm = false;
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
    <button class="btn btn-primary btn-sm" onclick={showForm ? () => (showForm = false) : openCreate}>
      {showForm ? "Close" : "+ New rule"}
    </button>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}
{#if notice}<div class="badge" style="margin-bottom:12px">{notice}</div>{/if}

{#if showForm}
  <section class="card" style="margin-bottom:14px">
    <div class="row spread wrap" style="gap:10px;margin-bottom:14px">
      <h2>{editId == null ? "New rule" : "Edit rule"}</h2>
      <div class="row" style="gap:8px">
        <button class="btn btn-sm" onclick={() => (showForm = false)}>Cancel</button>
        <button class="btn btn-primary btn-sm" onclick={save}>{editId == null ? "Create rule" : "Save changes"}</button>
      </div>
    </div>

    <label class="field" style="margin-bottom:14px">
      Name
      <input class="input" bind:value={form.name} placeholder="Groceries" />
    </label>

    <div class="section-label">
      <span>When a transaction matches</span>
    </div>

    {#if unparsed}
      <div class="notice">
        These conditions were set up outside the visual builder, so they're not shown here.
        They'll be kept as-is — add conditions below to replace them.
      </div>
    {/if}
    <RuleGroup group={root} {refs} />

    <div class="preview-line">
      {#if previewError}
        <span class="neg small">Incomplete or invalid expression.</span>
      {:else if preview}
        <span class="badge">Matches {preview.matched} transaction{preview.matched === 1 ? "" : "s"}</span>
        {#if preview.sample.length}
          <span class="faint small">e.g. {preview.sample.slice(0, 3).map((s) => s.description || "—").join(", ")}</span>
        {/if}
      {:else if effectiveExpr}
        <span class="faint small">Previewing…</span>
      {:else}
        <span class="faint small">Add a condition to preview matches.</span>
      {/if}
    </div>

    <div class="section-label">Then apply</div>
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(160px,1fr))">
      <label class="field">Set category
        <select class="select" bind:value={form.set_category_id}>
          <option value="">— leave as is —</option>
          {#each categoryOptions(categories) as o}<option value={o.id}>{o.label}</option>{/each}
        </select>
      </label>
      <label class="field">Set merchant
        <select class="select" bind:value={form.set_merchant_id}>
          <option value="">— leave as is —</option>
          {#each merchants as m}<option value={m.id}>{m.name}</option>{/each}
        </select>
      </label>
      <label class="field">One-off
        <select class="select" bind:value={form.set_one_off}>
          <option value="">— leave as is —</option>
          <option value="true">Mark as one-off</option>
          <option value="false">Clear one-off</option>
        </select>
      </label>
    </div>

    <div class="row wrap" style="gap:16px;margin-top:14px">
      <label class="switch"><input type="checkbox" bind:checked={form.overwrite_manual} /><span class="track"></span><span>Overwrite manual</span></label>
      <label class="switch"><input type="checkbox" bind:checked={form.stop_on_match} /><span class="track"></span><span>Stop on match</span></label>
      <label class="switch"><input type="checkbox" bind:checked={form.enabled} /><span class="track"></span><span>Enabled</span></label>
    </div>
  </section>
{/if}

<section class="card" style="margin-bottom:14px">
  <h2>Active rules</h2>
  {#if rules.length === 0}
    <div class="empty">No rules yet.</div>
  {:else}
    {#each rules as r (r.id)}
      <div class="rule" class:off={!r.enabled}>
        <div class="rule-head">
          <div class="rule-name">
            <strong>{r.name}</strong>
            {#if r.set_category_id}<span class="badge" style="margin-left:6px">→ {catLabel(r.set_category_id)}</span>{/if}
            {#if r.set_merchant_id}<span class="badge" style="margin-left:6px">merchant: {merchName.get(r.set_merchant_id) ?? "?"}</span>{/if}
            {#if r.set_one_off != null}<span class="badge" style="margin-left:6px">{r.set_one_off ? "mark one-off" : "clear one-off"}</span>{/if}
            {#if !r.enabled}<span class="badge" style="margin-left:6px">disabled</span>{/if}
          </div>
          <div class="row rule-actions" style="gap:6px">
            <label class="switch" title="Enabled"><input type="checkbox" checked={r.enabled} onchange={() => toggleEnabled(r)} /><span class="track"></span></label>
            <button class="btn btn-sm" onclick={() => openEdit(r)}>Edit</button>
            <button class="btn btn-sm" onclick={() => runRule(r.id)}>Run</button>
            <button class="btn btn-sm btn-danger" onclick={() => del(r.id)} title="Delete" aria-label="Delete rule">✕</button>
          </div>
        </div>
        <div class="summary muted small">{summaryOf(r)}</div>
      </div>
    {/each}
  {/if}
</section>

<section class="card">
  <h2>Audit log</h2>
  {#if runs.length === 0}
    <div class="empty">Runs will appear here.</div>
  {:else}
    <div style="overflow-x:auto">
    <table class="table">
      <thead><tr><th>When</th><th>Rule</th><th>Matched</th><th>Changed</th><th></th></tr></thead>
      <tbody>
        {#each runs.slice(0, 25) as run (run.id)}
          {@const expandable = run.changed > 0}
          <!-- Whole row toggles the diff; the caret button is the keyboard-accessible control. -->
          <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
          <tr
            class:has-detail={openRun === run.id}
            class:clickable={expandable}
            onclick={() => expandable && toggleRun(run.id)}
          >
            <td class="faint small">
              {#if expandable}
                <button
                  class="caret-btn"
                  class:open={openRun === run.id}
                  onclick={(e) => { e.stopPropagation(); toggleRun(run.id); }}
                  aria-expanded={openRun === run.id}
                  aria-label="Show changes"
                >▸</button>
              {/if}
              <!-- Wrapped so the visual suite can mask it: `created_at` is stamped by the
                   database's own clock, so it is the one thing on this page that can't be
                   pinned to a fixed date. -->
              <span class="run-when">{formatDate(run.created_at)}</span>
            </td>
            <td>
              <!-- An automatic run has no rule_id (it evaluates the whole enabled set), so it
                   has to be named before the rule_id branch below — otherwise it falls through
                   to "Deleted rule", which is what a run of a since-deleted single rule is. -->
              {#if run.kind === "all"}All rules
              {:else if run.kind === "auto"}Automatic, after new transactions
              {:else if run.rule_id != null}{ruleName.get(run.rule_id) ?? `Rule #${run.rule_id}`}
              {:else}Deleted rule{/if}
            </td>
            <td class="tabular">{run.matched}</td>
            <td class="tabular">{run.changed}</td>
            <td style="text-align:right">
              {#if run.undone}
                <span class="badge">undone</span>
              {:else if run.changed > 0}
                <button class="btn btn-sm" onclick={(e) => { e.stopPropagation(); undo(run.id); }}>Undo</button>
              {/if}
            </td>
          </tr>
          {#if openRun === run.id}
            <tr class="detail-row">
              <td colspan="5">
                {#if !runDetails[run.id]}
                  <div class="row" style="justify-content:center;padding:10px"><span class="spinner"></span></div>
                {:else if runDetails[run.id].length === 0}
                  <div class="faint small" style="padding:4px 2px">No recorded changes.</div>
                {:else}
                  <div class="diff">
                    {#each runDetails[run.id] as a (a.id)}
                      <div class="txn-change" class:reverted={a.reverted}>
                        <a class="txn-line" href={`#/transactions?tx=${a.transaction_id}`} title="View this transaction">
                          <span class="faint small" style="white-space:nowrap">{formatDate(a.posted_at)}</span>
                          <span class="txn-desc">{a.description || "—"}</span>
                          <span class="go" aria-hidden="true">↗</span>
                          <span
                            class="tabular small"
                            class:pos={a.amount_minor >= 0}
                            class:neg={a.amount_minor < 0}
                            style="white-space:nowrap"
                          >{formatMoney(a.amount_minor, a.currency_code)}</span>
                        </a>
                        <div class="txn-diffs">
                          {#if a.prev_category_id !== a.new_category_id}
                            <span class="txn-diff">Category <b>{catLabel(a.prev_category_id)}</b> → <b>{catLabel(a.new_category_id)}</b></span>
                          {/if}
                          {#if a.prev_merchant_id !== a.new_merchant_id}
                            <span class="txn-diff">Merchant <b>{merchLabel(a.prev_merchant_id)}</b> → <b>{merchLabel(a.new_merchant_id)}</b></span>
                          {/if}
                          {#if a.prev_one_off !== a.new_one_off}
                            <span class="txn-diff">One-off <b>{a.prev_one_off ? "yes" : "no"}</b> → <b>{a.new_one_off ? "yes" : "no"}</b></span>
                          {/if}
                          {#if a.reverted}<span class="badge">reverted</span>{/if}
                        </div>
                      </div>
                    {/each}
                  </div>
                {/if}
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
    </div>
  {/if}
</section>

<style>
  .section-label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
    margin: 4px 0 8px;
  }
  .notice {
    margin: 2px 0 10px;
    padding: 8px 12px;
    border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--warn) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 32%, transparent);
    color: var(--text-muted);
    font-size: 13px;
    line-height: 1.5;
  }
  .preview-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    min-height: 24px;
    margin: 12px 0 4px;
  }
  .rule {
    padding: 11px 0;
    border-bottom: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  /* A rule's name and its four controls, side by side until they don't fit.
     Was `.row.spread`, which put both in one non-wrapping line: the four controls need ~175px
     and shrank rather than dropping, leaving the name ~70px of a phone's card — so a name like
     "Loan repayment interest → Interest charged" (the shape the 72 rules shipped in migration
     0026 all have) wrapped to four lines and then ran *under* the toggle. */
  .rule-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  /* The basis is the width below which a name and the controls stop being worth putting on one
     line, not a measurement of either: over it the name grows to fill whatever is left (desktop,
     one line, as before), under it the controls wrap beneath the name (a phone). */
  .rule-name {
    flex: 1 1 16rem;
    min-width: 0;
    /* A single unbroken token — a rule named after a payee reference, say — has no space to wrap
       at, and would otherwise widen the row until the whole card scrolled sideways. */
    overflow-wrap: anywhere;
  }
  /* Never shrunk: a compressed button row is what pushed the controls back over the text. And
     right-aligned on the line it wraps onto, since `justify-content` no longer reaches it there. */
  .rule-actions {
    flex: 0 0 auto;
    margin-left: auto;
  }
  .rule:last-child {
    border-bottom: none;
  }
  .rule.off {
    opacity: 0.55;
  }
  .summary {
    line-height: 1.5;
  }
  .caret-btn {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 11px;
    padding: 0 6px 0 0;
    transition: transform 0.15s;
    display: inline-block;
  }
  .caret-btn.open {
    transform: rotate(90deg);
  }
  tr.clickable {
    cursor: pointer;
  }
  tr.has-detail td {
    border-bottom: none;
  }
  .detail-row td {
    background: var(--bg-elev);
    padding: 4px 10px 12px;
  }
  .detail-row:hover td {
    background: var(--bg-elev);
  }
  .diff {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  /* NB: do not name this `.app` — a global `.app { min-height: 100dvh }` (the shell root
     in app.css) also matches component elements and would stretch each row full-height. */
  .txn-change {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .txn-change.reverted {
    opacity: 0.55;
  }
  .txn-line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    color: inherit;
    text-decoration: none;
  }
  .txn-line:hover .txn-desc {
    text-decoration: underline;
  }
  .txn-desc {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .go {
    flex: 0 0 auto;
    color: var(--accent);
    font-size: 12px;
    opacity: 0;
    transition: opacity 0.12s;
  }
  .txn-line:hover .go {
    opacity: 1;
  }
  .txn-diffs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    font-size: 13px;
    color: var(--text-muted);
  }
  .txn-diff b {
    color: var(--text);
    font-weight: 600;
  }
</style>
