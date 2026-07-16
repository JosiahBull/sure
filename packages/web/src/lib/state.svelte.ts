// Global, reactive report filters shared across pages (time range + one-off toggle).

export type RangeKey = "last_month" | "last_90" | "ytd" | "last_12m" | "all";

export const RANGES: { key: RangeKey; label: string }[] = [
  { key: "last_month", label: "Last month" },
  { key: "last_90", label: "Last 90 days" },
  { key: "ytd", label: "Year to date" },
  { key: "last_12m", label: "Last 12 months" },
  { key: "all", label: "All time" },
];

export const filters = $state({
  range: "last_12m" as RangeKey,
  includeOneOff: false,
  /** Brush-selected window (Grafana-style zoom) that overrides `range` while set. */
  custom: null as { from: string; to: string } | null,
});

function iso(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/** Resolve the active range to `{ from, to }` ISO dates (empty for "all time"). */
export function rangeDates(range: RangeKey = filters.range): { from?: string; to?: string } {
  const now = new Date();
  const to = iso(now);
  const d = new Date(now);
  switch (range) {
    case "last_month":
      d.setMonth(d.getMonth() - 1);
      return { from: iso(d), to };
    case "last_90":
      d.setDate(d.getDate() - 90);
      return { from: iso(d), to };
    case "ytd":
      return { from: `${now.getFullYear()}-01-01`, to };
    case "last_12m":
      d.setFullYear(d.getFullYear() - 1);
      return { from: iso(d), to };
    case "all":
      return {};
  }
}

/** Effective query window: an active brush selection wins over the preset range. */
export function activeRange(): { from?: string; to?: string } {
  if (filters.custom) return { from: filters.custom.from, to: filters.custom.to };
  return rangeDates(filters.range);
}
