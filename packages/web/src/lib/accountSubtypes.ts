/**
 * Curated per-accountable subtype lists, mirroring the reference Rails app's `SUBTYPES`
 * constants so our creation modals offer the same choices. Re-sync from (relative to the
 * repo's sibling checkout, ../../../sure-old):
 *   app/models/depository.rb   → DEPOSITORY_SUBTYPES
 *   app/models/property.rb     → PROPERTY_SUBTYPES
 *   app/models/loan.rb         → LOAN_SUBTYPES
 *   app/models/crypto.rb       → CRYPTO_SUBTYPES + TAX_TREATMENTS (its `tax_treatment` enum)
 *   app/models/investment.rb   → INVESTMENT_SUBTYPE_GROUPS + INVESTMENT_TAX_TREATMENT
 *     (its `SUBTYPES` rows and the `subtypes_grouped_for_select` helper's ordering)
 *   config/locales/views/accounts/en.yml → the group + tax-treatment label strings
 *
 * Labels are the reference's `long` variants, because that is what its forms render
 * (`v[:long]`); the `short` variants are only used by its compact badges.
 *
 * These lists live in the web layer on purpose: the backend stores a subtype as a free-form
 * `Option<String>` (like the reference's `subtype` string column), so the curated options and
 * their human labels are a UI concern. This module is deliberately a leaf — plain data plus
 * one lookup — so it can be imported from anywhere without cycles.
 */

export interface SubtypeOption {
  value: string;
  label: string;
}

export interface SubtypeGroup {
  label: string;
  options: SubtypeOption[];
}

/** Cash/bank accounts (their `Depository`). */
export const DEPOSITORY_SUBTYPES: SubtypeOption[] = [
  { value: "checking", label: "Checking" },
  { value: "savings", label: "Savings" },
  { value: "hsa", label: "Health Savings Account" },
  { value: "cd", label: "Certificate of Deposit" },
  { value: "money_market", label: "Money Market" },
];

/** Real estate (their `Property`). */
export const PROPERTY_SUBTYPES: SubtypeOption[] = [
  { value: "single_family_home", label: "Single Family Home" },
  { value: "multi_family_home", label: "Multi-Family Home" },
  { value: "condominium", label: "Condominium" },
  { value: "townhouse", label: "Townhouse" },
  { value: "investment_property", label: "Investment Property" },
  { value: "second_home", label: "Second Home" },
];

/** Borrowing (their `Loan`). */
export const LOAN_SUBTYPES: SubtypeOption[] = [
  { value: "mortgage", label: "Mortgage" },
  { value: "student", label: "Student Loan" },
  { value: "auto", label: "Auto Loan" },
  { value: "other", label: "Other Loan" },
];

/**
 * How crypto is held: a self-custody/synced wallet, or a centralised exchange. The
 * reference gates manual trade entry on `subtype == "exchange"`.
 */
export const CRYPTO_SUBTYPES: SubtypeOption[] = [
  { value: "wallet", label: "Crypto Wallet" },
  { value: "exchange", label: "Crypto Exchange" },
];

/**
 * The crypto-facing tax treatment select. Crypto is the only profile where the treatment is
 * user-chosen (default `taxable`); for investments it is derived from the subtype, see
 * {@link INVESTMENT_TAX_TREATMENT}. Note the derived side can also yield "tax_advantaged"
 * (labelled "Tax-Advantaged" by the reference's badge), which is intentionally not offered
 * here — it isn't in their `Crypto#tax_treatment` enum.
 */
export const TAX_TREATMENTS: SubtypeOption[] = [
  { value: "taxable", label: "Taxable" },
  { value: "tax_deferred", label: "Tax-Deferred" },
  { value: "tax_exempt", label: "Tax-Exempt" },
];

/** Region keys the reference groups investment subtypes by; `null` = available everywhere. */
type InvestmentRegion = "us" | "uk" | "ca" | "au" | "eu" | null;

/** A treatment value as stored/derived — mirrors their tax-treatment categories. */
type InvestmentTaxTreatment = "taxable" | "tax_deferred" | "tax_exempt" | "tax_advantaged";

interface InvestmentSubtype extends SubtypeOption {
  region: InvestmentRegion;
  taxTreatment: InvestmentTaxTreatment;
}

/**
 * Row-for-row transcription of `Investment::SUBTYPES`, kept in the reference's declaration
 * order. Both investment exports below are derived from this single list so the grouped
 * options and the tax-treatment map can never drift apart.
 */
const INVESTMENT_SUBTYPES: InvestmentSubtype[] = [
  // === United States ===
  { value: "brokerage", label: "Brokerage", region: "us", taxTreatment: "taxable" },
  { value: "401k", label: "401(k)", region: "us", taxTreatment: "tax_deferred" },
  { value: "roth_401k", label: "Roth 401(k)", region: "us", taxTreatment: "tax_exempt" },
  { value: "403b", label: "403(b)", region: "us", taxTreatment: "tax_deferred" },
  { value: "457b", label: "457(b)", region: "us", taxTreatment: "tax_deferred" },
  { value: "tsp", label: "Thrift Savings Plan", region: "us", taxTreatment: "tax_deferred" },
  { value: "ira", label: "Traditional IRA", region: "us", taxTreatment: "tax_deferred" },
  { value: "roth_ira", label: "Roth IRA", region: "us", taxTreatment: "tax_exempt" },
  { value: "sep_ira", label: "SEP IRA", region: "us", taxTreatment: "tax_deferred" },
  { value: "simple_ira", label: "SIMPLE IRA", region: "us", taxTreatment: "tax_deferred" },
  { value: "529_plan", label: "529 Education Savings Plan", region: "us", taxTreatment: "tax_advantaged" },
  { value: "hsa", label: "Health Savings Account", region: "us", taxTreatment: "tax_advantaged" },
  { value: "ugma", label: "UGMA Custodial Account", region: "us", taxTreatment: "taxable" },
  { value: "utma", label: "UTMA Custodial Account", region: "us", taxTreatment: "taxable" },

  // === United Kingdom ===
  { value: "isa", label: "Individual Savings Account", region: "uk", taxTreatment: "tax_exempt" },
  { value: "lisa", label: "Lifetime ISA", region: "uk", taxTreatment: "tax_exempt" },
  { value: "sipp", label: "Self-Invested Personal Pension", region: "uk", taxTreatment: "tax_deferred" },
  { value: "workplace_pension_uk", label: "Workplace Pension", region: "uk", taxTreatment: "tax_deferred" },

  // === Canada ===
  { value: "tfsa", label: "Tax-Free Savings Account", region: "ca", taxTreatment: "tax_exempt" },
  { value: "rrsp", label: "Registered Retirement Savings Plan", region: "ca", taxTreatment: "tax_deferred" },
  // Their key really is hyphenated here, unlike every other snake_case key.
  { value: "non-registered", label: "Non-Registered Investment Account", region: "ca", taxTreatment: "taxable" },
  { value: "fhsa", label: "First Home Savings Account", region: "ca", taxTreatment: "tax_exempt" },
  { value: "rdsp", label: "Registered Disability Savings Plan", region: "ca", taxTreatment: "tax_advantaged" },
  { value: "resp", label: "Registered Education Savings Plan", region: "ca", taxTreatment: "tax_advantaged" },
  { value: "dpsp", label: "Deferred Profit Sharing Plan", region: "ca", taxTreatment: "tax_deferred" },
  { value: "prpp", label: "Pooled Registered Pension Plan", region: "ca", taxTreatment: "tax_deferred" },
  { value: "lira", label: "Locked-In Retirement Account", region: "ca", taxTreatment: "tax_deferred" },
  { value: "rrif", label: "Registered Retirement Income Fund", region: "ca", taxTreatment: "tax_deferred" },
  { value: "lif", label: "Life Income Fund", region: "ca", taxTreatment: "tax_deferred" },
  { value: "lrif", label: "Locked-In Retirement Income Fund", region: "ca", taxTreatment: "tax_deferred" },
  { value: "prif", label: "Prescribed Registered Retirement Income Fund", region: "ca", taxTreatment: "tax_deferred" },
  { value: "rlif", label: "Restricted Life Income Fund", region: "ca", taxTreatment: "tax_deferred" },

  // === Australia ===
  { value: "super", label: "Superannuation", region: "au", taxTreatment: "tax_deferred" },
  { value: "smsf", label: "Self-Managed Super Fund", region: "au", taxTreatment: "tax_deferred" },

  // === Europe ===
  { value: "pea", label: "Plan d'Épargne en Actions", region: "eu", taxTreatment: "tax_advantaged" },
  { value: "pillar_3a", label: "Private Pension (Pillar 3a)", region: "eu", taxTreatment: "tax_deferred" },
  { value: "riester", label: "Riester-Rente", region: "eu", taxTreatment: "tax_deferred" },

  // === Generic (available everywhere) ===
  { value: "pension", label: "Pension", region: null, taxTreatment: "tax_deferred" },
  { value: "retirement", label: "Retirement Account", region: null, taxTreatment: "tax_deferred" },
  { value: "mutual_fund", label: "Mutual Fund", region: null, taxTreatment: "taxable" },
  { value: "angel", label: "Angel Investment", region: null, taxTreatment: "taxable" },
  { value: "trust", label: "Trust", region: null, taxTreatment: "taxable" },
  { value: "other", label: "Other Investment", region: null, taxTreatment: "taxable" },
];

/**
 * Group order. The reference's `subtypes_grouped_for_select` maps a currency to a region and
 * floats that region to the top, then the region-less "General" bucket, then the rest in
 * `us, uk, ca, au, eu` order. NZD isn't in its `CURRENCY_REGION_MAP`, so for our users it
 * always falls through to the no-currency case: General first, then the regions in declared
 * order. We hard-code that ordering rather than reimplement the currency shuffle — the
 * generic entries are the ones an NZ user actually wants first, and everything else stays a
 * scroll away instead of the list order changing under them.
 */
const REGION_GROUPS: { region: InvestmentRegion; label: string }[] = [
  { region: null, label: "General" },
  { region: "us", label: "United States" },
  { region: "uk", label: "United Kingdom" },
  { region: "ca", label: "Canada" },
  { region: "au", label: "Australia" },
  { region: "eu", label: "Europe" },
];

/** Investment subtypes as `<optgroup>`-ready groups, in the reference's order. */
export const INVESTMENT_SUBTYPE_GROUPS: SubtypeGroup[] = REGION_GROUPS.map(
  ({ region, label }) => ({
    label,
    options: INVESTMENT_SUBTYPES.filter((s) => s.region === region).map(({ value, label: l }) => ({
      value,
      label: l,
    })),
  }),
).filter((g) => g.options.length > 0);

/**
 * Tax treatment per investment subtype. The reference derives this from the subtype rather
 * than storing it (`Investment#tax_treatment`), defaulting to "taxable" when the subtype is
 * unknown or unset, so the UI can show a derived badge instead of asking the user.
 */
export const INVESTMENT_TAX_TREATMENT: Record<string, string> = Object.fromEntries(
  INVESTMENT_SUBTYPES.map((s) => [s.value, s.taxTreatment]),
);

/** Which curated list backs each metadata profile; profiles absent here have no subtypes. */
const LISTS: Record<string, SubtypeOption[]> = {
  depository: DEPOSITORY_SUBTYPES,
  property: PROPERTY_SUBTYPES,
  loan: LOAN_SUBTYPES,
  crypto: CRYPTO_SUBTYPES,
  // Both of our investment-shaped profiles pick from the same region-grouped list.
  shares: INVESTMENT_SUBTYPES,
  brokerage: INVESTMENT_SUBTYPES,
};

/**
 * Human label for a stored subtype, or `undefined` when it's blank or not one of the curated
 * options — callers should fall back to showing nothing rather than a raw key, since stored
 * subtypes are free-form strings (a provider sync or an older list could yield anything).
 */
export function subtypeLabel(
  profile: string,
  value: string | undefined | null,
): string | undefined {
  const key = value?.trim();
  if (!key) return undefined;
  return LISTS[profile]?.find((o) => o.value === key)?.label;
}
