<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    children: Snippet;
    /** The buttons revealed by swiping left; the last one is the one a long swipe fires. */
    actions: Snippet;
    /** Whether this row is the open one. The parent keeps at most one open at a time. */
    open?: boolean;
    /** Width of the revealed actions, in px. */
    width?: number;
    onopen?: () => void;
    onclose?: () => void;
    /** A swipe carried most of the way across the row. */
    onfull?: () => void;
  };
  let { children, actions, open = false, width = 144, onopen, onclose, onfull }: Props = $props();

  let x = $state(0);
  let dragging = $state(false);
  let rowEl = $state<HTMLElement | null>(null);

  let axis: "h" | "v" | null = null;
  let startX = 0;
  let startY = 0;
  let startOffset = 0;
  let swallowClick = false;

  $effect(() => {
    if (!dragging) x = open ? -width : 0;
  });

  function down(e: PointerEvent) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    axis = null;
    startX = e.clientX;
    startY = e.clientY;
    startOffset = x;
    swallowClick = false;
  }
  function move(e: PointerEvent) {
    if (e.buttons === 0 && e.pointerType === "mouse") return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (axis === null) {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      axis = Math.abs(dx) > Math.abs(dy) * 1.2 ? "h" : "v";
      if (axis === "h") {
        dragging = true;
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      }
    }
    if (axis !== "h") return;
    swallowClick = true;
    // Rubber-bands past the row's own width rather than hitting a wall.
    const limit = (rowEl?.offsetWidth ?? 320) * 0.9;
    x = Math.max(-limit, Math.min(0, startOffset + dx));
  }
  function up() {
    if (axis !== "h") return;
    const full = (rowEl?.offsetWidth ?? 320) * 0.6;
    dragging = false;
    if (-x > full && onfull) {
      x = 0;
      onclose?.();
      onfull();
      return;
    }
    if (-x > width / 2) {
      x = -width;
      onopen?.();
    } else {
      x = 0;
      onclose?.();
    }
  }
  function cancel() {
    if (axis === "h") {
      dragging = false;
      x = open ? -width : 0;
    }
  }
  // A drag ends in a click on whatever is under the finger; that is not a tap on the row.
  function clickCapture(e: MouseEvent) {
    if (swallowClick) {
      swallowClick = false;
      e.stopPropagation();
      e.preventDefault();
    } else if (open) {
      // Tapping an open row shuts it rather than opening what it holds.
      e.stopPropagation();
      e.preventDefault();
      onclose?.();
    }
  }
</script>

<div class="swipe" bind:this={rowEl}>
  <div
    class="swipe-actions"
    class:closed={!open && x === 0 && !dragging}
    style:width="{Math.max(width, -x)}px"
    style:--grow={-x > width ? 4 : 1}
    inert={!open && x === 0}
  >
    {@render actions()}
  </div>
  <div
    class="swipe-content"
    class:dragging
    style:transform="translateX({x}px)"
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={cancel}
    onclickcapture={clickCapture}
    role="presentation"
  >
    {@render children()}
  </div>
</div>

<style>
  .swipe {
    position: relative;
    overflow: hidden;
  }
  .swipe-content {
    position: relative;
    background: var(--surface);
    /* Vertical scrolling stays the browser's; the horizontal gesture is ours. */
    touch-action: pan-y;
    transition: transform 0.22s cubic-bezier(0.2, 0.9, 0.3, 1);
  }
  .swipe-content.dragging {
    transition: none;
  }
  .swipe-actions {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    display: flex;
  }
  /* Hidden only once the row has finished sliding shut: an opaque row over an opaque button
     still lets the button's colour bleed through the row's anti-aliased rounded corners. */
  .swipe-actions.closed {
    visibility: hidden;
    transition: visibility 0s linear 0.22s;
  }
  .swipe-actions :global(button) {
    flex: 1 1 0;
    min-width: 0;
    border: 0;
    padding: 0 4px;
    font: inherit;
    font-size: 14px;
    font-weight: 600;
    color: #fff;
    cursor: pointer;
  }
  .swipe-actions :global(button:last-child) {
    flex-grow: var(--grow, 1);
  }
  @media (prefers-reduced-motion: reduce) {
    .swipe-content {
      transition: none;
    }
  }
</style>
