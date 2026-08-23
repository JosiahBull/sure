<script lang="ts">
  // What the projection assumed, and how to disagree with a rate.
  //
  // Dated changes used to live here too. They moved to the Life events tab when the event model was
  // unified: a certainty is that model with 100% probability and no spread, so keeping a second
  // editor for the same rows would have been two places to look for one thing.
  //
  // Owns its own edit state and calls the API directly, reporting back through `onchanged` so
  // the page can re-run the simulation — the `oncreated` arrangement the modals already use.
  // The alternative, lifting `editingKey`/`editForm` into the page, would put state there that
  // only this tab can interpret.
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../../lib/api";
  import Sparkline from "../../lib/charts/Sparkline.svelte";
  import {
    people,
    ensureLoaded as ensurePeopleLoaded,
    ownershipLabel,
    ownershipColor,
    placeholders,
  } from "../../lib/people.svelte";

  type ResolvedAssumption = Schemas["ResolvedAssumption"];

  let {
    result,
    currency,
    onchanged,
    onerror,
  }: {
    result: Schemas["ForecastResult"] | null;
    currency: string;
    onchanged: () => void;
    onerror: (message: string) => void;
  } = $props();

  // Two accounts can share a name — a household with a student loan each has two rows both
  // called "Student loan" — so the owner is what tells them apart, and the roster is what turns
  // an id into a name. Loaded here rather than assumed: this tab is linkable directly.
  onMount(ensurePeopleLoaded);
  const placeholderIds = $derived(new Set(placeholders().map((p) => p.id)));

  // ---- the household inflation dial ------------------------------------------------
  //
  // One number, at the top, above the per-row list. It replaced a per-category fitted growth
  // rate, and the argument for that is in `AssumptionSource::Indexed`: 24 lumpy months pin a
  // category's mean to roughly ±15% and do not identify its trend at all, so fitting one put six
  // of seven categories on a ±25%/yr clamp nobody chose. Assuming the trend and measuring the
  // level is the honest split — and an assumption has to be *visible* to be arguable, which is
  // what this control is for.
  let inflationBps = $state<number | null>(null);
  let inflationField = $state("");
  let savingInflation = $state(false);

  onMount(async () => {
    const { data } = await api.GET("/api/settings", {});
    if (data) {
      inflationBps = data.inflation_bps;
      inflationField = (data.inflation_bps / 100).toString();
    }
  });

  const inflationDirty = $derived(
    inflationBps != null && Math.round(parseFloat(inflationField || "0") * 100) !== inflationBps
  );

  async function saveInflation() {
    const bps = Math.round(parseFloat(inflationField || "0") * 100);
    if (!Number.isFinite(bps)) {
      onerror("That is not a percentage.");
      return;
    }
    savingInflation = true;
    // The base currency has to go back with it: `UpdateSettings` requires it, and re-sending what
    // is already stored is the whole body's worth of round-trip rather than a second endpoint.
    const current = await api.GET("/api/settings", {});
    const { error: e } = await api.PUT("/api/settings", {
      body: {
        base_currency_code: current.data?.base_currency_code ?? currency,
        inflation_bps: bps,
      },
    });
    savingInflation = false;
    if (e) {
      onerror("Failed to save the inflation rate.");
      return;
    }
    inflationBps = bps;
    onchanged();
  }

  const categories = $derived((result?.assumptions ?? []).filter((a) => a.target_type === "category"));
  // The legend is only worth showing if something on the page needs it, and it belongs once in the
  // card header rather than repeated under every row that happens to have a break.
  const anyBreak = $derived(categories.some((a) => a.break_months_ago != null));
  // Only what the dial actually moves. An overridden category is the user's own figure and is
  // not re-priced by changing the household rate, so including it in the readout below would
  // overstate what the control does.
  const indexedSpendMinor = $derived(
    categories
      .filter((a) => !a.is_income && a.source === "indexed")
      .reduce((t, a) => t + (a.baseline_minor ?? 0), 0)
  );
  const years = $derived((result?.horizon_months ?? 0) / 12);
  // The single highest-value figure on this page. Nobody reading "+25.0%/yr" on a row realised it
  // meant $15,306/mo; everybody reading "$4,051/mo becomes $8,498/mo" does. It is deliberately
  // computed from the same baselines the projection ran on rather than from a second model, so it
  // cannot disagree with the chart.
  const spendAtHorizonMinor = $derived(
    indexedSpendMinor * Math.pow(1 + (inflationBps ?? 0) / 10_000, years)
  );
  const horizonLabel = $derived(
    result?.months.length ? formatDate(result.months[result.months.length - 1].as_of).slice(-4) : ""
  );

  /**
   * What a category row measured its level over, in words. The figure that says how much the rest
   * of the row is worth — and the one the old UI never showed, which is how a 24-month claim over
   * an 11-month fit stayed invisible.
   */
  function windowNote(a: ResolvedAssumption): string | null {
    if (a.target_type !== "category" || a.fitted_months == null) return null;
    const months = a.fitted_months === 1 ? "1 month" : `${a.fitted_months} months`;
    if (a.break_months_ago != null) {
      return `measured over ${months}, since spending on it changed level`;
    }
    return `measured over ${months}`;
  }

  /**
   * What this category's own history says its trend is — shown next to a projection that is not
   * using it, on purpose. A reader who cannot see the measurement cannot judge whether to
   * disagree with the assumption that replaced it.
   */
  function measuredNote(a: ResolvedAssumption): string | null {
    if (a.measured_growth_bps == null) return null;
    return `history suggests ${pct(a.measured_growth_bps)}/yr`;
  }

  function pct(bps: number): string {
    return `${bps >= 0 ? "+" : ""}${(bps / 100).toFixed(1)}%`;
  }
  function sourceLabel(s: Schemas["AssumptionSource"]): string {
    switch (s) {
      case "override":
        return "override";
      case "cron":
        return "from scheduled adjustment";
      case "derived":
        return "from history";
      case "indexed":
        return "measured level, rising with inflation";
      case "deterministic":
        return "amortisation schedule";
      case "insufficient_history":
        return "not enough history";
      case "modelled_from_income":
        return "modelled from income streams";
      case "modelled_from_schedule":
        return "loan interest comes from its schedule";
      case "contribution_driven":
        return "receives contributions — measured rate set aside";
      case "vesting_schedule":
        return "vesting schedule";
    }
  }

  /**
   * "amortisation schedule" alone says nothing about *which* schedule. Spell out the roll-off,
   * because the refix rate — and how unsure of it we are — is what the band around a mortgage is
   * actually made of.
   */
  /**
   * What the vesting projection is actually doing, spelled out for the same reason
   * `scheduleLabel` spells out a refix: "vesting schedule" alone does not say how much is still
   * to come, and that figure — units the deed guarantees, at a price nobody can trade — is the
   * whole reason this account is not projected from its own history.
   */
  function vestingLabel(a: ResolvedAssumption): string {
    const v = a.vesting;
    if (!v) return sourceLabel(a.source);
    const worth = formatMoney(v.unvested_value_minor, a.currency_code ?? currency);
    const when = v.fully_vested_on ? `, fully vested ${formatDate(v.fully_vested_on)}` : "";
    const mark =
      v.unit_value_minor != null
        ? ` · at ${formatMoney(v.unit_value_minor, a.currency_code ?? currency)} a unit${
            v.unit_value_as_of ? ` set ${formatDate(v.unit_value_as_of)}` : ""
          }`
        : "";
    return `${v.unvested_units.toLocaleString()} units still to vest, worth ${worth}${when}${mark}`;
  }

  function scheduleLabel(a: ResolvedAssumption): string {
    if (a.source === "vesting_schedule") return vestingLabel(a);
    const s = a.schedule;
    if (!s) return sourceLabel(a.source);
    if (s.refix_in_months == null || s.refix_rate_bps == null) {
      return "amortisation schedule · rate held to term";
    }
    const when = s.refix_in_months === 1 ? "next month" : `in ${s.refix_in_months} months`;
    const rate = (s.refix_rate_bps / 100).toFixed(2);
    const sd = s.refix_rate_uncertainty_bps ?? 0;
    const spread = sd > 0 ? ` ± ${(sd / 100).toFixed(2)}%` : "";
    return `amortisation schedule · refixes ${when} at ${rate}%${spread}`;
  }

  /**
   * How long a derived trend is projected before it starts decaying toward its long-run rate,
   * spelled out only where it applies. A rate the user asserted is not decayed, and an
   * amortisation schedule has no rate to decay — saying so on every row would be noise.
   */
  function decayNote(a: ResolvedAssumption): string | null {
    if (a.source !== "derived" || a.annual_growth_bps === 0) return null;
    const anchor = a.long_run_growth_bps;
    return `held 5 years, then eases toward ${pct(anchor)}/yr`;
  }

  /**
   * An account that receives contributions and has no expected return is projected flat, which is a
   * placeholder rather than an answer. Say so on the row itself — this tab is where the override
   * that fixes it lives, so the prompt belongs next to the button.
   */
  function needsReturn(a: ResolvedAssumption): boolean {
    return a.source === "contribution_driven" && a.annual_growth_bps === 0;
  }

  // ---- assumption override editing ------------------------------------------------
  let editingKey = $state<string | null>(null);
  let editForm = $state({
    growth: "0",
    volatility: "0",
    dividendYield: "0",
    longRun: "0",
    fee: "",
    fixedFee: "",
  });

  function startEdit(a: ResolvedAssumption) {
    editingKey = `${a.target_type}:${a.target_id}`;
    editForm = {
      growth: (a.annual_growth_bps / 100).toString(),
      volatility: (a.annual_volatility_bps / 100).toString(),
      dividendYield: ((a.dividend_yield_bps ?? 0) / 100).toString(),
      longRun: (a.long_run_growth_bps / 100).toString(),
      fee: a.annual_fee_bps != null ? (a.annual_fee_bps / 100).toString() : "",
      fixedFee: a.annual_fixed_fee_minor != null ? (a.annual_fixed_fee_minor / 100).toString() : "",
    };
  }
  function cancelEdit() {
    editingKey = null;
  }
  async function saveEdit(a: ResolvedAssumption) {
    const body: Schemas["SaveForecastAssumption"] = {
      target_type: a.target_type,
      target_id: a.target_id,
      annual_growth_bps: Math.round(parseFloat(editForm.growth || "0") * 100),
      annual_volatility_bps: Math.round(parseFloat(editForm.volatility || "0") * 100),
      dividend_yield_bps:
        a.dividend_yield_bps != null
          ? Math.round(parseFloat(editForm.dividendYield || "0") * 100)
          : null,
      long_run_growth_bps: Math.round(parseFloat(editForm.longRun || "0") * 100),
      // Empty means "not modelled" rather than zero — a fund that charges nothing is a claim worth
      // making on purpose, and assuming it is flattering.
      annual_fee_bps: editForm.fee.trim() === "" ? null : Math.round(parseFloat(editForm.fee) * 100),
      annual_fixed_fee_minor:
        editForm.fixedFee.trim() === "" ? null : Math.round(parseFloat(editForm.fixedFee) * 100),
    };
    const { error: e } = await api.PUT("/api/forecast/assumptions", { body });
    if (e) {
      onerror("Failed to save the override.");
      return;
    }
    editingKey = null;
    onchanged();
  }
  async function clearOverride(a: ResolvedAssumption) {
    await api.DELETE("/api/forecast/assumptions/{target_type}/{target_id}", {
      params: { path: { target_type: a.target_type, target_id: a.target_id } },
    });
    onchanged();
  }

</script>

<div class="grid cards">
  {#if inflationBps != null}
    <section class="card dial">
      <div class="card-title">
        <h2>Spending rises with inflation</h2>
        <span class="muted small">one rate, both sides of the ledger</span>
      </div>
      <div class="dial-row">
        <label class="field">
          <span class="small faint">Inflation %/yr</span>
          <input
            class="input tabular big"
            bind:value={inflationField}
            onkeydown={(e) => e.key === "Enter" && saveInflation()}
          />
        </label>
        {#if inflationDirty}
          <button class="btn btn-primary btn-sm" onclick={saveInflation} disabled={savingInflation}>
            {savingInflation ? "Saving…" : "Apply"}
          </button>
        {/if}
        <p class="dial-note small">
          Applied to every category below without an override of its own, and to any income stream
          marked as indexed.
          {#if indexedSpendMinor > 0 && years >= 1}
            <br />
            <span class="dial-figure">
              At {(inflationBps / 100).toFixed(1)}%, the {formatMoney(
                Math.round(indexedSpendMinor / 100) * 100,
                currency
              )}/mo you spend today is {formatMoney(
                Math.round(spendAtHorizonMinor / 100) * 100,
                currency
              )}/mo {#if horizonLabel}by {horizonLabel}{:else}at the horizon{/if}.
            </span>
          {/if}
        </p>
      </div>
    </section>
  {/if}

  <section class="card">
    <div class="card-title">
      <h2>Assumptions</h2>
      <span class="muted small">
        {#if anyBreak}
          <span class="spark-key">
            <span class="swatch excluded"></span> before a change in level
            <span class="swatch boundary"></span> measured from here
          </span>
          ·
        {/if}
        tune any of these — clear an override to go back to the measured default
      </span>
    </div>
    {#if !result?.assumptions.length}
      <div class="empty">
        Nothing to forecast yet — add accounts, transactions and categorise your spending.
      </div>
    {:else}
      <div class="assumption-list">
        {#each result.assumptions as a (a.target_type + ":" + a.target_id)}
          {@const key = a.target_type + ":" + a.target_id}
          <div class="assumption-row">
            <div class="a-main row spread">
              <span class="row" style="gap:8px;min-width:0">
                <span class="badge target-badge">{a.target_type}</span>
                <span class="ell" style="font-weight:560">{a.label}</span>
                {#if a.ownership && people.list.length > 0}
                  {@const color = ownershipColor(a.ownership)}
                  {@const isPlaceholder =
                    a.ownership.kind === "person" && placeholderIds.has(a.ownership.person_id)}
                  <span
                    class="badge owner"
                    class:placeholder={isPlaceholder}
                    style={color && !isPlaceholder
                      ? `border-color:${color};color:${color}`
                      : undefined}
                  >
                    {ownershipLabel(a.ownership)}
                  </span>
                {/if}
              </span>
              {#if a.schedule}
                {@const s = a.schedule}
                <div class="row" style="gap:14px">
                  <span class="tabular small">
                    {formatMoney(s.monthly_payment_minor, a.currency_code ?? currency)}/mo
                  </span>
                  <span class="tabular small faint">{(s.current_rate_bps / 100).toFixed(2)}%</span>
                  <span class="tabular small faint">{s.remaining_term_months} mo left</span>
                </div>
              {:else if a.source !== "deterministic"}
                <div class="row" style="gap:14px">
                  <span class="tabular small">growth {pct(a.annual_growth_bps)}/yr</span>
                  <span class="tabular small faint"
                    >± {(a.annual_volatility_bps / 100).toFixed(1)}%/yr</span
                  >
                  {#if a.dividend_yield_bps != null}
                    <span class="tabular small faint"
                      >yield {(a.dividend_yield_bps / 100).toFixed(1)}%</span
                    >
                  {/if}
                  {#if a.annual_fee_bps}
                    <span class="tabular small fee">− {(a.annual_fee_bps / 100).toFixed(2)}% fee</span>
                  {/if}
                </div>
              {/if}
            </div>
            <div class="a-meta row spread">
              <span class="small faint">
                {scheduleLabel(a)}{#if windowNote(a)}<span class="faint"> · {windowNote(a)}</span
                  >{/if}{#if measuredNote(a)}<span class="measured"> · {measuredNote(a)}</span
                  >{/if}{#if decayNote(a)}<span class="faint"> · {decayNote(a)}</span
                  >{/if}{#if needsReturn(a)}<span class="needs-return">
                    · set an expected return, or this stays flat</span
                  >{/if}{#if a.source === "vesting_schedule"}<span class="faint">
                    · growth below applies to the share price, not the units</span
                  >{/if}
              </span>
              {#if a.source !== "deterministic"}
                <div class="row" style="gap:6px">
                  {#if a.source === "override"}
                    <button class="btn btn-sm" onclick={() => clearOverride(a)}
                      >Clear override</button
                    >
                  {/if}
                  <button
                    class="btn btn-sm"
                    onclick={() => (editingKey === key ? cancelEdit() : startEdit(a))}
                  >
                    {editingKey === key ? "Cancel" : "Override"}
                  </button>
                </div>
              {/if}
            </div>
            {#if a.history_minor?.length}
              <!-- The evidence, next to the claim. A row that asserts a level and a growth rate
                   without showing the months they came from is unfalsifiable by the one person who
                   knows whether it is right. -->
              <div class="evidence">
                <Sparkline
                  values={a.history_minor}
                  usedMonths={a.fitted_months ?? a.history_minor.length}
                  label={`${a.label}: ${a.history_minor.length} months of history`}
                />

              </div>
            {/if}
            {#if editingKey === key}
              <div class="edit-form">
                <label class="field">
                  <span class="small faint">Growth %/yr</span>
                  <input class="input tabular" bind:value={editForm.growth} />
                </label>
                <label class="field">
                  <span class="small faint">Volatility %/yr</span>
                  <input class="input tabular" bind:value={editForm.volatility} />
                </label>
                {#if a.dividend_yield_bps != null}
                  <label class="field">
                    <span class="small faint">Dividend yield %</span>
                    <input class="input tabular" bind:value={editForm.dividendYield} />
                  </label>
                {/if}
                <label class="field">
                  <span class="small faint">Long-run %/yr</span>
                  <input class="input tabular" bind:value={editForm.longRun} />
                </label>
                {#if a.target_type === "account"}
                  <label class="field">
                    <span class="small faint">Fund fee %/yr</span>
                    <input
                      class="input tabular"
                      placeholder="not modelled"
                      bind:value={editForm.fee}
                    />
                  </label>
                  <label class="field">
                    <span class="small faint">Flat fee /yr</span>
                    <input
                      class="input tabular"
                      placeholder="not modelled"
                      bind:value={editForm.fixedFee}
                    />
                  </label>
                {/if}
                <button class="btn btn-primary btn-sm" onclick={() => saveEdit(a)}>Save</button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>

</div>

<style>
  .cards {
    gap: 16px;
  }
  .assumption-list {
    display: flex;
    flex-direction: column;
  }
  .assumption-row {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .assumption-row:first-child {
    border-top: none;
  }
  .a-meta {
    margin-top: 2px;
  }
  .target-badge {
    text-transform: capitalize;
  }
  /* The same owner badge the accounts list uses, so a name reads the same wherever it sits. */
  .owner {
    border: 1px solid var(--border);
    background: transparent;
    flex: none;
  }
  /* The one badge that's a to-do rather than a fact — it reads as a gap, not a label. */
  .owner.placeholder {
    border-style: dashed;
    color: var(--text-muted);
  }
  .needs-return {
    color: var(--warn);
  }
  /* The measured figure the projection is deliberately *not* using. Toned down from the assertion
     beside it so it reads as a footnote rather than as a competing number. */
  .measured {
    color: var(--text-muted);
    font-style: italic;
  }
  .dial-row {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
  }
  .dial-row .input.big {
    width: 96px;
    font-size: 1.35rem;
    font-weight: 560;
    /* The one control on this page that sets a number applying to a dozen rows, so it is sized
       like the decision it is rather than like the per-row override inputs below. */
    padding: 8px 10px;
  }
  .dial-note {
    flex: 1 1 340px;
    margin: 0;
    color: var(--text-muted);
    line-height: 1.55;
  }
  /* The sentence that turns a percentage into a number a household can judge. Nobody reading
     "+25.0%/yr" on a row realised it meant $15,306/mo; everybody reading "$6,308/mo becomes
     $7,137/mo" does. */
  .dial-figure {
    display: inline-block;
    margin-top: 4px;
    color: var(--text);
    font-variant-numeric: tabular-nums;
    font-weight: 560;
  }
  .evidence {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    flex-wrap: wrap;
    margin-top: 6px;
  }
  .spark-key {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .swatch {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 1px;
  }
  .swatch.excluded {
    background: var(--border-strong);
  }
  .swatch.boundary {
    background: var(--warn);
    margin-left: 8px;
  }
  .fee {
    color: var(--negative);
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .edit-form {
    display: flex;
    gap: 10px;
    align-items: flex-end;
    flex-wrap: wrap;
    margin-top: 8px;
    padding: 10px;
    background: var(--surface-2);
    border-radius: var(--r-sm);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field .input {
    width: 100px;
  }
</style>
