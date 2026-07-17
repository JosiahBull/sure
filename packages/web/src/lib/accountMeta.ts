// Data-driven description of per-kind account metadata. This mirrors the typed
// `AccountMetadata` union in the backend (packages/core/src/types.rs): the field keys
// here must match the Rust struct field names, and the profiles must match
// `AccountMetadata::profile_for`. The form (AccountForm.svelte) iterates these
// descriptors so there is one source of truth for the UI.
import type { Schemas } from "./api";

export type MetaProfile =
  | "depository"
  | "property"
  | "mortgage"
  | "loan"
  | "vehicle"
  | "shares"
  | "generic";

export type MetaFieldType =
  | "text"
  | "url"
  | "textarea"
  | "money"
  | "percent"
  | "int"
  | "date"
  | "select";

export interface MetaField {
  key: string;
  label: string;
  type: MetaFieldType;
  placeholder?: string;
  options?: { value: string; label: string }[];
}

/** Selectable account kinds with their display labels. */
export const KINDS: { value: Schemas["AccountKind"]; label: string }[] = [
  { value: "bank", label: "Bank" },
  { value: "cash", label: "Cash" },
  { value: "savings", label: "Savings" },
  { value: "credit_card", label: "Credit card" },
  { value: "revolving_credit", label: "Revolving credit" },
  { value: "mortgage", label: "Mortgage" },
  { value: "student_loan", label: "Student loan" },
  { value: "loan", label: "Loan" },
  { value: "vehicle", label: "Vehicle" },
  { value: "real_estate", label: "Real estate" },
  { value: "shares_nz", label: "Shares (NZ)" },
  { value: "shares_us", label: "Shares (US)" },
  { value: "shares_private", label: "Shares (private)" },
  { value: "asset", label: "Other asset" },
  { value: "liability", label: "Other liability" },
];

export const kindLabel = (k: string) => KINDS.find((x) => x.value === k)?.label ?? k;

/** Liability kinds can be secured against an asset (e.g. a mortgage against a home). */
const LIABILITY_KINDS = new Set([
  "credit_card",
  "revolving_credit",
  "mortgage",
  "student_loan",
  "loan",
  "liability",
]);
export const isLiabilityKind = (k: string) => LIABILITY_KINDS.has(k);

/**
 * Whether the top-level `institution` field applies. Only depository accounts held at a
 * bank/lender (and cash isn't one) use it — loans carry a `lender`, shares a `broker`,
 * and physical assets (property, vehicle) have no institution at all.
 */
export const showsInstitution = (k: string) => kindToProfile(k) === "depository" && k !== "cash";

/** Only credit_card/revolving_credit accounts track a credit limit. */
export const showsCreditLimit = (k: string) => k === "credit_card" || k === "revolving_credit";

/**
 * Remaining borrowing power (credit limit minus what's currently owed), if this account
 * tracks a credit limit and one is known — `null` otherwise (a different kind, or a
 * limit that hasn't been set/synced yet). `valueMinor` is the account's current balance
 * (negative when money is owed, per this app's sign convention).
 */
export function remainingBorrowing(
  kind: string,
  metadata: Schemas["AccountMetadata"] | null | undefined,
  valueMinor: number,
): number | null {
  if (!showsCreditLimit(kind)) return null;
  const limit = (metadata as Record<string, unknown> | null | undefined)?.credit_limit_minor;
  return typeof limit === "number" ? limit + valueMinor : null;
}

/**
 * Percentage of a mortgage/loan's original borrowed amount that's been paid down so far
 * — `null` if this isn't a loan-shaped kind, or the original amount isn't known yet
 * (nothing synced/entered). `valueMinor` is the account's current balance (negative when
 * money is owed); the amount paid down is `original − |valueMinor|`.
 */
export function loanPaidOffPct(
  kind: string,
  metadata: Schemas["AccountMetadata"] | null | undefined,
  valueMinor: number,
): number | null {
  if (kindToProfile(kind) !== "mortgage" && kindToProfile(kind) !== "loan") return null;
  const original = (metadata as Record<string, unknown> | null | undefined)?.original_amount_minor;
  if (typeof original !== "number" || original <= 0) return null;
  const paid = original - Math.abs(valueMinor);
  return Math.max(0, Math.min(100, (paid / original) * 100));
}

/** Map an account kind to its metadata profile (mirrors the backend). */
export function kindToProfile(kind: string): MetaProfile {
  switch (kind) {
    case "real_estate":
      return "property";
    case "mortgage":
      return "mortgage";
    case "loan":
    case "student_loan":
      return "loan";
    case "vehicle":
      return "vehicle";
    case "shares_nz":
    case "shares_us":
    case "shares_private":
      return "shares";
    case "asset":
    case "liability":
      return "generic";
    default:
      return "depository"; // cash / bank / savings / credit_card / revolving_credit
  }
}

const URL_FIELD: MetaField = { key: "url", label: "Link", type: "url", placeholder: "https://…" };
const NOTES_FIELD: MetaField = { key: "notes", label: "Notes", type: "textarea" };
const RATE_TYPE_FIELD: MetaField = {
  key: "rate_type",
  label: "Rate type",
  type: "select",
  options: [
    { value: "", label: "—" },
    { value: "fixed", label: "Fixed" },
    { value: "floating", label: "Floating" },
    { value: "split", label: "Split" },
  ],
};

/** Ordered metadata fields per profile. Keys match the backend struct field names. */
export const FIELDS: Record<MetaProfile, MetaField[]> = {
  depository: [
    { key: "account_number", label: "Account number", type: "text" },
    // Only rendered for credit_card/revolving_credit — see `showsCreditLimit` — but kept
    // in the generic depository field list so `buildMetadata`/`metadataToRaw` round-trip
    // it for free like every other field here.
    { key: "credit_limit_minor", label: "Credit limit", type: "money" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  property: [
    { key: "address", label: "Address", type: "text" },
    { key: "purchase_date", label: "Purchase date", type: "date" },
    { key: "purchase_price_minor", label: "Purchase price", type: "money" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  mortgage: [
    { key: "lender", label: "Lender", type: "text" },
    { key: "original_amount_minor", label: "Original amount borrowed", type: "money" },
    { key: "interest_rate_bps", label: "Interest rate (%)", type: "percent", placeholder: "5.49" },
    RATE_TYPE_FIELD,
    { key: "fixed_until", label: "Fixed until", type: "date" },
    { key: "fixed_term_months", label: "Fixed term (months)", type: "int" },
    { key: "term_months", label: "Overall term (months)", type: "int" },
    { key: "start_date", label: "Start date", type: "date" },
    { key: "interest_paid_minor", label: "Interest paid so far", type: "money" },
    { key: "capital_paid_minor", label: "Capital paid so far", type: "money" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  loan: [
    { key: "lender", label: "Lender", type: "text" },
    { key: "original_amount_minor", label: "Original amount borrowed", type: "money" },
    { key: "interest_rate_bps", label: "Interest rate (%)", type: "percent", placeholder: "8.90" },
    { key: "term_months", label: "Term (months)", type: "int" },
    { key: "start_date", label: "Start date", type: "date" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  vehicle: [
    { key: "make", label: "Make", type: "text" },
    { key: "model", label: "Model", type: "text" },
    { key: "year", label: "Year", type: "int" },
    { key: "plate", label: "Plate", type: "text" },
    { key: "nickname", label: "Nickname", type: "text" },
    { key: "vin", label: "VIN", type: "text" },
    { key: "purchase_date", label: "Purchase date", type: "date" },
    { key: "sale_date", label: "Sale date", type: "date" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  shares: [
    { key: "broker", label: "Broker / platform", type: "text" },
    { key: "ticker", label: "Ticker", type: "text" },
    { key: "exchange", label: "Exchange", type: "text" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  generic: [URL_FIELD, NOTES_FIELD],
};

/** Convert an interest rate in basis points to a display percentage, e.g. 549 → "5.49%". */
export const formatRateBps = (bps: number) => `${(bps / 100).toFixed(2).replace(/\.?0+$/, "")}%`;

/**
 * Build a metadata object for POST/PUT from the form's raw string inputs, converting
 * money → minor units, percent → basis points, and ints, and dropping empty fields.
 */
export function buildMetadata(kind: string, raw: Record<string, string>): Schemas["AccountMetadata"] {
  const profile = kindToProfile(kind);
  const out: Record<string, unknown> = { profile };
  for (const f of FIELDS[profile]) {
    const v = (raw[f.key] ?? "").trim();
    if (!v) continue;
    if (f.type === "money" || f.type === "percent") {
      const n = parseFloat(v.replace(/[^0-9.-]/g, ""));
      if (!isNaN(n)) out[f.key] = Math.round(n * 100);
    } else if (f.type === "int") {
      const n = parseInt(v, 10);
      if (!isNaN(n)) out[f.key] = n;
    } else {
      out[f.key] = v;
    }
  }
  return out as Schemas["AccountMetadata"];
}

/** Inverse of {@link buildMetadata}: prime the form's raw inputs from stored metadata. */
export function metadataToRaw(metadata: Schemas["AccountMetadata"] | null | undefined): Record<string, string> {
  const raw: Record<string, string> = {};
  if (!metadata) return raw;
  const m = metadata as Record<string, unknown>;
  const profile = (m.profile as MetaProfile) ?? "depository";
  for (const f of FIELDS[profile] ?? []) {
    const v = m[f.key];
    if (v === undefined || v === null) continue;
    if (f.type === "money" || f.type === "percent") {
      raw[f.key] = String(Number(v) / 100);
    } else {
      raw[f.key] = String(v);
    }
  }
  return raw;
}

/** A short, human-friendly one-line summary of an account's metadata for list rows. */
export function metaSummary(kind: string, metadata: Schemas["AccountMetadata"] | null | undefined): string {
  if (!metadata) return "";
  const m = metadata as Record<string, unknown>;
  const bits: string[] = [];
  const s = (k: string) => (typeof m[k] === "string" ? (m[k] as string) : undefined);
  const n = (k: string) => (typeof m[k] === "number" ? (m[k] as number) : undefined);
  switch (kindToProfile(kind)) {
    case "property":
      if (s("address")) bits.push(s("address")!);
      break;
    case "vehicle": {
      const desc = [s("make"), s("model"), n("year")].filter(Boolean).join(" ");
      if (s("nickname")) bits.push(`“${s("nickname")}”`);
      if (desc) bits.push(desc);
      if (s("plate")) bits.push(s("plate")!);
      break;
    }
    case "mortgage":
    case "loan":
      // Loans have no top-level institution; the lender stands in for it here.
      if (s("lender")) bits.push(s("lender")!);
      if (n("interest_rate_bps") !== undefined) bits.push(formatRateBps(n("interest_rate_bps")!));
      break;
    case "shares":
      // Shares have no top-level institution; the broker/platform stands in for it here.
      if (s("broker")) bits.push(s("broker")!);
      if (s("ticker")) bits.push(s("ticker")!);
      break;
    case "depository":
      if (s("account_number")) bits.push(s("account_number")!);
      break;
    default:
      break;
  }
  return bits.join(" · ");
}
