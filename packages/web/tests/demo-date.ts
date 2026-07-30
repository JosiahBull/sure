/**
 * The one instant the whole visual suite pretends it is.
 *
 * Screenshots can only be byte-identical across days if nothing in them is derived from
 * the real clock, and three separate things were:
 *   1. the seeded data (`scripts/seed.mjs` dates every account/transaction relative to
 *      "today") — pinned by passing `SEED_TODAY` in global-setup;
 *   2. the SPA's own `new Date()` — chiefly `rangeDates()` in src/lib/state.svelte.ts,
 *      which resolves the default "Last 12 months" filter and the net-worth chart's axis
 *      labels — pinned by the fixed browser clock in fixtures.ts;
 *   3. `rule_runs.created_at`, stamped by SQLite's `strftime('now')` when the seed runs
 *      the rules, which nothing outside the database can set — masked in app.spec.ts.
 *
 * (1) and (2) have to agree: pin the seed alone and the live 12-month window would slide
 * off the data as real time passed; pin the clock alone and the seeded dates would drift.
 *
 * Mid-month so no `monthsAgo(n, day)` offset can land on a day the target month lacks.
 */
export const DEMO_TODAY = "2026-07-15";

/**
 * The same day as an instant for the browser clock — midnight UTC, which is midday in the
 * config's pinned `Pacific/Auckland` (UTC+12 in July; NZ has no DST then). Deliberately not
 * midday UTC: that is *midnight* in NZ, so the app's local-time reads would land on the
 * 16th while its UTC reads stayed on the 15th. Still deterministic, but needlessly perched
 * on a date boundary, and confusing against the seeded dates.
 */
export const DEMO_NOW = new Date(`${DEMO_TODAY}T00:00:00Z`);

/**
 * `DEMO_TODAY` in the same form the app's `formatDate` renders — the value the audit log's
 * database-stamped "When" column is rewritten to before it's photographed (see app.spec.ts).
 */
export const DEMO_WHEN = new Intl.DateTimeFormat("en-NZ", {
  day: "numeric",
  month: "short",
  year: "numeric",
  timeZone: "Pacific/Auckland",
}).format(DEMO_NOW);
