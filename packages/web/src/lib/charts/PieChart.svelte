<script lang="ts">
  // Donut rendered with circle stroke-dasharray — no trig, and full circles "just work".
  // Segments are hoverable (dim the others, name the hovered one in the centre) and, when
  // `onselect` is given, clickable. Hover is controlled via `active` + `onhover` so the
  // parent can sync it with an external legend.
  interface Slice {
    label: string;
    value: number;
    color: string;
    categoryId?: number | null;
  }
  let {
    slices,
    size = 190,
    thickness = 30,
    centerLabel = "",
    centerValue = "",
    active = null,
    onhover,
    onselect,
    format,
  }: {
    slices: Slice[];
    size?: number;
    thickness?: number;
    centerLabel?: string;
    centerValue?: string;
    active?: number | null;
    onhover?: (index: number | null) => void;
    onselect?: (index: number) => void;
    format?: (value: number) => string;
  } = $props();

  const r = $derived((size - thickness) / 2);
  const c = $derived(size / 2);
  const circ = $derived(2 * Math.PI * r);
  const total = $derived(Math.max(1e-9, slices.reduce((s, x) => s + Math.max(0, x.value), 0)));

  // Each segment keeps its original slice index so hover/select map back to the parent's
  // slice list (which may include zero-value entries we skip drawing).
  const segments = $derived.by(() => {
    let acc = 0;
    const out: { i: number; label: string; color: string; len: number; offset: number }[] = [];
    slices.forEach((s, i) => {
      if (s.value <= 0) return;
      const len = (s.value / total) * circ;
      out.push({ i, label: s.label, color: s.color, len, offset: -acc });
      acc += len;
    });
    return out;
  });

  const hoveredSlice = $derived(active != null ? slices[active] : null);
</script>

<div class="pie" style="width:{size}px;height:{size}px">
  <svg width={size} height={size} viewBox="0 0 {size} {size}">
    <g transform="rotate(-90 {c} {c})">
      <circle cx={c} cy={c} {r} fill="none" stroke="var(--surface-2)" stroke-width={thickness} />
      {#each segments as s (s.i)}
        <!-- Mouse/touch enhancement; the parent's legend provides the keyboard-accessible path. -->
        <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
        <circle
          class="seg"
          class:dim={active != null && active !== s.i}
          class:interactive={!!onselect}
          cx={c}
          cy={c}
          {r}
          fill="none"
          stroke={s.color}
          stroke-width={thickness}
          stroke-dasharray="{s.len} {circ - s.len}"
          stroke-dashoffset={s.offset}
          role={onselect ? "button" : undefined}
          aria-label={onselect ? s.label : undefined}
          onpointerenter={() => onhover?.(s.i)}
          onpointerleave={() => onhover?.(null)}
          onclick={() => onselect?.(s.i)}
        />
      {/each}
    </g>
  </svg>
  {#if hoveredSlice}
    <div class="pie-center">
      <div class="cv tabular">{format ? format(hoveredSlice.value) : hoveredSlice.value}</div>
      <div class="cl">{hoveredSlice.label}</div>
    </div>
  {:else if centerLabel || centerValue}
    <div class="pie-center">
      <div class="cv tabular">{centerValue}</div>
      <div class="cl">{centerLabel}</div>
    </div>
  {/if}
</div>

<style>
  .pie {
    position: relative;
    flex: none;
  }
  .seg {
    transition: opacity 0.15s ease;
  }
  .seg.interactive {
    cursor: pointer;
  }
  .seg.dim {
    opacity: 0.22;
  }
  .pie-center {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    pointer-events: none;
    padding: 0 6px;
  }
  .cv {
    font-size: 18px;
    font-weight: 680;
  }
  .cl {
    font-size: 11px;
    color: var(--text-faint);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
