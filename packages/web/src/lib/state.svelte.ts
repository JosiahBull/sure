// Global, reactive report filters shared across pages (time range + one-off toggle).
//
// Deliberately *not* here: whose money a report describes. Accounts and transactions still
// carry an owner, and the app still labels and groups by it — but every view is the whole
// household's, because that is what the household is.
import { untrack } from "svelte";

import { queryParams, router, setQueryParams } from "./router.svelte";

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

/**
 * The period a visit starts on when nothing says otherwise.
 *
 * The month just gone: a closed window, the one most questions about spending are actually
 * about, and small enough that the charts say something specific rather than averaging a year
 * into a flat line.
 */
export const DEFAULT_RANGE: RangeKey = "last_month";

export const filters = $state({
  range: DEFAULT_RANGE as RangeKey,
  includeOneOff: false,
  /** Brush-selected window (Grafana-style zoom) that overrides `range` while set. */
  custom: null as { from: string; to: string } | null,
});

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

// ---- the period, in the URL --------------------------------------------------------------

/** Whether a string is one of the preset keys. */
const isRangeKey = (v: string | null): v is RangeKey => !!v && RANGES.some((r) => r.key === v);
const isDate = (v: string | null): v is string => !!v && /^\d{4}-\d{2}-\d{2}$/.test(v);

/**
 * The period the URL is asking for, or `null` when it does not ask for one.
 *
 * `?start=&end=` is a brushed window and outranks `?range=`, the same precedence the
 * transactions page has always applied to the two — a shared link to a zoomed chart means that
 * window, not the preset it was zoomed out of.
 */
export function periodFromUrl(
  params: URLSearchParams = queryParams(),
): { range: RangeKey; custom: { from: string; to: string } | null } | null {
  const start = params.get("start");
  const end = params.get("end");
  if (isDate(start) && isDate(end)) {
    const range = params.get("range");
    return { range: isRangeKey(range) ? range : filters.range, custom: { from: start, to: end } };
  }
  const range = params.get("range");
  return isRangeKey(range) ? { range, custom: null } : null;
}

/**
 * How the current period should appear in the query string.
 *
 * The default appears as nothing at all. A URL says what is unusual about a view, and
 * `#/?range=last_month` on a fresh visit is a parameter that carries no information — it is the
 * answer you would have got by saying nothing, so saying it makes every plain link longer and
 * every shared one look like a deliberate choice that was not made.
 */
function periodParams(): Record<string, string | null> {
  const isDefault = filters.range === DEFAULT_RANGE && !filters.custom;
  return {
    range: isDefault ? null : filters.range,
    start: filters.custom?.from ?? null,
    end: filters.custom?.to ?? null,
  };
}

/**
 * Keep the selected period and the address bar saying the same thing.
 *
 * Called once from the shell. Two effects rather than one because the two directions have
 * different triggers, and each only writes when the two disagree — which is what stops them
 * chasing each other.
 *
 * The reader deliberately does *nothing* when the URL names no period. A link that carries its
 * own opinion (`?tx=`, `?account=`) is a request to see everything relevant to that row, and the
 * transactions page widens the range to "all" for exactly that reason; resetting to the default
 * here because the URL happened to be silent would undo it on arrival. Silence means "leave it
 * alone", and the writer then puts the resulting period into the URL anyway, so what you end up
 * looking at is still what the address bar says.
 */
export function syncPeriodWithUrl(): void {
  // URL → filters, on arrival and on every navigation.
  //
  // `untrack` around the body is load-bearing, not tidiness: comparing against `filters` *reads*
  // it, so without it this effect re-runs whenever the period changes — reads the URL as it
  // stood before the writer below had a chance to update it, finds a disagreement, and puts the
  // old period back. Two-way sync where each side also watches the other is a fight, and the
  // symptom is a select that snaps back the instant you change it.
  $effect(() => {
    const query = router.path.split("?")[1] ?? "";
    untrack(() => {
      const asked = periodFromUrl(new URLSearchParams(query));
      if (!asked) return;
      if (asked.range !== filters.range) filters.range = asked.range;
      const now = filters.custom;
      const next = asked.custom;
      if (next?.from !== now?.from || next?.to !== now?.to) filters.custom = next;
    });
  });

  // filters → URL. Replace rather than push: picking four ranges in a row should leave one
  // entry to go back from, not four.
  $effect(() => {
    const wanted = periodParams();
    untrack(() => {
      const have = new URLSearchParams(router.path.split("?")[1] ?? "");
      const differs = Object.entries(wanted).some(([k, v]) => (have.get(k) ?? null) !== v);
      if (differs) setQueryParams(wanted, { replace: true });
    });
  });
}
