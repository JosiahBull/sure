// Global, reactive report filters shared across pages (time range + one-off toggle).

export type RangeKey =
  | "mtd"
  | "last_30"
  | "last_month"
  | "last_90"
  | "ytd"
  | "last_12m"
  | "all";

// Ordered by how much time each one covers, shortest first — "Month to date" is one to
// thirty-one days, so it leads. It is also the pair to "Year to date" below: both are
// open-ended windows running from the start of a calendar period up to today, as against the
// closed "Last month" and the rolling "Last 30 days".
export const RANGES: { key: RangeKey; label: string }[] = [
  { key: "mtd", label: "Month to date (MTD)" },
  { key: "last_30", label: "Last 30 days" },
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
  /**
   * Whose money the reports describe: an `ownershipKey` ("person:3" / "joint"), or "" for
   * the whole household — which stays the default, because the household total is still the
   * number you usually want.
   */
  attributedTo: "",
});

/**
 * The `attributed_to` query param for the reports, or undefined for the whole household.
 * The wire form is a bare id or "joint"; the UI's key form carries a "person:" prefix.
 */
export function attributionParam(): string | undefined {
  const key = filters.attributedTo;
  if (key === "") return undefined;
  return key.startsWith("person:") ? key.slice("person:".length) : key;
}

/**
 * A date as the calendar day it is *here*, not in UTC. `toISOString()` would answer for
 * UTC, which in NZ (UTC+12/13) is the previous day for the whole local morning — so a
 * range asked for at 9am started a day early, and "Last month" would name the wrong month
 * outright, its boundaries being midnight-adjacent by construction.
 */
function iso(d: Date): string {
  const m = `${d.getMonth() + 1}`.padStart(2, "0");
  const day = `${d.getDate()}`.padStart(2, "0");
  return `${d.getFullYear()}-${m}-${day}`;
}

/** Resolve the active range to `{ from, to }` ISO dates (empty for "all time"). */
export function rangeDates(range: RangeKey = filters.range): { from?: string; to?: string } {
  const now = new Date();
  const to = iso(now);
  const d = new Date(now);
  switch (range) {
    // The previous *calendar* month: a closed window that ends before today, which is what
    // "last month" means when you say it out loud ("what did August cost?"). The rolling
    // month-back window it used to mean is "Last 30 days", below.
    case "last_month": {
      const start = new Date(now);
      start.setDate(1);
      start.setMonth(start.getMonth() - 1);
      const end = new Date(now);
      end.setDate(0);
      return { from: iso(start), to: iso(end) };
    }
    // The current calendar month so far: the 1st through today. Open-ended like "ytd" below,
    // and unlike "last_month" above, which is the closed window before this one. No month
    // arithmetic, so none of the overflow traps that go with it — only the day is changed.
    case "mtd":
      d.setDate(1);
      return { from: iso(d), to };
    case "last_30":
      d.setDate(d.getDate() - 30);
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
