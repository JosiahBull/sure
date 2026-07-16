<script lang="ts">
  import {
    FIELDS,
    fieldDef,
    opsFor,
    arityOf,
    defaultOp,
    choiceOptions,
    isRepresentable,
    type Condition,
    type BuilderRefs,
  } from "./expr";

  let {
    condition,
    refs,
    onRemove,
  }: { condition: Condition; refs: BuilderRefs; onRemove: () => void } = $props();

  const f = $derived(fieldDef(condition.field) ?? FIELDS[0]);
  const ops = $derived(opsFor(f.type));
  const arity = $derived(arityOf(f.type, condition.op));
  const isChoice = $derived(f.type === "enum" || f.type === "ref");
  const available = $derived(
    isChoice ? choiceOptions(f, refs).filter((c) => !condition.values.includes(c.value)) : [],
  );
  const labelOf = $derived((v: string) =>
    isChoice ? (choiceOptions(f, refs).find((o) => o.value === v)?.label ?? v) : v,
  );

  let draft = $state("");

  function changeField(e: Event) {
    const key = (e.currentTarget as HTMLSelectElement).value;
    condition.field = key;
    condition.op = defaultOp(fieldDef(key)!.type);
    condition.values = [];
    draft = "";
  }

  function commitDraft() {
    const v = draft.trim();
    if (v && !condition.values.includes(v)) condition.values = [...condition.values, v];
    draft = "";
  }
  function onDraftKey(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      commitDraft();
    } else if (e.key === "Backspace" && draft === "" && condition.values.length) {
      condition.values = condition.values.slice(0, -1);
    }
  }
  function addChoice(e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    const v = sel.value;
    if (v && !condition.values.includes(v)) condition.values = [...condition.values, v];
    sel.value = "";
  }
  const removeVal = (v: string) => (condition.values = condition.values.filter((x) => x !== v));
  const setNum = (i: number, v: string) => {
    const next = [...condition.values];
    next[i] = v;
    condition.values = next;
  };
</script>

<div class="cond">
  <select class="select field" value={condition.field} onchange={changeField} aria-label="Field">
    {#each FIELDS as fd}<option value={fd.key}>{fd.label}</option>{/each}
  </select>

  <select class="select op" value={condition.op} onchange={(e) => (condition.op = e.currentTarget.value)} aria-label="Condition">
    {#each ops as o}<option value={o.key}>{o.label}</option>{/each}
  </select>

  <div class="val">
    {#if arity === "none"}
      <span class="none-hint">—</span>
    {:else if isChoice}
      <div class="tags">
        {#each condition.values as v (v)}
          <span class="tag">{labelOf(v)}<button type="button" class="x" onclick={() => removeVal(v)} aria-label="Remove">×</button></span>
        {/each}
        {#if available.length}
          <select class="select add" onchange={addChoice} aria-label="Add value">
            <option value="">{condition.values.length ? "Add…" : "Select…"}</option>
            {#each available as o}<option value={o.value}>{o.label}</option>{/each}
          </select>
        {/if}
      </div>
    {:else if arity === "many"}
      <div class="tags">
        {#each condition.values as v (v)}
          <span class="tag" class:bad={!isRepresentable(v)}>{v}<button type="button" class="x" onclick={() => removeVal(v)} aria-label="Remove">×</button></span>
        {/each}
        <input
          class="input tag-input"
          bind:value={draft}
          onkeydown={onDraftKey}
          onblur={commitDraft}
          placeholder={condition.values.length ? "add…" : (f.placeholder ?? "type a value")}
        />
      </div>
    {:else if arity === "two"}
      <div class="between">
        {#if f.unit}<span class="unit">{f.unit}</span>{/if}
        <input class="input num" type="number" inputmode="decimal" value={condition.values[0] ?? ""} oninput={(e) => setNum(0, e.currentTarget.value)} placeholder="min" />
        <span class="and">and</span>
        {#if f.unit}<span class="unit">{f.unit}</span>{/if}
        <input class="input num" type="number" inputmode="decimal" value={condition.values[1] ?? ""} oninput={(e) => setNum(1, e.currentTarget.value)} placeholder="max" />
      </div>
    {:else if f.type === "money" || f.type === "int"}
      <div class="single">
        {#if f.unit}<span class="unit">{f.unit}</span>{/if}
        <input class="input num" type="number" inputmode="decimal" value={condition.values[0] ?? ""} oninput={(e) => setNum(0, e.currentTarget.value)} placeholder={f.placeholder ?? "0"} />
      </div>
    {:else}
      <input class="input" value={condition.values[0] ?? ""} oninput={(e) => (condition.values = [e.currentTarget.value])} placeholder={f.placeholder ?? "value"} />
    {/if}
  </div>

  <button type="button" class="btn btn-sm remove" onclick={onRemove} title="Remove condition" aria-label="Remove condition">✕</button>
</div>

<style>
  .cond {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .field {
    width: auto;
    flex: 0 0 auto;
    font-weight: 550;
  }
  .op {
    width: auto;
    flex: 0 0 auto;
    color: var(--text-muted);
  }
  .val {
    flex: 1 1 180px;
    min-width: 140px;
  }
  .none-hint {
    color: var(--text-faint);
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 4px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r);
    background: var(--bg-elev);
    min-height: 38px;
  }
  .tags:focus-within {
    outline: 2px solid rgba(45, 212, 191, 0.4);
    border-color: var(--accent);
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 4px 2px 9px;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--text);
    font-size: 13px;
    white-space: nowrap;
  }
  .tag.bad {
    background: color-mix(in srgb, var(--negative) 18%, transparent);
    color: var(--negative);
  }
  .tag .x {
    border: none;
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 15px;
    line-height: 1;
    padding: 0 4px;
    border-radius: 999px;
  }
  .tag .x:hover {
    color: var(--negative);
  }
  .tag-input {
    flex: 1 1 80px;
    width: auto;
    min-width: 80px;
    border: none;
    background: transparent;
    padding: 4px 6px;
  }
  .tag-input:focus {
    outline: none;
  }
  .add {
    width: auto;
    padding: 4px 8px;
    font-size: 13px;
    border-style: dashed;
  }
  .single,
  .between {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .num {
    width: auto;
    flex: 1 1 90px;
    min-width: 80px;
  }
  .unit {
    color: var(--text-faint);
    font-size: 13px;
  }
  .and {
    color: var(--text-faint);
    font-size: 13px;
  }
  .remove {
    flex: 0 0 auto;
    color: var(--text-faint);
    padding: 5px 9px;
  }
  .remove:hover {
    color: var(--negative);
    border-color: rgba(251, 113, 133, 0.4);
  }
</style>
