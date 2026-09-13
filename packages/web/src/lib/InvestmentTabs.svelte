<script lang="ts">
  /**
   * The tab strip every investment account panel shares.
   *
   * Extracted rather than duplicated because the *point* of the shape is that the three kinds
   * read the same way: an investment's value is units × price, so each panel splits into what is
   * held, how that changed, and what one is worth. Two panels drifting into two different tab
   * strips would undo that.
   */
  type Tab = { id: string; label: string; count?: number };

  let {
    tabs,
    active,
    onselect,
  }: { tabs: Tab[]; active: string; onselect: (id: string) => void } = $props();
</script>

<div class="tabs" role="tablist">
  {#each tabs as t (t.id)}
    <button
      class="tab"
      class:active={t.id === active}
      role="tab"
      aria-selected={t.id === active}
      onclick={() => onselect(t.id)}
    >
      {t.label}
      {#if t.count !== undefined}<span class="count">{t.count}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: 2px;
    border-bottom: 1px solid var(--border);
    margin: 0 0 10px;
    overflow-x: auto;
  }
  .tab {
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    padding: 7px 10px;
    /* `--muted` is not a token — the palette defines `--text-muted` — so this resolved to
       nothing and an inactive tab inherited full-strength ink, leaving the underline as the
       only thing telling the three apart. */
    color: var(--text-muted);
    cursor: pointer;
    font: inherit;
    font-size: 0.9rem;
    white-space: nowrap;
    transition: color 0.15s, border-color 0.15s;
  }
  .tab:hover {
    color: var(--text);
  }
  .tab.active {
    color: var(--text);
    font-weight: 600;
    border-bottom-color: var(--accent);
  }
  .count {
    display: inline-block;
    margin-left: 5px;
    padding: 0 6px;
    border-radius: 999px;
    /* Not `--surface-2`: it is the same colour as `--bg-elev`, which is what every panel
       hosting this strip is painted with, so the chip had no visible fill at all. */
    background: color-mix(in srgb, var(--text) 9%, transparent);
    font-size: 0.75rem;
    font-weight: 550;
  }
</style>
