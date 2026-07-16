<script lang="ts">
  import RuleGroup from "./RuleGroup.svelte";
  import RuleConditionRow from "./RuleConditionRow.svelte";
  import { newCondition, newGroup, type Group, type RuleNode, type BuilderRefs } from "./expr";

  let {
    group,
    refs,
    onRemove,
    depth = 0,
  }: { group: Group; refs: BuilderRefs; onRemove?: () => void; depth?: number } = $props();

  const remove = (child: RuleNode) => (group.children = group.children.filter((c) => c !== child));
  const addCondition = () => (group.children = [...group.children, newCondition()]);
  const addGroup = () => (group.children = [...group.children, newGroup(group.combinator === "and" ? "or" : "and")]);
</script>

<div class="grp" class:or={group.combinator === "or"} class:nested={depth > 0}>
  <div class="grp-head">
    <span class="lead">Match</span>
    <div class="seg" role="group" aria-label="Combine conditions">
      <button type="button" class="seg-btn" class:on={group.combinator === "and"} onclick={() => (group.combinator = "and")}>All</button>
      <button type="button" class="seg-btn" class:on={group.combinator === "or"} onclick={() => (group.combinator = "or")}>Any</button>
    </div>
    <span class="lead">of&nbsp;these</span>
    <div class="spacer"></div>
    {#if onRemove}
      <button type="button" class="btn btn-sm ghost" onclick={onRemove} title="Remove group" aria-label="Remove group">✕ group</button>
    {/if}
  </div>

  {#if group.children.length === 0}
    <div class="empty-grp">No conditions — add one below.</div>
  {/if}

  <div class="children">
    {#each group.children as child, i (child.id)}
      {#if i > 0}<div class="joiner">{group.combinator === "and" ? "AND" : "OR"}</div>{/if}
      <div class="child">
        {#if child.kind === "group"}
          <RuleGroup group={child} {refs} depth={depth + 1} onRemove={() => remove(child)} />
        {:else}
          <RuleConditionRow condition={child} {refs} onRemove={() => remove(child)} />
        {/if}
      </div>
    {/each}
  </div>

  <div class="grp-actions">
    <button type="button" class="btn btn-sm" onclick={addCondition}>+ Condition</button>
    <button type="button" class="btn btn-sm" onclick={addGroup}>+ Group</button>
  </div>
</div>

<style>
  .grp {
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: var(--r);
    padding: 12px;
    background: color-mix(in srgb, var(--accent) 4%, transparent);
  }
  .grp.or {
    border-left-color: var(--warn);
    background: color-mix(in srgb, var(--warn) 5%, transparent);
  }
  .grp.nested {
    background: var(--bg-elev);
  }
  .grp-head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
  }
  .lead {
    font-size: 13px;
    color: var(--text-muted);
  }
  .spacer {
    flex: 1;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    overflow: hidden;
  }
  .seg-btn {
    border: none;
    background: transparent;
    color: var(--text-muted);
    font-size: 13px;
    font-weight: 600;
    padding: 4px 12px;
    cursor: pointer;
  }
  .seg-btn.on {
    background: var(--accent);
    color: var(--accent-ink);
  }
  .grp.or .seg-btn.on {
    background: var(--warn);
    color: #2a1c00;
  }
  .children {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .child {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    padding: 8px;
  }
  .grp.nested .child {
    background: var(--surface-2);
  }
  .joiner {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    color: var(--text-faint);
    padding-left: 4px;
  }
  .empty-grp {
    color: var(--text-faint);
    font-size: 13px;
    padding: 4px 0 10px;
  }
  .grp-actions {
    display: flex;
    gap: 8px;
    margin-top: 10px;
  }
  .ghost {
    color: var(--text-faint);
  }
  .ghost:hover {
    color: var(--negative);
    border-color: rgba(251, 113, 133, 0.4);
  }
</style>
