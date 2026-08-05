<script lang="ts">
  // What the projection assumed, and the two ways to disagree with it: an override on a rate,
  // or a dated change you are certain about.
  //
  // Owns its own edit state and calls the API directly, reporting back through `onchanged` so
  // the page can re-run the simulation — the `oncreated` arrangement the modals already use.
  // The alternative, lifting `editingKey`/`editForm` into the page, would put state there that
  // only this tab can interpret.
  import { api, formatMoney, formatDate, type Schemas } from "../../lib/api";

  type ResolvedAssumption = Schemas["ResolvedAssumption"];
  type ForecastEvent = Schemas["ForecastEvent"];

  let {
    result,
    events,
    currency,
    onchanged,
    onerror,
  }: {
    result: Schemas["ForecastResult"] | null;
    events: ForecastEvent[];
    currency: string;
    onchanged: () => void;
    onerror: (message: string) => void;
  } = $props();

  // Only targets the simulation actually resolved an assumption for — excludes cash (pooled)
  // and everyday transaction accounts, so the form can't be pointed at a target that would
  // silently have no effect.
  const targets = $derived(
    (result?.assumptions ?? []).map((a) => ({
      key: `${a.target_type}:${a.target_id}`,
      target_type: a.target_type,
      target_id: a.target_id,
      label: a.label,
    }))
  );
  const targetLabels = $derived(new Map(targets.map((t) => [t.key, t.label])));
  function targetLabel(e: ForecastEvent): string {
    return targetLabels.get(`${e.target_type}:${e.target_id}`) ?? `#${e.target_id}`;
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
      case "deterministic":
        return "amortisation schedule";
      case "insufficient_history":
        return "not enough history";
      case "modelled_from_income":
        return "modelled from income streams";
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

  // ---- certain changes ---------------------------------------------------------------
  let ef = $state({
    targetKey: "",
    kind: "step_change" as Schemas["ForecastEventKind"],
    effective_date: new Date().toISOString().slice(0, 10),
    amount: "",
    label: "",
  });

  async function addEvent() {
    if (!ef.targetKey || !ef.label.trim() || !ef.amount) return;
    const [target_type, target_id] = ef.targetKey.split(":") as [
      Schemas["ForecastTargetType"],
      string,
    ];
    const body: Schemas["SaveForecastEvent"] = {
      target_type,
      target_id: Number(target_id),
      kind: ef.kind,
      effective_date: ef.effective_date,
      amount_minor: Math.round(parseFloat(ef.amount) * 100),
      label: ef.label.trim(),
    };
    const { error: e } = await api.POST("/api/forecast/events", { body });
    if (e) {
      onerror("Failed to add the change.");
      return;
    }
    ef.label = "";
    ef.amount = "";
    onchanged();
  }
  async function deleteEvent(id: number) {
    await api.DELETE("/api/forecast/events/{id}", { params: { path: { id } } });
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

  <section class="card">
    <div class="card-title">
      <h2>Certain changes</h2>
      <span class="muted small"
        >a dated, exact adjustment — applied to every simulated path, not estimated</span
      >
    </div>
    <div class="event-form">
      <select class="select" bind:value={ef.targetKey}>
        <option value="" disabled>Target…</option>
        {#each targets as t (t.key)}<option value={t.key}>{t.label}</option>{/each}
      </select>
      <select class="select" bind:value={ef.kind}>
        <option value="step_change">New recurring baseline from date</option>
        <option value="one_off_amount">One-off amount on date</option>
      </select>
      <input class="input" type="date" bind:value={ef.effective_date} />
      <input class="input tabular" placeholder="Amount" bind:value={ef.amount} />
      <input class="input" placeholder="Label" bind:value={ef.label} />
      <button class="btn btn-primary" onclick={addEvent}>Add</button>
    </div>
    {#if result?.assumptions.length === 0}
      <div class="small faint" style="margin-top:8px">Add an account or category first.</div>
    {/if}
    <div class="event-list">
      {#each events as e (e.id)}
        <div class="line row spread">
          <span>
            <span class="badge target-badge"
              >{e.kind === "step_change" ? "step change" : "one-off"}</span
            >
            {e.label} on {targetLabel(e)}
            <span class="faint small">from {formatDate(e.effective_date)}</span>
          </span>
          <div class="row" style="gap:8px">
            <span class="tabular small">{formatMoney(e.amount_minor, currency)}</span>
            <button class="btn btn-sm btn-danger" onclick={() => deleteEvent(e.id)}>✕</button>
          </div>
        </div>
      {/each}
      {#if events.length === 0}<div class="small faint">None yet.</div>{/if}
    </div>
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
  .event-form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
    gap: 10px;
    margin-bottom: 12px;
  }
  .line {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .line:first-of-type {
    border-top: none;
  }
</style>
