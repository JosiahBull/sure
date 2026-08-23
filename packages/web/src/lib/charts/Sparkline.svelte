<script lang="ts">
  // A category's own monthly history, as bars, small enough to sit inside a table row.
  //
  // This exists to answer one question — "why does the projection say that?" — which the
  // Assumptions tab could not answer at all before. A row reading `growth +25.0%/yr` next to no
  // history gives a reader nothing to disbelieve, and that is how a clamped, over-fitted rate
  // survived on six of seven categories unnoticed. Twenty-four bars and a rule make the same
  // claim checkable at a glance.
  //
  // Deliberately not a `LineChart`: these series are lumpy and often mostly zeros, and a line
  // through them implies a continuity the data does not have. A month either had spending in it
  // or it did not, which is a bar.

  let {
    /// Oldest first, base-currency minor units.
    values,
    /// How many months at the end of `values` the fit actually used. The rest is drawn greyed:
    /// excluded is a different statement from absent, and a reader who cannot see what was
    /// dropped cannot tell a break from a short history.
    usedMonths = values.length,
    height = 26,
    label = "",
  }: {
    values: number[];
    usedMonths?: number;
    height?: number;
    label?: string;
  } = $props();

  // Scaled to the 90th percentile of the non-empty months, not to the maximum.
  //
  // Household spending is lumpy enough that peak-scaling shows one spike and twenty-three
  // hairlines: a single $3,354 month makes the $1,000 months that matter indistinguishable from
  // the empty ones, which defeats the whole purpose of drawing the series. Bars above the p90
  // saturate at full height instead, and `title` carries their real figure for the one reader who
  // wants it.
  const scale = $derived.by(() => {
    const nonZero = values.filter((v) => v > 0).sort((a, b) => a - b);
    if (nonZero.length === 0) return 1;
    // With few months p90 is the maximum anyway, so this degrades to peak-scaling exactly where
    // peak-scaling is correct.
    return nonZero[Math.min(nonZero.length - 1, Math.floor(nonZero.length * 0.9))] || 1;
  });
  const firstUsed = $derived(Math.max(0, values.length - usedMonths));
  // A zero month still gets a hairline, so "no spending" reads as a measured month rather than a
  // gap in the chart. 1.5px is the smallest that survives a non-integer device pixel ratio.
  const floor = 1.5;
</script>

<div
  class="spark"
  style="height:{height}px"
  role="img"
  aria-label={label ||
    `${values.length} months of history, ${usedMonths} of them used in the projection`}
>
  {#each values as v, i (i)}
    <span
      class="bar"
      class:excluded={i < firstUsed}
      class:boundary={i === firstUsed && firstUsed > 0}
      class:clipped={v > scale}
      style="height:{Math.min(height, Math.max(floor, (Math.max(v, 0) / scale) * height))}px"
    ></span>
  {/each}
</div>

<style>
  .spark {
    display: flex;
    align-items: flex-end;
    gap: 1px;
    /* A fixed width so every category's history is read at the same scale on the horizontal axis
       too: a fit over eight months and one over twenty-four should not draw the same width, or the
       bar spacing itself becomes a misleading signal about how much data there is. 9px per month
       against the 24-month maximum. */
    width: 216px;
    flex: none;
  }
  .bar {
    flex: 1 1 0;
    min-width: 3px;
    background: var(--text-muted);
    border-radius: 1px;
  }
  /* Dropped by the structural break. Faint rather than hidden — the point is that a reader can
     see there was a different regime before this one. */
  .excluded {
    background: var(--border-strong);
  }
  /* The first month of the current regime, which is where the level is measured from. Marked on
     the bar itself rather than as a separate rule so it cannot drift out of alignment with the
     bars when the count changes. */
  .boundary {
    background: var(--warn);
  }
  /* A month above the p90, drawn at full height rather than at its true one. Flagged so a reader
     can tell a saturated bar from one that merely reached the top of the scale. */
  .clipped {
    border-top: 2px solid var(--negative);
  }
</style>
