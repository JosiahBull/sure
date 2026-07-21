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
