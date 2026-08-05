/**
 * The forecast chart's x-axis, extracted so anything drawn *alongside* the chart lands on the
 * same pixel as the chart itself.
 *
 * `ForecastChart` renders into a fixed `viewBox` at `width:100%` with
 * `preserveAspectRatio="none"`, which has one very useful consequence: viewBox x maps linearly
 * onto rendered container width. So an HTML overlay positioned with `sxPct` sits exactly on top
 * of an SVG element positioned with `sxFor`, at any container size, with no measurement.
 *
 * That property is what the life-event annotations and the timeline strip are built on — and the
 * reason this lives in its own module rather than inside the chart. The chart and the strip
 * previously would have had to keep matching copies of the same three constants, which is the
 * arrangement `CHECKPOINTS` was already in (duplicated between `ForecastChart.svelte` and
 * `Forecast.svelte`) and which had already drifted once.
 */

/** viewBox width. Height varies with the `height` prop; width never does. */
export const CHART_W = 640;

/** Horizontal padding, in viewBox units. Vertical padding belongs to the chart: it grows with
 *  the number of label lanes, and nothing outside the chart can know that. */
export const CHART_PAD_X = { l: 6, r: 10 } as const;

/** x, in viewBox units, of point `i` in an evenly-spaced series of `n` points. */
export function sxFor(i: number, n: number): number {
  return CHART_PAD_X.l + (CHART_W - CHART_PAD_X.l - CHART_PAD_X.r) * (n <= 1 ? 0 : i / (n - 1));
}

/** The same x as a percentage of rendered width, for positioning an HTML overlay. */
export function sxPct(i: number, n: number): number {
  return (sxFor(i, n) / CHART_W) * 100;
}

/**
 * The combined-series index for a month offset from today.
 *
 * History and projection share one index-based axis, and the projection's month 0 *is* the last
 * historical point (the seam) — so an offset of 0 is `histLen - 1`, not `histLen`.
 */
export function seriesIndex(histLen: number, monthOffset: number): number {
  return histLen - 1 + monthOffset;
}

/**
 * Horizon options, in months. Years rather than months because life events are decade-scale:
 * at the old five-year ceiling most of them fell outside the window entirely.
 */
export const HORIZONS = [
  { months: 12, label: "1 year" },
  { months: 24, label: "2 years" },
  { months: 60, label: "5 years" },
  { months: 120, label: "10 years" },
  { months: 240, label: "20 years" },
  { months: 360, label: "30 years" },
] as const;

/**
 * Checkpoint months for a horizon, always including the final month.
 *
 * A fixed +3/+6/+9/+12 is four labels inside the first 3% of a 30-year chart — technically
 * correct and completely useless. The last month is always included because "where does this end
 * up" is the question the page exists to answer.
 */
export function checkpointsFor(horizon: number): number[] {
  const steps =
    horizon <= 12
      ? [3, 6, 9, 12]
      : horizon <= 36
        ? [6, 12, 24, 36]
        : horizon <= 120
          ? [12, 24, 60, 120]
          : [12, 60, 120, 240, 360];
  const within = steps.filter((m) => m <= horizon);
  return within.at(-1) === horizon ? within : [...within, horizon];
}

/** "+18 months" is readable; "+240 months" is not. */
export function horizonLabel(months: number): string {
  if (months < 24) return `${months} month${months === 1 ? "" : "s"}`;
  const years = months / 12;
  return `${Number.isInteger(years) ? years : years.toFixed(1)} years`;
}

/**
 * How much history to draw beside a projection of `horizon` months.
 *
 * History shares the projection's axis, so a fixed window silently shrinks as the horizon grows
 * — one year against thirty is a 3% sliver and the actuals line becomes a nub. Half the horizon,
 * floored at 12 months and capped at 60: at the 12-month horizon this is exactly the year that
 * was shown before, so no existing view moves, and at 30 years it is five years of trend without
 * squeezing the projection off the right-hand side. The net-worth report is monthly-sampled, so
 * 60 points costs nothing.
 */
export function historyMonthsFor(horizon: number): number {
  return Math.max(12, Math.min(60, Math.round(horizon / 2)));
}

/**
 * Year ticks for a projection of `months.length` months, as `{ index, label }` against the
 * combined series.
 *
 * One label per year is thirty labels in 640 viewBox units, so the step thins to whatever keeps
 * at most six of them. "Today" is always drawn, on the seam, because it is the only tick whose
 * position means something independent of its label.
 */
export function yearTicks(
  histLen: number,
  months: { as_of: string }[]
): { index: number; label: string; today: boolean }[] {
  const out: { index: number; label: string; today: boolean }[] = [];
  if (histLen > 0) out.push({ index: histLen - 1, label: "Today", today: true });
  const years = months.length / 12;
  const step = [1, 2, 5, 10].find((s) => years / s <= 6) ?? 10;
  for (let m = step * 12; m <= months.length; m += step * 12) {
    const at = months[m - 1];
    if (at) out.push({ index: seriesIndex(histLen, m), label: at.as_of.slice(0, 4), today: false });
  }
  return out;
}
