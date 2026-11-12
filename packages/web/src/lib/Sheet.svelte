<script lang="ts" module>
  // Sheets can stack (an action sheet opening a form), and each one locks the page behind it.
  // Counted rather than a boolean so closing the top one does not unlock the one beneath.
  let locks = 0;
  // Open sheets, innermost last: Escape belongs to the one on top, not to all of them at once.
  const stack: object[] = [];
</script>

<script lang="ts">
  import { onDestroy, tick, type Snippet } from "svelte";

  type Props = {
    open: boolean;
    title: string;
    /** The label of the button that dismisses it. */
    closeLabel?: string;
    onclose?: () => void;
    children: Snippet;
    footer?: Snippet;
  };
  let { open = $bindable(false), title, closeLabel = "Done", onclose, children, footer }: Props = $props();

  let panelEl = $state<HTMLElement | null>(null);
  let dragY = $state(0);
  let dragging = $state(false);
  let restoreTo: HTMLElement | null = null;
  let locked = false;
  const token = {};
  // When a control inside last settled. On iOS, dismissing a native picker can deliver a tap to
  // whatever was under the finger — the scrim — and a sheet that closes itself the moment a
  // choice is made is exactly the behaviour this component exists to avoid.
  let lastInteraction = 0;

  function close() {
    open = false;
    onclose?.();
  }

  // Moves the node to <body>: `.main-col` is a size container, and a fixed-position box inside
  // one is laid out against that rather than against the screen.
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  $effect(() => {
    if (!open) return;
    lastInteraction = 0;
    restoreTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (locks++ === 0) document.body.classList.add("scroll-locked");
    locked = true;
    stack.push(token);
    void tick().then(() => panelEl?.focus({ preventScroll: true }));
    return unlock;
  });
  function unlock() {
    if (locked) {
      locked = false;
      stack.splice(stack.indexOf(token), 1);
      if (--locks === 0) document.body.classList.remove("scroll-locked");
    }
    restoreTo?.focus({ preventScroll: true });
    restoreTo = null;
  }
  onDestroy(() => {
    if (locked) unlock();
  });

  function onKeydown(e: KeyboardEvent) {
    if (open && e.key === "Escape" && stack[stack.length - 1] === token) {
      e.stopPropagation();
      close();
    }
  }
  function scrimClick() {
    if (Date.now() - lastInteraction < 400) return;
    close();
  }

  // Drag the grabber/title down to dismiss, the way every iOS sheet does.
  let startY = 0;
  let startT = 0;
  function dragStart(e: PointerEvent) {
    if ((e.target as HTMLElement).closest("button")) return;
    dragging = true;
    startY = e.clientY;
    startT = e.timeStamp;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function dragMove(e: PointerEvent) {
    if (dragging) dragY = Math.max(0, e.clientY - startY);
  }
  function dragEnd(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    const fast = dragY / Math.max(1, e.timeStamp - startT) > 0.6;
    const far = dragY > (panelEl?.offsetHeight ?? 400) * 0.35;
    if (dragY > 8 && (fast || far)) close();
    dragY = 0;
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <div class="sheet-root" use:portal>
    <button type="button" class="sheet-scrim" aria-label="Close {title}" tabindex="-1" onclick={scrimClick}></button>
    <div
      class="sheet"
      class:dragging
      role="dialog"
      aria-modal="true"
      aria-label={title}
      tabindex="-1"
      bind:this={panelEl}
      style:transform={dragY ? `translateY(${dragY}px)` : undefined}
      onfocusout={(e) => {
        if (e.target instanceof HTMLSelectElement) lastInteraction = Date.now();
      }}
      onchange={(e) => {
        if (e.target instanceof HTMLSelectElement) lastInteraction = Date.now();
      }}
    >
      <header
        class="sheet-head"
        role="presentation"
        onpointerdown={dragStart}
        onpointermove={dragMove}
        onpointerup={dragEnd}
        onpointercancel={dragEnd}
      >
        <span class="sheet-grab" aria-hidden="true"></span>
        <h2 class="sheet-title">{title}</h2>
        <button type="button" class="sheet-done" onclick={close}>{closeLabel}</button>
      </header>
      <div class="sheet-body">{@render children()}</div>
      {#if footer}<footer class="sheet-foot">{@render footer()}</footer>{/if}
    </div>
  </div>
{/if}

<style>
  /* Sized in viewport units, not `inset: 0` — see the drawer scrim in App.svelte: on a phone a
     fixed box's edges resolve against the layout viewport, which can be larger than the screen. */
  .sheet-root {
    position: fixed;
    top: 0;
    left: 0;
    width: 100dvw;
    height: 100dvh;
    z-index: 80;
    display: flex;
    align-items: flex-end;
    justify-content: center;
  }
  .sheet-scrim {
    position: absolute;
    inset: 0;
    border: 0;
    padding: 0;
    background: rgba(0, 0, 0, 0.4);
    animation: sheet-fade 0.2s ease-out;
  }
  .sheet {
    position: relative;
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 560px;
    max-height: min(88dvh, 100% - 12px);
    background: var(--bg-elev, var(--surface));
    color: var(--text);
    border-radius: 16px 16px 0 0;
    box-shadow: 0 -8px 40px rgba(0, 0, 0, 0.3);
    outline: none;
    animation: sheet-up 0.28s cubic-bezier(0.2, 0.9, 0.3, 1);
    transition: transform 0.2s ease-out;
  }
  .sheet.dragging {
    transition: none;
  }
  .sheet-head {
    flex: none;
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 52px;
    padding: 14px 16px 6px;
    /* The drag handle: the page must not scroll under a finger that is dismissing the sheet. */
    touch-action: none;
    cursor: grab;
  }
  .sheet-grab {
    position: absolute;
    top: 6px;
    left: 50%;
    width: 36px;
    height: 5px;
    margin-left: -18px;
    border-radius: 3px;
    background: var(--border-strong);
  }
  .sheet-title {
    font-size: 16px;
    font-weight: 600;
    letter-spacing: normal;
  }
  .sheet-done {
    position: absolute;
    right: 6px;
    top: 50%;
    margin-top: -2px;
    transform: translateY(-50%);
    min-height: 44px;
    padding: 0 12px;
    border: 0;
    background: none;
    color: var(--accent);
    font-size: 16px;
    font-weight: 600;
    cursor: pointer;
  }
  .sheet-body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    -webkit-overflow-scrolling: touch;
    padding: 4px 16px 16px;
  }
  .sheet-foot {
    flex: none;
    padding: 10px 16px calc(10px + env(safe-area-inset-bottom));
    border-top: 1px solid var(--border);
  }
  /* Without a footer the body is the last thing on screen and owns the home-indicator gap. */
  .sheet-body:last-child {
    padding-bottom: calc(16px + env(safe-area-inset-bottom));
  }
  @keyframes sheet-up {
    from {
      transform: translateY(100%);
    }
  }
  @keyframes sheet-fade {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .sheet,
    .sheet-scrim {
      animation: none;
      transition: none;
    }
  }
</style>
