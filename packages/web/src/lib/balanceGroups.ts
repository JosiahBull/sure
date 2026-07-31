// Groups a BalancesReport's accounts by kind for a given panel tab, shared by AccountPanel
// and the dashboard's Balance Sheet card so both agree on tab semantics and the weight-%
// denominator (a kind-group's share of its own tab's total, not of net worth as a whole).
import type { Schemas } from "./api";
import { kindLabel } from "./accountMeta";

export type PanelTab = "all" | "assets" | "debts";

export interface KindGroup {
  kind: string;
  label: string;
  totalMinor: number;
  weightPct: number;
  accounts: Schemas["AccountBalance"][];
}

/** "Assets" bundles every non-liability class (cash/investment/asset), matching the
 * reference app, where a account's classification is a plain asset/liability split. */
function inTab(a: Schemas["AccountBalance"], tab: PanelTab): boolean {
  if (tab === "all") return true;
  if (tab === "debts") return a.class === "liability";
  return a.class !== "liability";
}

export function groupByKind(
  accounts: Schemas["AccountBalance"][],
  tab: PanelTab
): { groups: KindGroup[]; totalMinor: number } {
  const rows = accounts.filter((a) => inTab(a, tab));
  const totalMinor = rows.reduce((sum, a) => sum + a.value_minor, 0);

  // Weight is always a share of the account's own classification total (assets or
  // liabilities), never of net worth — mixing the two on the "all" tab would otherwise
  // divide an asset by (assets − liabilities) and produce nonsense like 376% or -279%.
  const assetsTotal = accounts
    .filter((a) => a.class !== "liability")
    .reduce((sum, a) => sum + a.value_minor, 0);
  const liabilitiesTotal = accounts
    .filter((a) => a.class === "liability")
    .reduce((sum, a) => sum + a.value_minor, 0);

  const byKind = new Map<string, Schemas["AccountBalance"][]>();
  for (const a of rows) {
    const list = byKind.get(a.kind);
    if (list) list.push(a);
    else byKind.set(a.kind, [a]);
  }

  const groups: KindGroup[] = Array.from(byKind.entries()).map(([kind, kindAccounts]) => {
    const kindTotal = kindAccounts.reduce((sum, a) => sum + a.value_minor, 0);
    const isLiability = kindAccounts[0].class === "liability";
    const denominator = isLiability ? liabilitiesTotal : assetsTotal;
    return {
      kind,
      label: kindLabel(kind),
      totalMinor: kindTotal,
      weightPct: denominator === 0 ? 0 : (kindTotal / denominator) * 100,
      accounts: kindAccounts,
    };
  });
  groups.sort((a, b) => Math.abs(b.totalMinor) - Math.abs(a.totalMinor));

  return { groups, totalMinor };
}

// --- Higher-level classification groups (Cash / Investment / Property / …) --------------
// The reference sidebar buckets accounts into a handful of human classes rather than the raw
// per-kind list, in a fixed order (assets first, then debts). Each account kind maps to one.
const CLASS_GROUPS: { key: string; label: string; kinds: string[] }[] = [
  { key: "cash", label: "Cash", kinds: ["bank", "cash", "savings"] },
  { key: "investment", label: "Investment", kinds: ["shares_nz", "shares_us", "shares_private", "brokerage"] },
  // Crypto is an investment by class but the reference lists it as its own bucket, between
  // Investment and Property (Accountable::TYPES) — and gives it its own colour, see KIND_STYLE.
  { key: "crypto", label: "Crypto", kinds: ["crypto"] },
  { key: "property", label: "Property", kinds: ["real_estate"] },
  { key: "vehicle", label: "Vehicle", kinds: ["vehicle"] },
  { key: "other_asset", label: "Other assets", kinds: ["asset"] },
  { key: "credit", label: "Credit cards", kinds: ["credit_card", "revolving_credit"] },
  { key: "loans", label: "Loans", kinds: ["mortgage", "student_loan", "loan"] },
  { key: "other_liability", label: "Other liabilities", kinds: ["liability"] },
];
const CLASS_OF = new Map<string, { key: string; label: string; order: number }>();
CLASS_GROUPS.forEach((g, order) => g.kinds.forEach((k) => CLASS_OF.set(k, { key: g.key, label: g.label, order })));
// An unrecognised kind falls in with the catch-all assets bucket. Looked up rather than spelled
// out, so inserting a group above it can't leave the fallback sorting at the wrong position.
const FALLBACK_CLASS = CLASS_OF.get("asset")!;
const classOf = (kind: string) => CLASS_OF.get(kind) ?? FALLBACK_CLASS;

export interface ClassGroup {
  key: string;
  label: string;
  totalMinor: number;
  /** Change in the group's value over the active period (current − period-start), signed. */
  changeMinor: number;
  /** changeMinor as a % of the period-start value; null when there's no baseline to divide by. */
  changePct: number | null;
  accounts: Schemas["AccountBalance"][];
}

/**
 * Group accounts into the reference's classification buckets and, given a per-account baseline
 * (values as of the period start), compute each group's signed change and change-%. Mirrors the
 * sidebar's "value + coloured %" rows.
 */
export function groupByClass(
  accounts: Schemas["AccountBalance"][],
  tab: PanelTab,
  baseline: Map<number, number>,
): { groups: ClassGroup[]; totalMinor: number } {
  const rows = accounts.filter((a) => inTab(a, tab));
  const totalMinor = rows.reduce((sum, a) => sum + a.value_minor, 0);

  const byClass = new Map<string, Schemas["AccountBalance"][]>();
  for (const a of rows) {
    const c = classOf(a.kind).key;
    const list = byClass.get(c);
    if (list) list.push(a);
    else byClass.set(c, [a]);
  }

  const groups: ClassGroup[] = Array.from(byClass.entries()).map(([key, groupAccounts]) => {
    const total = groupAccounts.reduce((sum, a) => sum + a.value_minor, 0);
    const base = groupAccounts.reduce((sum, a) => sum + (baseline.get(a.account_id) ?? 0), 0);
    const changeMinor = total - base;
    return {
      key,
      label: classOf(groupAccounts[0].kind).label,
      totalMinor: total,
      changeMinor,
      changePct: base !== 0 ? (changeMinor / Math.abs(base)) * 100 : null,
      accounts: groupAccounts,
    };
  });
  // Fixed reference order (assets first, then debts), not by value.
  groups.sort((a, b) => classOf(a.accounts[0].kind).order - classOf(b.accounts[0].kind).order);

  return { groups, totalMinor };
}
