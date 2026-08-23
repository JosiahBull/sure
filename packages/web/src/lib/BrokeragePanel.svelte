<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "./api";
  import FxNotice from "./FxNotice.svelte";
  import ImportPanel from "./ImportPanel.svelte";
  import InvestmentTabs from "./InvestmentTabs.svelte";
  import ValuationPanel from "./ValuationPanel.svelte";

  let {
    accountId,
    // Whether this account gets a bulk importer. A Sharesies-style platform does; a single
    // listed holding is typed in, so offering it an import that expects a platform export
    // would just be a dead end.
    importable = true,
    onchange,
  }: { accountId: number; importable?: boolean; onchange?: () => void } = $props();

  // Units and price, kept apart the way they are for private equity: the lots ledger says how
  // many are held and when they arrived, `stock_prices` says what one is worth, and the
  // snapshot below is the product. Neither tab alone is the account's value.
  let snapshot = $state<Schemas["BrokerageSnapshot"] | null>(null);
  let lots = $state<Schemas["HoldingLot"][]>([]);
  let tab = $state<"holdings" | "activity" | "valuations">("holdings");
  let busy = $state<null | "revalue" | "backfill" | "lot">(null);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  async function load() {
    const [snap, held] = await Promise.all([
      api.GET("/api/accounts/{id}/brokerage", { params: { path: { id: accountId } } }),
      api.GET("/api/accounts/{id}/brokerage/holdings", { params: { path: { id: accountId } } }),
    ]);
    snapshot = snap.data ?? null;
    lots = held.data ?? [];
  }
  onMount(load);

  // ---- manual lots -----------------------------------------------------------------
  const blankLot = () => ({
    ticker: "",
    exchange: "",
    trade_date: new Date().toISOString().slice(0, 10),
    quantity: "",
    unit_price: "",
    kind: "buy" as Schemas["LotKind"],
  });
  let showLotForm = $state(false);
  let lotForm = $state(blankLot());

  async function saveLot() {
    const quantity = parseFloat(lotForm.quantity.replace(/[^0-9.-]/g, ""));
    if (!lotForm.ticker.trim() || !Number.isFinite(quantity) || quantity === 0) return;
    busy = "lot";
    error = null;
    const { error: e } = await api.POST("/api/accounts/{id}/brokerage/holdings", {
      params: { path: { id: accountId } },
      body: {
        ticker: lotForm.ticker.trim().toUpperCase(),
        exchange: lotForm.exchange.trim().toUpperCase(),
        currency_code: snapshot?.currency_code ?? "NZD",
        trade_date: lotForm.trade_date,
        // A sale is a negative quantity, so the form takes a magnitude and the kind decides
        // the sign — typing "-100" into a field labelled Units to record a sale is the
        // likeliest way to get this backwards.
        quantity: lotForm.kind === "sell" ? -Math.abs(quantity) : Math.abs(quantity),
        unit_price: lotForm.unit_price.trim() ? parseFloat(lotForm.unit_price) : null,
        kind: lotForm.kind,
      },
    });
    busy = null;
    if (e) {
      error = (e as { error?: { message?: string } }).error?.message ?? "Couldn't save that trade.";
      return;
    }
    showLotForm = false;
    lotForm = blankLot();
    await load();
    onchange?.();
  }

  async function deleteLot(id: number) {
    busy = "lot";
    const { error: e } = await api.DELETE("/api/brokerage/holdings/{id}", {
      params: { path: { id } },
    });
    busy = null;
    if (e) {
      error =
        (e as { error?: { message?: string } }).error?.message ?? "Couldn't delete that trade.";
      return;
    }
    await load();
    onchange?.();
  }

  const LOT_LABEL: Record<Schemas["LotKind"], string> = {
    buy: "bought",
    sell: "sold",
    corporate: "corporate action",
  };

  function reload() {
    load();
    onchange?.();
  }

  async function revalue() {
    busy = "revalue";
    error = null;
    const { error: e } = await api.POST("/api/accounts/{id}/brokerage/revalue", {
      params: { path: { id: accountId } },
    });
    // Surface the server's own words: a 422 here names the currency that has no rate, which
    // is the one thing that tells the user what to fix. "Revalue failed." does not.
    if (e) error = (e as { error?: { message?: string } }).error?.message ?? "Revalue failed.";
    else notice = "Revalued.";
    await load();
    onchange?.();
    busy = null;
  }

  async function backfill() {
    busy = "backfill";
    error = null;
    const { data, error: e } = await api.POST("/api/accounts/{id}/brokerage/backfill", {
      params: { path: { id: accountId } },
    });
    if (e) error = (e as { error?: { message?: string } }).error?.message ?? "Backfill failed.";
    else if (data) notice = `Backfilled ${data.days} day(s) of history.`;
    await load();
    onchange?.();
    busy = null;
  }

</script>

<div class="brokerage">
  {#if error}<div class="error-banner" style="margin-bottom:8px">{error}</div>{/if}
  {#if notice}<div class="badge" style="margin-bottom:8px">{notice}</div>{/if}

  {#if snapshot}
    <div class="row spread" style="margin-bottom:8px">
      <span class="muted small">
        Portfolio value <strong class="tabular" style="color:var(--text)"
          >{formatMoney(snapshot.total_value_minor, snapshot.currency_code)}</strong
        >
      </span>
      <div class="row" style="gap:8px">
        <button class="btn btn-sm" onclick={revalue} disabled={busy !== null}>
          {busy === "revalue" ? "…" : "Revalue"}
        </button>
        <button class="btn btn-sm" onclick={backfill} disabled={busy !== null}>
          {busy === "backfill" ? "…" : "Backfill"}
        </button>
      </div>
    </div>

    <!-- Same story as the dashboard's Investments card, and the reason a Revalue can 422:
         an unconvertible holding is outside "Portfolio value" and must not be written into a
         valuation, where nothing would ever reveal it was understated. -->
    <FxNotice
      unconverted={snapshot.unconverted}
      ratesAsOf={snapshot.rates_as_of}
      currency={snapshot.currency_code}
    />

    <InvestmentTabs
      tabs={[
        { id: "holdings", label: "Holdings", count: snapshot.positions.length },
        { id: "activity", label: "Activity", count: lots.length },
        { id: "valuations", label: "Valuations" },
      ]}
      active={tab}
      onselect={(id) => (tab = id as typeof tab)}
    />

    {#if tab === "activity"}
      <div class="small faint" style="padding:4px 0 8px">
        Every trade and corporate action. This ledger is what says how many units were held on
        any past date — the prices come from the feed, and the two multiply.
      </div>
      {#if importable}
        <ImportPanel {accountId} onchange={reload} />
      {/if}
      {#each lots as l (l.id)}
        <div class="row spread line small">
          <span style="min-width:0">
            <span class="badge">{LOT_LABEL[l.kind]}</span>
            <strong>{l.ticker}</strong>
            <span class="tabular"
              >{Math.abs(l.quantity).toLocaleString(undefined, { maximumFractionDigits: 4 })}</span
            > units
            <span class="faint">
              · {formatDate(l.trade_date)}{l.unit_price != null
                ? ` · at ${formatMoney(Math.round(l.unit_price * 100), l.currency_code)}`
                : ""}{l.provider ? ` · ${l.provider}` : ""}
            </span>
          </span>
          <button class="link danger" onclick={() => deleteLot(l.id)}>Delete</button>
        </div>
      {/each}
      {#if lots.length === 0}
        <div class="small faint" style="padding:6px 0">No trades recorded yet.</div>
      {/if}
      {#if showLotForm}
        <div class="sub">
          <div
            class="grid"
            style="grid-template-columns:repeat(auto-fit,minmax(110px,1fr));gap:8px"
          >
            <label class="field">
              <span class="small muted">Ticker</span>
              <input class="input" bind:value={lotForm.ticker} />
            </label>
            <label class="field">
              <span class="small muted">Exchange</span>
              <input class="input" placeholder="NZX" bind:value={lotForm.exchange} />
            </label>
            <label class="field">
              <span class="small muted">Date</span>
              <input class="input" type="date" bind:value={lotForm.trade_date} />
            </label>
            <label class="field">
              <span class="small muted">Units</span>
              <input class="input tabular" bind:value={lotForm.quantity} />
            </label>
            <label class="field">
              <span class="small muted">Unit price</span>
              <input class="input tabular" placeholder="0.00" bind:value={lotForm.unit_price} />
            </label>
            <label class="field">
              <span class="small muted">Kind</span>
              <select class="input" bind:value={lotForm.kind}>
                <option value="buy">Bought</option>
                <option value="sell">Sold</option>
                <option value="corporate">Corporate action</option>
              </select>
            </label>
          </div>
          <div class="row" style="gap:8px;margin-top:8px">
            <button class="btn btn-sm btn-primary" onclick={saveLot} disabled={busy !== null}
              >Add trade</button
            >
            <button class="btn btn-sm" onclick={() => (showLotForm = false)}>Cancel</button>
          </div>
          <div class="small faint" style="margin-top:6px">
            Enter units as a positive number — "Sold" makes it a reduction. Prices come from the
            feed, so the unit price here is a record of what was paid, not what it's worth now.
          </div>
        </div>
      {:else}
        <button class="btn btn-sm" style="margin-top:8px" onclick={() => (showLotForm = true)}
          >+ Trade</button
        >
      {/if}
    {/if}

    {#if tab === "valuations"}
      <div class="small faint" style="padding:4px 0 8px">
        Snapshots of this account's value in net-worth history. Revalue writes today's computed
        figure; Backfill writes one per day the feed has prices for. A value entered by hand
        overrides the computed one from its date until the next.
      </div>
      <ValuationPanel
        {accountId}
        accountClass="investment"
        currency={snapshot.currency_code}
        onchange={reload}
      />
    {/if}

    {#if tab === "holdings"}
    {#if snapshot.positions.length}
      <table class="holdings">
        <thead>
          <tr><th>Holding</th><th class="num">Units</th><th class="num">Price</th><th class="num">Value</th></tr>
        </thead>
        <tbody>
          {#each snapshot.positions as p (p.ticker + p.exchange)}
            <tr>
              <td>
                <strong>{p.ticker}</strong>
                {#if p.name}<span class="faint small"> · {p.name}</span>{/if}
              </td>
              <td class="num tabular">{p.quantity.toLocaleString(undefined, { maximumFractionDigits: 4 })}</td>
              <td class="num tabular">
                {#if p.price}{formatMoney(Math.round(Number(p.price) * 100), p.currency_code)}{:else}<span class="faint">—</span>{/if}
              </td>
              <td class="num tabular">
                {#if p.market_value_minor != null}{formatMoney(p.market_value_minor, p.currency_code)}{:else}<span class="faint">—</span>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <div class="small faint" style="padding:6px 0">
        No holdings yet — add a trade under Activity{importable
          ? ", or import a platform export there"
          : ""}.
      </div>
    {/if}

    {#if snapshot.wallets.length}
      <div class="wallets">
        <span class="faint small">Cash wallets</span>
        {#each snapshot.wallets as w (w.currency_code)}
          <span class="wallet tabular">{formatMoney(w.amount_minor, w.currency_code)}</span>
        {/each}
      </div>
    {/if}
    {/if}
  {/if}
</div>

<style>
  .brokerage {
    /* Same collision EquityPanel documents: `--surface-2` is the same colour as `--bg-elev` in
       both palettes, so a fill built on it vanishes inside this panel. The lot-kind chips and
       the inset form below were drawing nothing at all. */
    --inset: color-mix(in srgb, var(--text) 5%, transparent);

    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
    margin: 2px 0 12px;
  }
  .badge {
    background: var(--inset);
  }
  table.holdings {
    width: 100%;
    border-collapse: collapse;
    font-size: 14px;
  }
  table.holdings th {
    text-align: left;
    font-weight: 550;
    color: var(--text-muted);
    padding: 4px 8px;
    border-bottom: 1px solid var(--border);
  }
  table.holdings td {
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
  }
  table.holdings tr:last-child td {
    border-bottom: none;
  }
  .num {
    text-align: right;
  }
  .wallets {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--border);
  }
  .wallet {
    background: var(--surface-2);
    border-radius: 999px;
    padding: 2px 10px;
    font-size: 13px;
  }
  .line {
    padding: 5px 0;
    border-top: 1px solid var(--border);
  }
  .sub {
    margin: 8px 0 2px;
    padding: 10px;
    background: var(--inset);
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .link {
    background: none;
    border: 0;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    font: inherit;
  }
  .link.danger {
    color: var(--negative);
  }
</style>
