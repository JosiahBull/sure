<script lang="ts">
  // Donut rendered with circle stroke-dasharray — no trig, and full circles "just work".
  interface Slice {
    label: string;
    value: number;
    color: string;
  }
  let {
    slices,
    size = 190,
    thickness = 30,
    centerLabel = "",
    centerValue = "",
  }: {
    slices: Slice[];
    size?: number;
    thickness?: number;
    centerLabel?: string;
    centerValue?: string;
  } = $props();

  const r = $derived((size - thickness) / 2);
  const c = $derived(size / 2);
  const circ = $derived(2 * Math.PI * r);
  const total = $derived(Math.max(1e-9, slices.reduce((s, x) => s + Math.max(0, x.value), 0)));

  const segments = $derived.by(() => {
    let acc = 0;
    return slices
      .filter((s) => s.value > 0)
      .map((s) => {
        const len = (s.value / total) * circ;
        const seg = { color: s.color, len, offset: -acc };
        acc += len;
        return seg;
      });
  });
</script>

<div class="pie" style="width:{size}px;height:{size}px">
  <svg width={size} height={size} viewBox="0 0 {size} {size}">
    <g transform="rotate(-90 {c} {c})">
      <circle cx={c} cy={c} {r} fill="none" stroke="var(--surface-2)" stroke-width={thickness} />
      {#each segments as s}
        <circle
          cx={c}
          cy={c}
          {r}
          fill="none"
          stroke={s.color}
          stroke-width={thickness}
          stroke-dasharray="{s.len} {circ - s.len}"
          stroke-dashoffset={s.offset}
        />
      {/each}
    </g>
  </svg>
  {#if centerLabel || centerValue}
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
  .pie-center {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    pointer-events: none;
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
  }
</style>
