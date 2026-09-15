import { createSureClient, type Schemas } from "@sure/client";

import { baseCurrency, isBaseCurrency } from "./money.svelte";

/** Same-origin typed API client (dev proxies /api to the backend). */
export const api = createSureClient("/");

export type { Schemas };

/**
 * Format signed minor units as a currency string.
 *
 * The base currency gets a bare symbol — `$1,652.59` — because it is the one the reader is
 * already thinking in, and a prefix on every figure is noise on the overwhelming majority of a
 * page. Everything else gets its ISO code — `USD 1,652.59` — which is the only form that
 * *always* disambiguates: the narrow symbol for USD, NZD and AUD is the same "$", so a symbol
 * would silently read as the base currency whenever the base is another dollar.
 *
 * Both halves come from `Intl`, so grouping, the decimal mark and the placement of a minus sign
 * stay whatever the currency and locale say they are.
 */
export function formatMoney(
  minor: number,
  currency = baseCurrency(),
  decimals = 2,
): string {
  const major = minor / 10 ** decimals;
  try {
    return new Intl.NumberFormat("en-US", {
      style: "currency",
      currency,
      // The whole rule, in one option.
      currencyDisplay: isBaseCurrency(currency) ? "narrowSymbol" : "code",
      maximumFractionDigits: decimals,
    }).format(major);
  } catch {
    // An unknown or malformed code — `Intl` throws on those rather than guessing.
    return `${currency} ${major.toFixed(decimals)}`;
  }
}

/** Compact money for axes/labels, e.g. $1.2M, -$4k. */
export function formatShort(minor: number, decimals = 2): string {
  const major = minor / 10 ** decimals;
  const sign = major < 0 ? "-" : "";
  const abs = Math.abs(major);
  if (abs >= 1_000_000) return `${sign}$${(abs / 1_000_000).toFixed(abs >= 10_000_000 ? 0 : 1)}M`;
  if (abs >= 1_000) return `${sign}$${(abs / 1_000).toFixed(abs >= 100_000 ? 0 : 1)}k`;
  return `${sign}$${abs.toFixed(0)}`;
}

export function formatDate(iso: string): string {
  const d = new Date(iso.length <= 10 ? `${iso}T00:00:00` : iso);
  if (isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-NZ", { day: "numeric", month: "short", year: "numeric" });
}

/** Long, day-group heading form, e.g. "June 30, 2026" (the reference's :long format). */
export function formatDateLong(iso: string): string {
  const d = new Date(iso.length <= 10 ? `${iso}T00:00:00` : iso);
  if (isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric" });
}

/** Unwrap an openapi-fetch result, throwing a readable error on failure. */
export async function unwrap<T>(p: Promise<{ data?: T; error?: unknown }>): Promise<T> {
  const { data, error } = await p;
  if (error !== undefined || data === undefined) {
    const msg =
      (error as { error?: { message?: string } })?.error?.message ??
      (typeof error === "string" ? error : "Request failed");
    throw new Error(msg);
  }
  return data;
}

// Category colour lives in ./color alongside the depth-shading the category hierarchy
// needs; re-exported here so the existing `from "../api"` import sites keep working.
export { colorFor } from "./color";
