// Shared account-balances snapshot — the account panel (long-lived, persists across
// route changes) and the dashboard's Balance Sheet/Investments cards all read this same
// store instead of each fetching their own copy, so an edit in Accounts.svelte is visible
// everywhere as soon as it calls refresh().
import { api, type Schemas } from "./api";

export const balances = $state({
  data: null as Schemas["BalancesReport"] | null,
  loading: true,
  error: null as string | null,
});

export async function refresh(): Promise<void> {
  balances.loading = true;
  balances.error = null;
  const { data, error } = await api.GET("/api/reports/balances", {});
  balances.data = data ?? null;
  if (error) balances.error = "Failed to load balances.";
  balances.loading = false;
}
