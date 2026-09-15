import { api } from "./api";
import { setBaseCurrency } from "./money.svelte";

/**
 * The server settings the SPA needs before it can render a figure correctly.
 *
 * Only the base currency so far, and it is loaded here rather than by whichever page happens to
 * want it because *every* page wants it: `formatMoney` decides between a bare `$` and an ISO
 * code on it, so a page that rendered before this resolved would show the fallback and then
 * reflow. One request per visit, from the shell.
 *
 * Kept apart from `money.svelte.ts` on purpose — that module holds the state and is imported by
 * `api.ts`, so a fetch living there would have the two importing each other.
 */

let started = false;

/** Load once per page load. Safe to call from every component that might be the first to mount. */
export function ensureSettingsLoaded(): void {
  if (started) return;
  started = true;
  void refreshSettings();
}

/**
 * Re-read the settings and republish them.
 *
 * Failure is silent and leaves the fallback in place: the base currency decides how a number is
 * *labelled*, never what it is, so a server that did not answer should cost a prefix rather than
 * an error banner over a page of otherwise correct figures.
 */
export async function refreshSettings(): Promise<void> {
  const { data } = await api.GET("/api/settings", {});
  if (data) setBaseCurrency(data.base_currency_code);
}
