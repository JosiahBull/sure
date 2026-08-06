<script lang="ts">
  // What a student loan's ledger currently holds, and the way to add to it.
  //
  // The account row already shows the balance and paid-off %, so this panel reports the one thing
  // it can't: how much ledger is actually loaded, and how far back it reaches. Akahu reports this
  // account's balance but no transactions, so everything behind the cutover comes from a myIR
  // upload and everything after it is derived from the daily balance feed by the backend's
  // `balance_delta` task.
  //
  // The upload itself is `ImportPanel`'s, pre-scoped to this account — the same component the
  // Import page renders, so the two cannot drift apart the way four separate panels did.
  import { onMount } from "svelte";
  import { api } from "./api";
  import ImportPanel from "./ImportPanel.svelte";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  // High enough to cover a full student-loan history in one request (weekly living costs over a
  // degree plus fortnightly repayments is a few hundred rows); `saturated` keeps the count honest
  // rather than silently reporting the cap as the total.
  const LEDGER_LIMIT = 1000;

  let count = $state(0);
  let saturated = $state(false);
  let oldest = $state<string | null>(null);
  let newest = $state<string | null>(null);

  async function load() {
    // `include_one_off: false` drops the "Opening balance" row every account is seeded with —
    // counting it would make a brand-new loan claim it already has a ledger.
    const { data } = await api.GET("/api/transactions", {
      params: {
        query: { account_id: accountId, limit: LEDGER_LIMIT, include_one_off: false },
      },
    });
    const rows = data ?? [];
    count = rows.length;
    saturated = rows.length === LEDGER_LIMIT;
    const dates = rows.map((t) => t.posted_at.slice(0, 10)).sort();
    oldest = dates[0] ?? null;
    newest = dates[dates.length - 1] ?? null;
  }
  onMount(load);

  function reload() {
    load();
    onchange?.();
  }
</script>

<div class="student-loan">
  <div class="stat" style="gap:2px;margin-bottom:10px">
    {#if count}
      <div class="value tabular" style="font-size:20px">
        {saturated ? `${LEDGER_LIMIT}+` : count}
        <span class="faint" style="font-size:13px">transaction{count === 1 ? "" : "s"}</span>
      </div>
      <div class="small faint">{oldest} → {newest}</div>
    {:else}
      <div class="value" style="font-size:20px">No ledger yet</div>
      <div class="small faint">Akahu reports this loan's balance, but not its transactions.</div>
    {/if}
  </div>

  <ImportPanel {accountId} onchange={reload} />
</div>

<style>
  .student-loan {
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
</style>
