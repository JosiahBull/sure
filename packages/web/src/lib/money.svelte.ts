/**
 * The household's base currency, as every money figure in the app needs to know it.
 *
 * Deliberately a leaf: this module imports nothing. `api.ts` reads it (that is what
 * `formatMoney` is for) and `settings.svelte.ts` writes it, so putting the fetch in here too
 * would make `api.ts` and this file import each other. A cycle would work right up until the
 * evaluation order changed under a bundler.
 *
 * Why it needs to be reactive rather than read once: the base currency is a setting, and
 * changing it on the Preferences page has to re-render the figures on every other page.
 */

/** Matches the server's own fallback — `settings.base_currency_code` defaults to NZD. */
const FALLBACK = "NZD";

let code = $state(FALLBACK);

/** The configured base currency, uppercased. Reactive: reading it in a `$derived` tracks it. */
export function baseCurrency(): string {
  return code;
}

/**
 * Point the formatters at a different base currency.
 *
 * Called by the loader on startup and by the Preferences page the moment the setting is saved,
 * so the change reaches the rest of the app without a reload.
 */
export function setBaseCurrency(next: string | null | undefined): void {
  code = (next ?? FALLBACK).toUpperCase();
}

/** Whether a currency code is the base one, tolerating case and an absent value. */
export function isBaseCurrency(currency: string | null | undefined): boolean {
  return (currency ?? code).toUpperCase() === code;
}
