<script lang="ts">
  // The one place import lives. Before this it lived in four: a button at the top of the accounts
  // page, two different row expanders, a paste box on the bank-sync page — and a fifth button on
  // the transactions page that pointed at the rules screen and had never imported anything.
  //
  // The page adds nothing to `ImportPanel` beyond a destination and an explanation. Accounts and
  // bank-sync link here; a row on the accounts page renders the same component, pre-scoped to
  // that account, so the two surfaces cannot drift.
  import ImportPanel from "../lib/ImportPanel.svelte";
  import { queryParams } from "../lib/router.svelte";
  import { refresh as refreshBalances } from "../lib/balances.svelte";

  // `?account=` pre-scopes the page, which is what a link from an account row or a bank-sync
  // connection carries. Absent, the upload is routed by what's in it.
  const scoped = $derived.by(() => {
    const raw = queryParams().get("account");
    const id = raw === null ? NaN : Number(raw);
    return Number.isFinite(id) ? id : undefined;
  });
</script>

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <div>
    <h1 style="font-size:20px">Import</h1>
    <div class="muted small" style="max-width:70ch">
      Bank feeds only reach back about two years. Your bank's own export reaches much further, and
      for a student loan or a brokerage there is no feed of transactions at all — so this is how
      an account's history gets extended past what syncs on its own.
    </div>
  </div>
</div>

{#key scoped}
  <ImportPanel accountId={scoped} onchange={refreshBalances} />
{/key}

<section class="card" style="margin-top:14px">
  <h2>Where the files come from</h2>
  <dl class="sources small">
    <dt>ASB</dt>
    <dd>
      In FastNet, open the account → <strong>Export transactions</strong>, choose
      <strong>CSV</strong>
      and the <code>YYYY/MM/DD</code> date format, and pick as wide a date range as it allows. One
      file per account — select them all at once here.
    </dd>
    <dt>myIR (student loan)</dt>
    <dd>
      Export your <strong>TAP SLS Transactions</strong>. One export only reaches back about two
      years, so a whole loan takes several — pick them <em>together</em>, because the checks that
      catch a missing window can only run with every export in hand.
    </dd>
    <dt>Sharesies</dt>
    <dd>Request an export from your account settings and drop the <code>.zip</code> in whole.</dd>
    <dt>Anything else</dt>
    <dd>
      A <code>.csv</code> with <code>date</code> and <code>amount</code> columns —
      <code>description</code>, <code>merchant</code>, <code>currency</code> and
      <code>external_id</code> are used if they're there.
    </dd>
  </dl>
</section>

<style>
  .sources {
    display: grid;
    gap: 4px 14px;
    margin: 0;
  }
  .sources dt {
    font-weight: 600;
    margin-top: 8px;
  }
  .sources dt:first-child {
    margin-top: 0;
  }
  .sources dd {
    margin: 0;
    color: var(--text-muted);
    line-height: 1.5;
  }
  @media (min-width: 640px) {
    .sources {
      grid-template-columns: auto 1fr;
    }
    .sources dt {
      white-space: nowrap;
      margin-top: 0;
    }
  }
  code {
    background: var(--surface-2);
    border-radius: 4px;
    padding: 0 4px;
  }
</style>
