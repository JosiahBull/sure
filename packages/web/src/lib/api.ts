import { createSureClient, type Schemas } from "@sure/client";

/** Same-origin typed API client (dev proxies /api to the backend). */
export const api = createSureClient("/");

export type { Schemas };

/** Format signed minor units as a full currency string. */
export function formatMoney(minor: number, currency = "NZD", decimals = 2): string {
  const major = minor / 10 ** decimals;
  try {
    return new Intl.NumberFormat("en-NZ", {
      style: "currency",
      currency,
      maximumFractionDigits: decimals,
    }).format(major);
  } catch {
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

/** Deterministic color for a category id (stable across renders). */
const PALETTE = [
  "#e99537", "#4da568", "#6471eb", "#db5a54", "#df4e92",
  "#c44fe9", "#eb5429", "#61c9ea", "#805dee", "#6ad28a",
];
export function colorFor(key: string | number | null | undefined, fallbackIndex = 0): string {
  if (key === null || key === undefined) return "#737373"; // Category::UNCATEGORIZED_COLOR
  const s = String(key);
  let h = fallbackIndex;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return PALETTE[h % PALETTE.length];
}
