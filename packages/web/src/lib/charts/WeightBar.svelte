<script lang="ts">
  // A single-row bar of proportionally-sized coloured segments — the balance-sheet card's
  // assets/liabilities weight bar. Segments are plain flex children sized by % width, no SVG.
  //
  // Each segment is a real control, not decoration: the bar is the densest summary on the card,
  // and a reader who has found the slice they care about should not have to go looking for the
  // same thing again in the list below.
  interface Segment {
    /** Stable identity for keying and for what a click reports — the label can repeat. */
    key: string;
    label: string;
    color: string;
    weightPct: number;
    /** Right-hand line of the tooltip: the money, already formatted. */
    value?: string;
    /** Third line, when there is a period change worth stating. */
    change?: { text: string; positive: boolean } | null;
    /** What a click would do, in words. Absent means the segment is not clickable. */
    action?: string;
  }
  let {
    segments,
    onselect,
  }: { segments: Segment[]; onselect?: (key: string) => void } = $props();

  let bar = $state<HTMLDivElement>();
  let hovered = $state<number | null>(null);
  /** Tooltip x, clamped inside the bar so a segment at either end does not push it off-card. */
  let tipX = $state(0);

  function move(e: PointerEvent, i: number) {
    hovered = i;
    const r = bar?.getBoundingClientRect();
    if (r) tipX = Math.min(Math.max(e.clientX - r.left, 0), r.width);
  }
  const active = $derived(hovered === null ? null : segments[hovered]);
</script>

<div class="wrap" bind:this={bar}>
  <!-- `group`, not `img`: `role="img"` makes the whole subtree presentational, which would have
       hidden every one of these buttons from assistive tech the moment they became buttons. -->
  <div class="weightbar" role="group" aria-label="Share by category">
    {#each segments as s, i (s.key)}
      <!-- A `<button>` even when there is nothing to select, so the hover target and the
           keyboard target are the same element and the tooltip is reachable without a mouse. -->
      <button
        type="button"
        class="seg"
        class:dim={hovered !== null && hovered !== i}
        class:clickable={!!s.action}
        style="width:{s.weightPct}%;background:{s.color}"
        aria-label="{s.label}, {s.weightPct.toFixed(1)}%{s.value ? `, ${s.value}` : ''}{s.action
          ? `. ${s.action}`
          : ''}"
        tabindex={s.action ? undefined : -1}
        onpointerenter={(e) => move(e, i)}
        onpointermove={(e) => move(e, i)}
        onpointerleave={() => (hovered = null)}
        onfocus={() => (hovered = i)}
        onblur={() => (hovered = null)}
        onclick={() => s.action && onselect?.(s.key)}
      ></button>
    {/each}
  </div>
  {#if active}
    <div class="tip" class:flip={tipX > (bar?.clientWidth ?? 0) / 2} style="left:{tipX}px">
      <span class="tip-label">
        <span class="tip-dot" style="background:{active.color}"></span>
        {active.label}
      </span>
      <span class="tip-value tabular">
        {active.weightPct.toFixed(1)}%{active.value ? ` · ${active.value}` : ""}
      </span>
      {#if active.change}
        <span class="tip-change tabular" class:pos={active.change.positive} class:neg={!active.change.positive}>
          {active.change.text}
        </span>
      {/if}
      {#if active.action}<span class="tip-action">{active.action}</span>{/if}
    </div>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
  }
  .weightbar {
    display: flex;
    /* One pixel of gap between segments, drawn by the track showing through, so two adjacent
       slices of similar colour still read as two. */
    gap: 1px;
    height: 8px;
    border-radius: 999px;
    overflow: hidden;
    background: var(--surface-2);
  }
  .seg {
    height: 100%;
    padding: 0;
    border: 0;
    /* The drawn bar is 8px; the *target* is the full row height, which is what makes a slice
       only a few pixels wide hoverable at all. `background-clip: content-box` keeps the colour
       in the 8px band while the padding carries the reach. */
    box-sizing: content-box;
    padding-block: 8px;
    margin-block: -8px;
    background-clip: content-box !important;
    transition: opacity 0.12s ease;
  }
  .seg.clickable {
    cursor: pointer;
  }
  .seg.dim {
    opacity: 0.35;
  }
  .seg:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  /* Matches the money-flow chart's tooltip, which is the other thing on this page that explains
     a shape under the pointer. */
  .tip {
    position: absolute;
    top: 16px;
    z-index: 5;
    transform: translateX(-8px);
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 6px 9px;
    border-radius: var(--r-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: var(--shadow);
    font-size: 12.5px;
    line-height: 1.35;
    white-space: nowrap;
    pointer-events: none;
  }
  .tip.flip {
    transform: translateX(calc(-100% + 8px));
  }
  .tip-label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text);
    font-weight: 600;
  }
  .tip-dot {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    flex: none;
  }
  .tip-value {
    color: var(--text-muted);
  }
  .tip-change.pos {
    color: var(--positive);
  }
  .tip-change.neg {
    color: var(--negative);
  }
  .tip-action {
    color: var(--text-faint);
    font-size: 11.5px;
  }
</style>
