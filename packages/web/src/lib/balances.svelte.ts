// Shared account-balances snapshot — the account panel (long-lived, persists across
// route changes) and the dashboard's Balance Sheet/Investments cards all read this same
// store instead of each fetching their own copy, so an edit in Accounts.svelte is visible
// everywhere as soon as it calls refresh().
//
// It starts from the last-known snapshot (see `lastKnown.ts`), so the panel has figures in its
// first frame, and it refreshes itself after every mutation: the panel outlives the page an
// edit was made on, and was otherwise left showing pre-edit balances until a reload.
import { api, type Schemas } from "./api";
import { keep, lastKnown, onMutationSettled } from "./lastKnown";

const KEY = "balances";
const kept = lastKnown<Schemas["BalancesReport"]>(KEY);

export const balances = $state({
  data: kept?.data ?? null,
  loading: true,
  error: null as string | null,
  /** `data` is not the server's latest answer: kept from an earlier launch, or predating an edit. */
  stale: kept !== null,
  /** When `data` was fetched, for saying how old it is if a refresh fails. */
  at: kept?.at ?? null,
});

/** Whether this session has asked the server yet; kept data does not count. */
let asked = false;
/** Only the latest refresh may write, so a slow pre-edit answer cannot land over a newer one. */
let seq = 0;

export async function refresh(): Promise<void> {
  const mine = ++seq;
  asked = true;
  balances.loading = true;
  balances.error = null;
  // Offline, `fetch` throws rather than returning an error; both mean the same here.
  const { data, error } = await keep(KEY, () => api.GET("/api/reports/balances", {})).catch(
    (e: unknown) => ({ data: undefined, error: e }),
  );
  if (mine !== seq) return;
  if (data) {
    balances.data = data;
    balances.stale = false;
    balances.at = Date.now();
  }
  // A failed refresh leaves whatever was drawn, still marked stale.
  if (error) balances.error = "Failed to load balances.";
  balances.loading = false;
}

/** Ask the server once per session; later callers share the snapshot. */
export function ensureLoaded(): void {
  if (!asked) void refresh();
}

onMutationSettled(() => {
  if (!asked) return;
  balances.stale = balances.data !== null;
  void refresh();
});
