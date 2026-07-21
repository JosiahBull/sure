<script lang="ts">
  // A single-row bar of proportionally-sized colored segments — the Balance Sheet card's
  // assets/liabilities weight bar. Segments are plain flex children sized by % width, no SVG.
  interface Segment {
    label: string;
    color: string;
    weightPct: number;
  }
  let { segments }: { segments: Segment[] } = $props();
</script>

<div class="weightbar" role="img" aria-label="Weight by category">
  {#each segments as s (s.label)}
    <div
      class="seg"
      style="width:{s.weightPct}%;background:{s.color}"
      title="{s.label} — {s.weightPct.toFixed(1)}%"
    ></div>
  {/each}
</div>

<style>
  .weightbar {
    display: flex;
    height: 8px;
    border-radius: 999px;
    overflow: hidden;
    background: var(--surface-2);
  }
  .seg {
    height: 100%;
  }
</style>
