// Data-driven description of per-kind account metadata. This mirrors the typed
// `AccountMetadata` union in the backend (packages/core/src/types.rs): the field keys
// here must match the Rust struct field names, and the profiles must match
// `AccountMetadata::profile_for`. The form (AccountForm.svelte) iterates these
// descriptors so there is one source of truth for the UI.
//
// The `required` marks likewise mirror the backend's requirement tables — `PROFILE_REQUIRED`
// and `KIND_REQUIRED`, enforced by `AccountMetadata::validate_for` — plus the account-level
// rules in packages/dal/src/accounts.rs (`INSTITUTION_REQUIRED`, `opening_balance_problems`),
// re-stated here as `requiresInstitution` / `requiresOpeningBalance`. The server is the
// authority: it answers a save that misses any of them with a 422 naming every missing field.
// So the two tables have to stay in step in both directions — a field required there but
// unmarked here is a rejection the form never warned about, and one marked here but not there
// is a field we nag about for no reason.
import type { Schemas } from "./api";
import {
  CRYPTO_SUBTYPES,
  DEPOSITORY_SUBTYPES,
  INVESTMENT_SUBTYPE_GROUPS,
  LOAN_SUBTYPES,
  PROPERTY_SUBTYPES,
  TAX_TREATMENTS,
  subtypeLabel,
  type SubtypeGroup,
  type SubtypeOption,
} from "./accountSubtypes";

export type MetaProfile =
  | "depository"
  | "property"
  | "mortgage"
  | "loan"
  | "student_loan"
  | "vehicle"
  | "shares"
  | "brokerage"
  | "crypto"
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

/**
 * The two numeric shapes {@link valueProblem} checks, mirroring the backend's `Required`
 * enum (`packages/core/src/types.rs`) — but only the members whose problem a blank/non-blank
 * client check would miss:
 *
 * - `"amount"`: a money amount in minor units. Must be strictly positive once present — 0
 *   is a placeholder, not an answer (matches `Required::Amount`; also covers
 *   `Required::Count`, e.g. a model year, which is the same "must be > 0" rule on an `int`
 *   field instead of a `money` one).
 * - `"bps"`: a rate in basis points. 0 is legitimate (an interest-free loan); only a
 *   negative one is rejected (matches `Required::Bps`).
 *
 * There is no `"text"`/`"choice"` member: `Required::Text` and `Required::Choice` check
 * only presence, which {@link isFieldRequired} already covers — a field with no
 * `valueRule` needs nothing checked beyond that.
 *
 * packages/core/src/types.rs's `PROFILE_REQUIRED`/`KIND_REQUIRED` tables (via
 * `Required::problem`) are the source of truth: a field marked here must be listed there
 * with the matching `Required` variant, and vice versa, or the two tables can silently
 * drift — a value the client accepts and the server 422s, or the other way round.
 */
export type ValueRule = "amount" | "bps" | "count";

export interface MetaField {
  key: string;
  label: string;
  type: MetaFieldType;
  placeholder?: string;
  /** Flat `<option>` list for a `select`. Mutually exclusive with {@link groups}. */
  options?: SubtypeOption[];
  /**
   * Grouped `<optgroup>` list for a `select` — used by the (long, region-grouped)
   * investment subtypes, where a flat list would be unusable. Mutually exclusive with
   * {@link options}.
   */
  groups?: SubtypeGroup[];
  /**
   * Which part of the form the field belongs to. Defaults to `"main"`; `"details"` fields
   * are tucked inside the collapsed "Additional details" disclosure so the common path
   * (name, balance, type-specific essentials) stays short — this mirrors the reference
   * app's per-accountable modals.
   */
  section?: "main" | "details";
  /**
   * One line of explanatory copy rendered under the control, for the rare field whose
   * "right" answer isn't obvious from its label and options alone — e.g. crypto's tax
   * treatment, where the answer depends on the wrapper the coins sit in, not the coin
   * itself. Most fields don't need one and leave this unset.
   */
  hint?: string;
  /**
   * Restrict the field to these account kinds. Several kinds share one metadata profile
   * (e.g. cash/bank/savings/credit_card are all `depository`) but not every field applies
   * to all of them — an APR only makes sense on a card. Unset means "every kind that uses
   * this profile".
   */
  kinds?: string[];
  /**
   * Whether the backend refuses to store the account without this field. `true` = required
   * for every kind that uses the profile; an array = required only for those kinds (the same
   * shape as {@link kinds}, e.g. a credit limit on the shared `depository` profile, which only
   * a card has to supply). Unset means optional.
   *
   * Ask {@link isFieldRequired} rather than reading this directly — it also accounts for
   * {@link kinds}, since a field the kind never shows cannot be required of it.
   *
   * The same table is enforced server-side by `AccountMetadata::validate_for`
   * (packages/core/src/types.rs), which is what actually rejects an incomplete save; these
   * marks exist so the form can say so *before* the round trip, and must stay in step with it.
   */
  required?: boolean | string[];
  /**
   * Narrows {@link required} to the case where a sibling field currently holds one of
   * these values — a field can be unanswerable rather than merely unanswered. A floating
   * mortgage has no rate expiry and nothing to refix to, so asking for either would be
   * asking for a number that does not exist.
   *
   * Mirrors the `REFIX_REQUIRED` branch of `AccountMetadata::validate_for`
   * (packages/core/src/types.rs), which is what actually enforces it.
   */
  requiredWhen?: { key: string; equals: string[] };
  /**
   * How this field's *value* is checked once it is present, independent of whether it's
   * required at all (that's {@link required}/{@link isFieldRequired} — a blank value is
   * never a `valueRule` problem, only a required-ness one). Mirrors the shape checks in the
   * backend's `Required` enum (`packages/core/src/types.rs`) field-for-field — see
   * {@link valueProblem} for the exact semantics of each member, and keep the two in step:
   * this is a pre-flight for the same rule the server actually enforces, not a second,
   * independent opinion about what's valid.
   *
   * Unset (the common case — every `text`/`date`/`select` field, and every optional
   * `money`/`percent`/`int` one) means "no shape beyond blank/non-blank", which matches
   * `Required::Text`/`Required::Choice` and every field the server places no `Required`
   * entry on at all.
   */
  valueRule?: ValueRule;
  /**
   * Value submitted when the input is left blank. Only meaningful for a required `select`,
   * which has no blank option (see {@link NONE_OPTION}) and therefore displays its first
   * option whether or not the user has touched it: naming that value here keeps the payload
   * in step with what is on screen instead of sending nothing and being told the field is
   * required. Selects with no conventional answer — a mortgage's rate type, a property's
   * subtype — deliberately have none, because a default there would be a guess stored as
   * fact; leaving it unset means an unanswered field comes back as a 422 that names it.
   */
  default?: string;
  /** Forwarded to `<input type="number">` for `int` fields (e.g. a plausible year range). */
  min?: number;
  max?: number;
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
  { value: "brokerage", label: "Brokerage" },
  { value: "crypto", label: "Crypto" },
  { value: "asset", label: "Other asset" },
  { value: "liability", label: "Other liability" },
];

export const kindLabel = (k: string) => KINDS.find((x) => x.value === k)?.label ?? k;

/**
 * Per-kind glyph + accent colour, so a kind looks the same everywhere it's shown as a
 * visual token (currently the New-account picker). Colours are the reference app's
 * accountable palette (see ../../../sure-old/app/models/*.rb `color`): one hue per family
 * — purple for cash-like, blue for investments, cyan property, pink vehicles, red credit,
 * magenta borrowing, green/grey for the catch-alls. Icon names must exist in icons.ts.
 */
export const KIND_STYLE: Record<Schemas["AccountKind"], { icon: string; color: string }> = {
  bank: { icon: "landmark", color: "#875BF7" },
  cash: { icon: "banknote", color: "#875BF7" },
  savings: { icon: "piggy-bank", color: "#875BF7" },
  brokerage: { icon: "chart-line", color: "#1570EF" },
  shares_nz: { icon: "trending-up", color: "#1570EF" },
  shares_us: { icon: "trending-up", color: "#1570EF" },
  shares_private: { icon: "building-2", color: "#1570EF" },
  // Crypto is an investment but the reference gives it grey rather than the investment
  // blue, so it reads as its own thing next to shares/brokerage.
  crypto: { icon: "bitcoin", color: "#737373" },
  real_estate: { icon: "home", color: "#06AED4" },
  vehicle: { icon: "car-front", color: "#F23E94" },
  asset: { icon: "plus", color: "#12B76A" },
  credit_card: { icon: "credit-card", color: "#F13636" },
  revolving_credit: { icon: "repeat", color: "#F13636" },
  // A mortgage is borrowing (magenta, like the other loans) against a home (the house glyph).
  mortgage: { icon: "home", color: "#D444F1" },
  student_loan: { icon: "graduation-cap", color: "#D444F1" },
  loan: { icon: "hand-coins", color: "#D444F1" },
  liability: { icon: "minus", color: "#737373" },
};

export const kindStyle = (k: string) =>
  KIND_STYLE[k as Schemas["AccountKind"]] ?? { icon: "wallet", color: "#737373" };

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
 * Whether an account's `institution` is worth its own place outside the "Additional
 * details" disclosure — the main-row/list-row question. Only depository accounts held at a
 * bank/lender (and cash isn't one) earn that prominence: it's effectively part of the
 * account's identity there (two cards both called "Visa" are told apart by who issued
 * them), which isn't true of a loan's `lender`, a broker's `broker`, or a crypto wallet's
 * exchange — those already have their own, more specific field. Used by the accounts list
 * row (`Accounts.svelte`) and the provider-linking row (`ProviderConnectModal.svelte`) to
 * decide what to show next to the account name; **not** what decides whether the account
 * *form* offers an institution input at all — that's {@link offersInstitution}, a
 * different question this predicate used to be asked in place of.
 */
export const showsInstitution = (k: string) => kindToProfile(k) === "depository" && k !== "cash";

/**
 * Whether the account form should offer an institution input at all, regardless of
 * whether it's prominent enough for a list row (see {@link showsInstitution}, the
 * narrower and unrelated question). The reference keeps `institution_name` /
 * `institution_domain` on every accountable's shared form partial (`accounts/_form`),
 * tucked in "Additional details" alongside notes, no matter the kind — a property, a
 * vehicle, a crypto wallet can all usefully say who holds them, even though none of them
 * are worth a dedicated institution column on a list row. So this is `true` for every
 * kind; kept as its own predicate (rather than folded into `showsInstitution`, or that
 * function widened to cover both jobs) so the two questions can't be conflated again the
 * way they were before this existed — one is this file's layout choice for a list
 * row, the other is the form's.
 */
export const offersInstitution = (_k: string) => true;

/**
 * The kinds whose institution the backend *demands* (`INSTITUTION_REQUIRED` in
 * packages/dal/src/accounts.rs): the bank is part of identifying the account at all, since two
 * cards both called "Visa" are told apart by who issued them.
 */
const INSTITUTION_REQUIRED = ["bank", "savings", "credit_card", "revolving_credit"];

/**
 * Whether `institution` is required for this kind — a stricter question than
 * {@link offersInstitution}, which only asks whether to render the input at all. The two sets
 * don't coincide: every kind offers the field, but only these four demand it be filled in. They
 * answer to different tables — one to this file's layout choices, the other to the server's
 * validation — so they're kept apart rather than one being defined as the other.
 */
export const requiresInstitution = (k: string) => INSTITUTION_REQUIRED.includes(k);

/**
 * Whether creating this kind requires an opening balance (and its date). Every kind does except
 * `brokerage`, whose value is computed from its holdings ledger, so seeding one would
 * double-count — mirrors `opening_balance_problems` in packages/dal/src/accounts.rs. Only
 * relevant on create: the server refuses an opening balance on an update, where the balance is
 * maintained through transactions and valuations instead.
 */
export const requiresOpeningBalance = (k: string) => k !== "brokerage";

/**
 * The kinds a bank transaction-export CSV can be imported into — mirrors `accepts_asb_csv`
 * in packages/api/src/routes/asb.rs, which is the authority; this only decides whether the
 * accounts list offers the panel. The rest either have no bank statement (a property, a
 * share holding) or have an importer of their own (a student loan's myIR export).
 */
const BANK_CSV_KINDS = ["cash", "bank", "savings", "credit_card", "revolving_credit"];

/** Whether this kind can take a bank transaction-export CSV (see {@link BANK_CSV_KINDS}). */
export const takesBankCsv = (k: string) => BANK_CSV_KINDS.includes(k);

/** The depository kinds that behave like a card: a limit, an APR, a minimum payment. */
const CARD_KINDS = ["credit_card", "revolving_credit"];

/**
 * The depository kinds that are genuine deposit accounts. The reference keeps these and cards as
 * separate accountables, and only its `depositories/_form` renders a subtype select — a card's
 * one subtype is "credit_card", i.e. the kind restated — so the curated checking/savings/HSA/…
 * list is offered to these kinds alone rather than to every kind sharing the profile.
 */
const DEPOSIT_KINDS = ["cash", "bank", "savings"];

/**
 * The share kinds that trade on a public market, and so can be priced automatically from a
 * (ticker, exchange) pair — which is why both are required of them. `shares_private` is the
 * exception the split exists for: an unlisted holding has neither.
 */
const LISTED_SHARES_KINDS = ["shares_nz", "shares_us"];

/** Only credit_card/revolving_credit accounts track a credit limit. */
export const showsCreditLimit = (k: string) => CARD_KINDS.includes(k);

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
 *
 * A `student_loan` is deliberately not loan-shaped for this purpose: its profile has no
 * original amount, because an income-contingent loan is drawn down over years of study and
 * never had one. It therefore shows no repaid badge at all rather than a percentage of an
 * invented principal — which is what it used to do, pinned at 0% for as long as the balance
 * was still climbing.
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
      return "loan";
    // Its own profile, not the `loan` one: an income-contingent student loan has no
    // principal, term or schedule, so those fields do not exist on it. See
    // `StudentLoanMeta` in packages/core/src/types.rs.
    case "student_loan":
      return "student_loan";
    case "vehicle":
      return "vehicle";
    case "shares_nz":
    case "shares_us":
    case "shares_private":
      return "shares";
    case "brokerage":
      return "brokerage";
    case "crypto":
      return "crypto";
    case "asset":
    case "liability":
      return "generic";
    default:
      return "depository"; // cash / bank / savings / credit_card / revolving_credit
  }
}

const URL_FIELD: MetaField = {
  key: "url",
  label: "Link",
  type: "url",
  placeholder: "https://…",
  section: "details",
};
const NOTES_FIELD: MetaField = {
  key: "notes",
  label: "Notes",
  type: "textarea",
  placeholder:
    "Store additional information like account numbers, sort codes, IBAN, routing numbers, etc.",
  section: "details",
};
const RATE_TYPE_OPTIONS: SubtypeOption[] = [
  { value: "fixed", label: "Fixed" },
  { value: "floating", label: "Floating" },
  { value: "split", label: "Split" },
];
/**
 * Rate type as a loan asks it: optional, so it keeps a blank option. A mortgage's is required
 * (see {@link REQUIRED_RATE_TYPE_FIELD}) — nothing about a repayment schedule can be said
 * without knowing whether the rate can move — hence the two variants of the same field.
 */
const RATE_TYPE_FIELD: MetaField = {
  key: "rate_type",
  label: "Rate type",
  type: "select",
  options: [{ value: "", label: "—" }, ...RATE_TYPE_OPTIONS],
};
const REQUIRED_RATE_TYPE_FIELD: MetaField = {
  ...RATE_TYPE_FIELD,
  options: RATE_TYPE_OPTIONS,
  required: true,
};

/** The rate types that expire, and so need something to roll off onto. */
const REFIXING_RATE_TYPES = ["fixed", "split"];

/**
 * What happens when a fixed rate expires — the single largest uncertainty in a long-horizon
 * projection, and the reason the forecast can put an honest band around a mortgage instead
 * of drawing one confident line to the end of the term.
 *
 * Required only for a rate type that actually expires: a floating loan has no roll-off date
 * and nothing to refix to. Mirrors `REFIX_REQUIRED` in packages/core/src/types.rs.
 */
function refixFields(kind: string): MetaField[] {
  const when = { key: "rate_type", equals: REFIXING_RATE_TYPES };
  return [
    {
      key: "fixed_until",
      label: "Fixed until",
      type: "date",
      required: [kind],
      requiredWhen: when,
    },
    {
      key: "refix_rate_bps",
      label: "Assumed rate after refix (%)",
      type: "percent",
      placeholder: "4.49",
      required: [kind],
      requiredWhen: when,
      valueRule: "bps",
    },
    {
      key: "refix_rate_uncertainty_bps",
      label: "Refix uncertainty (± %)",
      type: "percent",
      placeholder: "1.50",
      required: [kind],
      requiredWhen: when,
      valueRule: "bps",
    },
  ];
}

/**
 * The repayment as it is actually made. Optional: the forecast can derive a table payment
 * from the terms, and does when this is blank. Recording it matters when the real payment
 * differs from the ideal one — a deliberate overpayment, or the lender's own rounding —
 * because then the projection follows what is really being paid.
 */
function repaymentFields(): MetaField[] {
  return [
    { key: "repayment_minor", label: "Repayment", type: "money", section: "details" },
    {
      key: "repayment_frequency",
      label: "Repayment frequency",
      type: "select",
      options: [
        NONE_OPTION,
        { value: "weekly", label: "Weekly" },
        { value: "fortnightly", label: "Fortnightly" },
        { value: "monthly", label: "Monthly" },
      ],
      section: "details",
    },
  ];
}

/**
 * An *optional* subtype select leads with a blank option — labelled "None" to match the
 * reference's `include_blank`. `buildMetadata` drops the empty string.
 *
 * A required select must not offer it: a field you can answer with "None" isn't required, and
 * choosing it would only earn a 422 from the server. Those lists therefore start at their first
 * real option — see {@link MetaField.required} and {@link MetaField.default}.
 */
const NONE_OPTION: SubtypeOption = { value: "", label: "None" };

// Unit selects have a sensible default on the backend and so get no blank option. Labels
// use the app's en-NZ spelling ("Metres"/"Kilometres") even though the wire values match
// the reference's.
const AREA_UNITS: SubtypeOption[] = [
  { value: "sqft", label: "Square Feet" },
  { value: "sqm", label: "Square Metres" },
];
const MILEAGE_UNITS: SubtypeOption[] = [
  { value: "mi", label: "Miles" },
  { value: "km", label: "Kilometres" },
];

// Bounds for the "year" style inputs. Computed once at module load: a session spanning
// New Year's Eve isn't worth an extra re-render.
const CURRENT_YEAR = new Date().getFullYear();

/** Ordered metadata fields per profile. Keys match the backend struct field names. */
export const FIELDS: Record<MetaProfile, MetaField[]> = {
  depository: [
    {
      key: "subtype",
      label: "Subtype",
      type: "select",
      options: [NONE_OPTION, ...DEPOSITORY_SUBTYPES],
      kinds: DEPOSIT_KINDS,
    },
    // Card-only block. These live on the shared depository profile (the backend has one
    // struct for every deposit-like kind) but only make sense for a card, hence `kinds`.
    // Note there's deliberately no "available credit" input: we derive remaining
    // borrowing from the limit and the balance instead — see `remainingBorrowing`, which has
    // nothing to say without a limit; hence it's required of exactly the kinds that show it.
    {
      key: "credit_limit_minor",
      label: "Credit limit",
      type: "money",
      kinds: CARD_KINDS,
      required: CARD_KINDS,
      valueRule: "amount",
    },
    { key: "minimum_payment_minor", label: "Minimum payment", type: "money", kinds: CARD_KINDS },
    { key: "apr_bps", label: "APR", type: "percent", placeholder: "15.99", kinds: CARD_KINDS },
    { key: "expiration_date", label: "Expiration date", type: "date", kinds: CARD_KINDS },
    {
      key: "annual_fee_minor",
      label: "Annual fee",
      type: "money",
      placeholder: "99",
      kinds: CARD_KINDS,
    },
    { key: "account_number", label: "Account number", type: "text", section: "details" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  property: [
    {
      key: "subtype",
      label: "Property type",
      type: "select",
      options: PROPERTY_SUBTYPES,
      required: true,
    },
    {
      key: "year_built",
      label: "Year built",
      type: "int",
      placeholder: "1990",
      min: 1500,
      max: CURRENT_YEAR,
    },
    { key: "area_value", label: "Area", type: "int", placeholder: "1200", min: 0 },
    { key: "area_unit", label: "Area unit", type: "select", options: AREA_UNITS },
    { key: "purchase_date", label: "Purchase date", type: "date" },
    { key: "purchase_price_minor", label: "Purchase price", type: "money" },
    // Labels follow the reference; the example values are NZ-flavoured like the rest of
    // the app. `address_line1` is stored under the legacy `address` key on old rows — the
    // backend aliases it, so nothing here needs to know. Street, city and country are what
    // make a property *this* property, hence required; the rest of the address (unit, region,
    // postcode) is detail we can do without.
    {
      key: "address_line1",
      label: "Address line 1",
      type: "text",
      placeholder: "123 Kōwhai Street",
      required: true,
    },
    { key: "address_line2", label: "Address line 2", type: "text", placeholder: "Apartment 4B" },
    { key: "city", label: "City", type: "text", placeholder: "Wellington", required: true },
    { key: "region", label: "State / region", type: "text", placeholder: "Wellington Region" },
    { key: "postal_code", label: "Postal code", type: "text", placeholder: "6011" },
    { key: "country", label: "Country", type: "text", placeholder: "New Zealand", required: true },
    URL_FIELD,
    NOTES_FIELD,
  ],
  mortgage: [
    { key: "lender", label: "Lender", type: "text", required: true },
    {
      key: "original_amount_minor",
      label: "Original amount borrowed",
      type: "money",
      required: true,
      valueRule: "amount",
    },
    {
      key: "interest_rate_bps",
      label: "Interest rate (%)",
      type: "percent",
      placeholder: "5.49",
      required: true,
      valueRule: "bps",
    },
    REQUIRED_RATE_TYPE_FIELD,
    ...refixFields("mortgage"),
    { key: "fixed_term_months", label: "Fixed term (months)", type: "int" },
    {
      key: "term_months",
      label: "Overall term (months)",
      type: "int",
      placeholder: "324",
      required: true,
      valueRule: "count",
    },
    { key: "start_date", label: "Start date", type: "date", required: true },
    ...repaymentFields(),
    // Running totals are bookkeeping rather than setup, so they sit with the extras.
    {
      key: "interest_paid_minor",
      label: "Interest paid so far",
      type: "money",
      section: "details",
    },
    { key: "capital_paid_minor", label: "Capital paid so far", type: "money", section: "details" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  loan: [
    {
      key: "subtype",
      label: "Loan type",
      type: "select",
      options: LOAN_SUBTYPES,
      required: true,
    },
    { key: "lender", label: "Lender", type: "text", required: true },
    {
      key: "original_amount_minor",
      // Named as the mortgage's is: it is the amount borrowed at the start, not the balance
      // now. The old "Original loan balance" wording invited today's figure, which then read
      // as 0% repaid forever.
      label: "Original amount borrowed",
      type: "money",
      required: true,
      valueRule: "amount",
    },
    {
      key: "interest_rate_bps",
      label: "Interest rate (%)",
      type: "percent",
      placeholder: "5.25",
      required: true,
      valueRule: "bps",
    },
    // All required, as a mortgage's are: this profile is a table loan's alone now, and the
    // schedule is what the forecast projects a payoff from. An income-contingent student loan
    // has none of it and uses the `student_loan` profile below.
    REQUIRED_RATE_TYPE_FIELD,
    ...refixFields("loan"),
    { key: "fixed_term_months", label: "Fixed term (months)", type: "int" },
    {
      key: "term_months",
      label: "Term (months)",
      type: "int",
      placeholder: "360",
      required: true,
      valueRule: "count",
    },
    { key: "start_date", label: "Start date", type: "date", required: true },
    ...repaymentFields(),
    URL_FIELD,
    NOTES_FIELD,
  ],
  // An IR/StudyLink-style loan, and the shortest form in the app on purpose. There is no
  // principal to ask for (it was drawn down over years of study, in one tranche per
  // semester), no term, and no repayment schedule — it comes out of pay as a percentage of
  // income until it is gone. Asking anyway got placeholders that looked like answers; see
  // `StudentLoanMeta` in packages/core/src/types.rs. The balance and its history do the rest,
  // via the myIR import and the balance feed (docs/STUDENT-LOAN.md).
  student_loan: [
    { key: "lender", label: "Lender", type: "text", placeholder: "Inland Revenue", required: true },
    {
      key: "interest_rate_bps",
      label: "Interest rate (%)",
      type: "percent",
      placeholder: "0",
      hint: "Interest-free while you're based in New Zealand — enter 0. Overseas-based borrowers accrue interest.",
      required: true,
      valueRule: "bps",
    },
    URL_FIELD,
    NOTES_FIELD,
  ],
  vehicle: [
    // Make/model/year is the least that identifies a vehicle (and the most that a valuation
    // could ever be based on); plate, VIN and mileage are extras.
    { key: "make", label: "Make", type: "text", placeholder: "Toyota", required: true },
    { key: "model", label: "Model", type: "text", placeholder: "Camry", required: true },
    {
      key: "year",
      label: "Year",
      type: "int",
      placeholder: "2023",
      min: 1900,
      // Next year's models go on sale this year.
      max: CURRENT_YEAR + 1,
      required: true,
      // `Required::Count` on the backend — same ">0" rule as an amount, just on an `int`.
      valueRule: "amount",
    },
    { key: "mileage_value", label: "Mileage", type: "int", placeholder: "15000", min: 0 },
    { key: "mileage_unit", label: "Unit", type: "select", options: MILEAGE_UNITS },
    { key: "plate", label: "Plate", type: "text", section: "details" },
    { key: "nickname", label: "Nickname", type: "text", section: "details" },
    { key: "vin", label: "VIN", type: "text", section: "details" },
    { key: "purchase_date", label: "Purchase date", type: "date", section: "details" },
    { key: "sale_date", label: "Sale date", type: "date", section: "details" },
    URL_FIELD,
    NOTES_FIELD,
  ],
  shares: [
    // The investment subtype list is long and region-grouped, and it's what the tax
    // treatment is derived from (see INVESTMENT_TAX_TREATMENT) — never stored.
    { key: "subtype", label: "Investment type", type: "select", groups: INVESTMENT_SUBTYPE_GROUPS },
    { key: "broker", label: "Broker / platform", type: "text", required: true },
    // A listed holding is priced by (ticker, exchange), so both are required for the listed
    // kinds. `shares_private` shows them (an unlisted holding sometimes has a symbol worth
    // recording) but is never asked for them — it has no exchange to be listed on.
    { key: "ticker", label: "Ticker", type: "text", required: LISTED_SHARES_KINDS },
    { key: "exchange", label: "Exchange", type: "text", required: LISTED_SHARES_KINDS },
    URL_FIELD,
    NOTES_FIELD,
  ],
  // A brokerage account's individual tickers/currencies live per-holding (see the
  // holdings ledger), not on the account, so only the platform-level fields are here.
  brokerage: [
    { key: "subtype", label: "Investment type", type: "select", groups: INVESTMENT_SUBTYPE_GROUPS },
    { key: "broker", label: "Broker / platform", type: "text", required: true },
    URL_FIELD,
    NOTES_FIELD,
  ],
  // Crypto is the one investment profile whose tax treatment isn't implied by the
  // subtype, so it's asked for explicitly (defaulting to taxable on the backend).
  crypto: [
    {
      key: "subtype",
      label: "Account type",
      type: "select",
      options: CRYPTO_SUBTYPES,
      required: true,
    },
    // Required, and the one required select with a conventional answer — holdings are taxable
    // unless the owner says otherwise (the reference's `Crypto#tax_treatment` default) — so it
    // carries that as its `default`, keeping the payload in step with the "Taxable" the select
    // shows before anyone touches it.
    {
      key: "tax_treatment",
      label: "Tax treatment",
      type: "select",
      options: TAX_TREATMENTS,
      required: true,
      default: "taxable",
      hint:
        "Most cryptocurrency is held in taxable accounts. Select a different option if held in a tax-advantaged account.",
    },
    URL_FIELD,
    NOTES_FIELD,
  ],
  generic: [URL_FIELD, NOTES_FIELD],
};

/**
 * Whether `f` must be filled in for an account of this `kind` — the one place to ask, since a
 * requirement can be profile-wide (`required: true`) or limited to some kinds
 * (`required: ["credit_card", …]`), and a field the kind doesn't even show (see
 * {@link MetaField.kinds}) is never required of it: `buildMetadata` drops those, so the server
 * never sees them either.
 */
export function isFieldRequired(
  f: MetaField,
  kind: string,
  raw?: Record<string, string>
): boolean {
  if (!f.required) return false;
  if (f.kinds && !f.kinds.includes(kind)) return false;
  if (f.required !== true && !f.required.includes(kind)) return false;
  if (!f.requiredWhen) return true;
  // Without the form's current values there's no way to evaluate the condition. Answer
  // "not required" rather than guessing: the caller that has `raw` (the save path) is the
  // one that must not let an incomplete account through, and it always passes it.
  if (!raw) return false;
  return f.requiredWhen.equals.includes((raw[f.requiredWhen.key] ?? "").trim());
}

/** Every legal `<option>` value for `f`, flattening {@link MetaField.groups} if it has them. */
function legalValues(f: MetaField): string[] {
  return f.groups
    ? f.groups.flatMap((g) => g.options.map((o) => o.value))
    : (f.options ?? []).map((o) => o.value);
}

/**
 * Whether `value` is a *stored* value for select field `f` that doesn't match any of its
 * legal options. Blank never counts — that's simply unanswered, a {@link isFieldRequired}
 * question, not this one.
 *
 * Reachable without a stale client: the backend does not value-check every select. An
 * investment (`shares`/`brokerage`) account's `subtype` is deliberately left unvalidated
 * server-side — `SUBTYPE_VALUES` in packages/core/src/types.rs excludes it on purpose,
 * because the curated 43-entry investment list already lives only in this file
 * (`accountSubtypes.ts`), and duplicating the check there would just give the two copies a
 * second place to drift — so an investment account written directly through the API (or,
 * one day, synced from a provider) can legitimately carry a `subtype` this file has never
 * heard of. A form that seeds every select from its own option list on the assumption that
 * "not in the list" means "stale/mistaken" would silently overwrite that value the moment
 * the account is next saved; callers should check this first and, if it's true, keep the
 * value on screen instead — see {@link selectOptions}/{@link selectGroups}, which do that
 * by construction.
 */
export function isUnknownStoredValue(f: MetaField, value: string | null | undefined): boolean {
  const v = (value ?? "").trim();
  return v !== "" && !legalValues(f).includes(v);
}

/**
 * `f`'s flat option list ({@link MetaField.options}), plus a trailing synthetic entry when
 * `currentValue` is set but unrecognised (see {@link isUnknownStoredValue}) — so a select
 * bound to a stored-but-uncurated value always has an option that *is* that value, and
 * rendering it can never lose it (whether by an eager seed substituting the first option,
 * or the option simply not existing to select). The trailing label makes plain it isn't a
 * real choice, just what's already there.
 *
 * For a `groups` field use {@link selectGroups} instead — the shape differs
 * (`SubtypeGroup[]`, not `SubtypeOption[]`), which matters in practice, since the one field
 * the backend actually leaves unvalidated (investment `subtype`) is grouped.
 */
export function selectOptions(
  f: MetaField,
  currentValue: string | null | undefined,
): SubtypeOption[] {
  const opts = f.options ?? [];
  if (!isUnknownStoredValue(f, currentValue)) return opts;
  const v = (currentValue as string).trim();
  return [...opts, { value: v, label: `${v} (current value)` }];
}

/** {@link selectOptions}'s counterpart for a `groups` field (see {@link MetaField.groups}). */
export function selectGroups(
  f: MetaField,
  currentValue: string | null | undefined,
): SubtypeGroup[] {
  const groups = f.groups ?? [];
  if (!isUnknownStoredValue(f, currentValue)) return groups;
  const v = (currentValue as string).trim();
  return [...groups, { label: "Current value", options: [{ value: v, label: v }] }];
}

/**
 * The value-shape problem with `f`'s current (trimmed) value, or `null` if there isn't
 * one — the client-side mirror of `Required::problem` in packages/core/src/types.rs, so a
 * value the pre-flight would otherwise wave through (blankness is all
 * {@link isFieldRequired} checks) doesn't come back as a 422 naming a wire key the user
 * never saw. Only fires when {@link MetaField.valueRule} names a rule *and* the field
 * actually has a value — an empty one is {@link isFieldRequired}'s problem to report, not
 * this one's, exactly like the server treats an absent key and a present-but-wrong-shaped
 * one as two different problems. An unparseable value (letters typed into a money field)
 * is likewise left to whatever already handles that upstream — this only judges values
 * that parse.
 *
 * packages/core/src/types.rs's `PROFILE_REQUIRED`/`KIND_REQUIRED` tables are the source of
 * truth this has to keep agreeing with: a field's `valueRule` here must match the
 * `Required` variant it's given there, or the two can quietly drift apart.
 */
export function valueProblem(f: MetaField, value: string): string | null {
  if (!f.valueRule) return null;
  const v = value.trim();
  if (!v) return null;
  const n = f.type === "int" ? parseInt(v, 10) : parseFloat(v.replace(/[^0-9.-]/g, ""));
  if (isNaN(n)) return null;
  switch (f.valueRule) {
    case "amount":
      return n > 0 ? null : `${f.label} must be greater than zero.`;
    case "bps":
      return n >= 0 ? null : `${f.label} cannot be negative.`;
    // `Required::Count` — a whole count, so zero is a placeholder rather than an answer.
    case "count":
      return n > 0 ? null : `${f.label} must be greater than zero.`;
  }
}

/** Convert an interest rate in basis points to a display percentage, e.g. 549 → "5.49%". */
export const formatRateBps = (bps: number) => `${(bps / 100).toFixed(2).replace(/\.?0+$/, "")}%`;

/**
 * Build a metadata object for POST/PUT from the form's raw string inputs, converting
 * money → minor units, percent → basis points, and ints.
 *
 * `stored` — the account's metadata as currently persisted, when editing one — turns this
 * from a rebuild into an **overlay**, which is the whole point of passing it:
 *
 * 1. The result starts as a shallow copy of `stored`, but only when `stored.profile`
 *    already matches `kind`'s profile. A kind change that crosses profiles (bank →
 *    real_estate, say) gets a clean slate instead — the old profile's keys mean nothing
 *    under the new one, so there is nothing sensible to overlay onto.
 * 2. For every field this `kind` actually renders — passes {@link MetaField.kinds}, the
 *    same test `AccountForm` uses to decide what's on screen — the corresponding stored
 *    key is deleted, then replaced with whatever `raw` says now (nothing, if the field is
 *    blank and has no {@link MetaField.default}: that's the user deliberately clearing it).
 * 3. A field this `kind` does **not** render is never touched, in either direction: not
 *    read from `raw` (there's normally nothing there to read), and — this is the fix —
 *    not deleted from `stored` either. A provider sync's `credit_limit_minor` on an
 *    overdraft mapped to `bank` (whose form has no field for it — credit limit is
 *    card-only) survives renaming the account, instead of vanishing the next time it's
 *    saved just because nothing on screen ever mentioned it.
 *
 * Omitting `stored` (every call site's behaviour before this parameter existed)
 * reproduces the old rebuild-from-scratch result — everything comes from `raw` or nothing
 * — which is exactly right for a brand new account with nothing stored yet.
 *
 * One asymmetry worth knowing rather than being surprised by: switching *within* one
 * profile (card → savings, both `depository`) does **not** clear the fields the old kind
 * had that the new one doesn't render (an old `apr_bps` survives, inert, until something
 * deletes it) — steps 1 and 3 above can't tell "kind changed but stayed in-profile" apart
 * from "kind never changed", and the fix this overlay exists for needs step 3 to hold in
 * both cases. Harmless in practice: the field stays invisible under the new kind (nothing
 * in `FIELDS` for that profile shows it to that kind), and reappears, correctly, if the
 * kind is switched back.
 *
 * The `profile` key in the result is always `kind`'s, never `stored`'s.
 */
export function buildMetadata(
  kind: string,
  raw: Record<string, string>,
  stored?: Schemas["AccountMetadata"] | null,
): Schemas["AccountMetadata"] {
  const profile = kindToProfile(kind);
  const storedRecord = stored as Record<string, unknown> | null | undefined;
  const out: Record<string, unknown> =
    storedRecord && storedRecord.profile === profile ? { ...storedRecord } : {};
  out.profile = profile;
  for (const f of FIELDS[profile]) {
    if (f.kinds && !f.kinds.includes(kind)) continue;
    delete out[f.key];
    // A field with a `default` is never sent blank — its select is already showing that value,
    // so submitting nothing would contradict the screen (and be refused, since only required
    // selects carry one). Everything else empty is simply omitted (the `delete` above already
    // leaves the key absent).
    const v = (raw[f.key] ?? "").trim() || (f.default ?? "");
    if (!v) continue;
    if (f.type === "money" || f.type === "percent") {
      const n = parseFloat(v.replace(/[^0-9.-]/g, ""));
      if (!isNaN(n)) out[f.key] = Math.round(n * 100);
    } else if (f.type === "int") {
      const n = parseInt(v, 10);
      if (!isNaN(n)) out[f.key] = n;
    } else {
      // Text, dates and every select (subtypes and the unit/treatment enums alike) are
      // plain strings on the wire.
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
  const profile = kindToProfile(kind);
  // The stored subtype is an opaque key (e.g. "single_family_home"); show its label.
  const subtype = subtypeLabel(profile, s("subtype"));
  switch (profile) {
    case "property":
      if (subtype) bits.push(subtype);
      // The city is the most recognisable part of an address; fall back to the street.
      if (s("city")) bits.push(s("city")!);
      else if (s("address_line1")) bits.push(s("address_line1")!);
      break;
    case "vehicle": {
      const desc = [s("make"), s("model"), n("year")].filter(Boolean).join(" ");
      if (s("nickname")) bits.push(`“${s("nickname")}”`);
      if (desc) bits.push(desc);
      if (s("plate")) bits.push(s("plate")!);
      if (n("mileage_value") !== undefined) {
        bits.push(`${n("mileage_value")!.toLocaleString()} ${s("mileage_unit") ?? "mi"}`);
      }
      break;
    }
    case "mortgage":
    case "loan":
    // A student loan has no subtype (its kind says "student" already), so `subtypeLabel`
    // returns nothing and this falls through to the lender and rate it does have.
    case "student_loan":
      if (subtype) bits.push(subtype);
      // Loans have no top-level institution; the lender stands in for it here.
      if (s("lender")) bits.push(s("lender")!);
      if (n("interest_rate_bps") !== undefined) bits.push(formatRateBps(n("interest_rate_bps")!));
      break;
    case "shares":
      if (subtype) bits.push(subtype);
      // Shares have no top-level institution; the broker/platform stands in for it here.
      if (s("broker")) bits.push(s("broker")!);
      if (s("ticker")) bits.push(s("ticker")!);
      break;
    case "brokerage":
      if (subtype) bits.push(subtype);
      // Like shares, the broker/platform stands in for an institution.
      if (s("broker")) bits.push(s("broker")!);
      break;
    case "crypto":
      // Wallet vs exchange is the only distinguishing detail worth a list row.
      if (subtype) bits.push(subtype);
      break;
    case "depository":
      if (subtype) bits.push(subtype);
      if (s("account_number")) bits.push(s("account_number")!);
      break;
    default:
      break;
  }
  return bits.join(" · ");
}
