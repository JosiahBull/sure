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
  import { api, formatMoney, type Schemas } from "../../lib/api";

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
      case "deterministic":
        return "amortisation schedule";
      case "insufficient_history":
        return "not enough history";
      case "modelled_from_income":
        return "modelled from income streams";
      case "contribution_driven":
        return "receives contributions — measured rate set aside";
    }
  }

  /**
   * "amortisation schedule" alone says nothing about *which* schedule. Spell out the roll-off,
   * because the refix rate — and how unsure of it we are — is what the band around a mortgage is
   * actually made of.
   */
  function scheduleLabel(a: ResolvedAssumption): string {
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
  let editForm = $state({ growth: "0", volatility: "0", dividendYield: "0", longRun: "0" });

  function startEdit(a: ResolvedAssumption) {
    editingKey = `${a.target_type}:${a.target_id}`;
    editForm = {
      growth: (a.annual_growth_bps / 100).toString(),
      volatility: (a.annual_volatility_bps / 100).toString(),
      dividendYield: ((a.dividend_yield_bps ?? 0) / 100).toString(),
      longRun: (a.long_run_growth_bps / 100).toString(),
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
  <section class="card">
    <div class="card-title">
      <h2>Assumptions</h2>
      <span class="muted small"
        >tune any of these — clear an override to go back to the derived default</span
      >
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
                </div>
              {/if}
            </div>
            <div class="a-meta row spread">
              <span class="small faint">
                {scheduleLabel(a)}{#if decayNote(a)}<span class="faint"> · {decayNote(a)}</span
                  >{/if}{#if needsReturn(a)}<span class="needs-return">
                    · set an expected return, or this stays flat</span
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
  .needs-return {
    color: var(--warn);
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
