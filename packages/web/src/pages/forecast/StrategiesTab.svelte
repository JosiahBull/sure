<script lang="ts">
  // What the household does with money it does not spend, over a window.
  //
  // Its own tab rather than a section of Assumptions, because a strategy is a *decision* rather than
  // an estimate: the rest of that page is figures the model measured and you may disagree with,
  // where these are things you intend to do. They also tile time — "80% of one salary until March
  // 2028, then 50%" is two rows, not one row with a schedule inside it — so the list needs room to
  // read chronologically.
  //
  // Owns its own edit state and calls the API directly, reporting back through `onchanged` so the
  // page re-runs the simulation. The same arrangement `AssumptionsTab` uses.
  import { onMount } from "svelte";
  import { api, formatDate, type Schemas } from "../../lib/api";

  type Strategy = Schemas["InvestmentStrategy"];

  let {
    result,
    onchanged,
    onerror,
  }: {
    result: Schemas["ForecastResult"] | null;
    onchanged: () => void;
    onerror: (message: string) => void;
  } = $props();

  let strategies = $state<Strategy[]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let streams = $state<Schemas["IncomeStream"][]>([]);
  let loading = $state(true);

  async function load() {
    const [s, a, i] = await Promise.all([
      api.GET("/api/investment-strategies", {}),
      api.GET("/api/accounts", {}),
      api.GET("/api/income-streams", {}),
    ]);
    strategies = s.data ?? [];
    accounts = a.data ?? [];
    streams = i.data ?? [];
    loading = false;
  }
  onMount(load);

  const accountName = (id: number) => accounts.find((a) => a.id === id)?.name ?? `#${id}`;
  const streamName = (id: number | null | undefined) =>
    id == null ? "all income" : (streams.find((s) => s.id === id)?.label ?? `#${id}`);

  /**
   * The return a strategy will actually earn, read off the *target account's* assumption rather
   * than off the strategy.
   *
   * There is deliberately no rate on a strategy — see `InvestmentStrategy::target_account_id`. The
   * consequence is that the rate has to be shown here anyway, because "invest the surplus" is not a
   * plan until you know what it is assumed to earn, and a reader should not have to visit another
   * tab to find out.
   */
  function targetReturn(id: number): number | null {
    const a = result?.assumptions.find(
      (x) => x.target_type === "account" && x.target_id === id
    );
    return a ? a.annual_growth_bps : null;
  }

  const pct = (bps: number) => `${bps >= 0 ? "" : "-"}${Math.abs(bps / 100).toFixed(1)}%`;

  // ---- editing --------------------------------------------------------------------
  let editing = $state<number | "new" | null>(null);
  let form = $state(blank());
  let saving = $state(false);

  function blank() {
    return {
      label: "",
      active_from: new Date().toISOString().slice(0, 10),
      active_to: "",
      income_share: "0",
      income_stream_id: null as number | null,
      windfall_share: "100",
      debt_above: "",
      target_account_id: null as number | null,
      enabled: true,
      notes: "",
    };
  }

  function startNew() {
    editing = "new";
    form = blank();
    // Pre-pointed at the first investment account, because a strategy with nowhere to invest is
    // the one shape the API refuses and the commonest way to fill this in wrong.
    form.target_account_id = accounts.find((a) => a.class === "investment")?.id ?? null;
  }

  function startEdit(s: Strategy) {
    editing = s.id;
    form = {
      label: s.label,
      active_from: s.active_from,
      active_to: s.active_to ?? "",
      income_share: (s.income_share_bps / 100).toString(),
      income_stream_id: s.income_stream_id ?? null,
      windfall_share: (s.windfall_share_bps / 100).toString(),
      debt_above: s.debt_above_bps == null ? "" : (s.debt_above_bps / 100).toString(),
      target_account_id: s.target_account_id,
      enabled: s.enabled,
      notes: s.notes ?? "",
    };
  }

  async function save() {
    if (form.target_account_id == null) {
      onerror("Choose an account for the strategy to invest into.");
      return;
    }
    const body: Schemas["SaveInvestmentStrategy"] = {
      label: form.label,
      active_from: form.active_from,
      active_to: form.active_to || null,
      income_share_bps: Math.round(parseFloat(form.income_share || "0") * 100),
      income_stream_id: form.income_stream_id,
      windfall_share_bps: Math.round(parseFloat(form.windfall_share || "0") * 100),
      // Empty means "leave debt alone", which is a different statement from 0% ("pay off
      // everything"), so it must not collapse to a number.
      debt_above_bps: form.debt_above.trim() === "" ? null : Math.round(parseFloat(form.debt_above) * 100),
      target_account_id: form.target_account_id,
      enabled: form.enabled,
      notes: form.notes || null,
    };
    saving = true;
    const { error: e } =
      editing === "new"
        ? await api.POST("/api/investment-strategies", { body })
        : await api.PUT("/api/investment-strategies/{id}", {
            params: { path: { id: editing as number } },
            body,
          });
    saving = false;
    if (e) {
      onerror("Failed to save the strategy.");
      return;
    }
    editing = null;
    await load();
    onchanged();
  }

  async function remove(s: Strategy) {
    const { error: e } = await api.DELETE("/api/investment-strategies/{id}", {
      params: { path: { id: s.id } },
    });
    if (e) {
      onerror("Failed to delete the strategy.");
      return;
    }
    await load();
    onchanged();
  }
</script>

<div class="grid cards">
  <section class="card">
    <div class="card-title">
      <h2>Investment strategies</h2>
      <button class="btn btn-sm btn-primary" onclick={startNew}>New strategy</button>
    </div>

    {#if loading}
      <div class="row" style="justify-content:center;padding:32px">
        <span class="spinner"></span>
      </div>
    {:else if strategies.length === 0 && editing !== "new"}
      <div class="empty">
        Nothing set up yet. A strategy says what happens to money you do not spend — a share of one
        salary, or a lump sum from selling something — and where it goes. Add one for each period
        where the answer is different.
      </div>
    {/if}

    <div class="list">
      {#each strategies as s (s.id)}
        {@const ret = targetReturn(s.target_account_id)}
        <div class="row-item" class:off={!s.enabled}>
          <div class="row spread">
            <span class="row" style="gap:8px;min-width:0">
              <span style="font-weight:560">{s.label}</span>
              {#if !s.enabled}<span class="badge">off</span>{/if}
            </span>
            <span class="small faint tabular">
              {formatDate(s.active_from)} →
              {#if s.active_to}{formatDate(s.active_to)}{:else}open-ended{/if}
            </span>
          </div>
          <div class="small faint plan">
            {#if s.income_share_bps > 0}
              <span
                >Invest <strong>{pct(s.income_share_bps)}</strong> of {streamName(
                  s.income_stream_id
                )}</span
              >
            {/if}
            {#if s.windfall_share_bps > 0}
              <span
                >· <strong>{pct(s.windfall_share_bps)}</strong> of any money raised by a sale</span
              >
            {/if}
            {#if s.debt_above_bps != null}
              <span>· pay off debt above <strong>{pct(s.debt_above_bps)}</strong> first</span>
            {/if}
            <span>
              · into <strong>{accountName(s.target_account_id)}</strong>
              {#if ret != null}
                at {pct(ret)}/yr
              {:else}
                <span class="warn-text">— no return set on that account</span>
              {/if}
            </span>
          </div>
          <div class="row" style="gap:6px;margin-top:6px">
            <button class="btn btn-sm" onclick={() => (editing === s.id ? (editing = null) : startEdit(s))}>
              {editing === s.id ? "Cancel" : "Edit"}
            </button>
            <button class="btn btn-sm" onclick={() => remove(s)}>Delete</button>
          </div>
          {#if editing === s.id}
            {@render editor()}
          {/if}
        </div>
      {/each}
      {#if editing === "new"}
        <div class="row-item">
          {@render editor()}
        </div>
      {/if}
    </div>
  </section>
</div>

{#snippet editor()}
  <div class="edit-form">
    <label class="field wide">
      <span class="small faint">What is this?</span>
      <input class="input" placeholder="e.g. Save most of one salary" bind:value={form.label} />
    </label>
    <label class="field">
      <span class="small faint">From</span>
      <input class="input" type="date" bind:value={form.active_from} />
    </label>
    <label class="field">
      <span class="small faint">Until (blank = open)</span>
      <input class="input" type="date" bind:value={form.active_to} />
    </label>
    <label class="field">
      <span class="small faint">% of income</span>
      <input class="input tabular" bind:value={form.income_share} />
    </label>
    <label class="field">
      <span class="small faint">Whose income</span>
      <select class="input" bind:value={form.income_stream_id}>
        <option value={null}>All income</option>
        {#each streams as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
      </select>
    </label>
    <label class="field">
      <span class="small faint">% of a windfall</span>
      <input class="input tabular" bind:value={form.windfall_share} />
    </label>
    <label class="field">
      <span class="small faint">Pay off debt above %/yr</span>
      <input class="input tabular" placeholder="leave debt alone" bind:value={form.debt_above} />
    </label>
    <label class="field">
      <span class="small faint">Invest into</span>
      <select class="input" bind:value={form.target_account_id}>
        {#each accounts.filter((a) => !a.archived) as a (a.id)}
          <option value={a.id}>{a.name}</option>
        {/each}
      </select>
    </label>
    <label class="field check">
      <span class="small faint">Active</span>
      <input type="checkbox" bind:checked={form.enabled} />
    </label>
    <button class="btn btn-primary btn-sm" onclick={save} disabled={saving}>
      {saving ? "Saving…" : "Save"}
    </button>
  </div>
  <p class="small faint hint">
    A strategy invests what is <em>left</em> after spending, pay and repayments have settled, and
    never more cash than there is — so a share you cannot afford is quietly smaller rather than
    driving the account negative. Overlapping windows both apply.
  </p>
{/snippet}

<style>
  .list {
    display: flex;
    flex-direction: column;
  }
  .row-item {
    padding: 12px 0;
    border-top: 1px solid var(--border);
  }
  .row-item:first-child {
    border-top: none;
  }
  /* A disabled strategy is still worth reading — it is a plan you turned off, not a mistake. */
  .off {
    opacity: 0.55;
  }
  .plan {
    margin-top: 3px;
    line-height: 1.55;
  }
  .plan strong {
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .warn-text {
    color: var(--warn);
  }
  .edit-form {
    display: flex;
    gap: 10px;
    align-items: flex-end;
    flex-wrap: wrap;
    margin-top: 10px;
    padding: 12px;
    background: var(--surface-2);
    border-radius: var(--r-sm);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field .input {
    width: 132px;
  }
  .field.wide .input {
    width: 240px;
  }
  .field.check {
    justify-content: flex-end;
  }
  .hint {
    margin: 8px 0 0;
    max-width: 62ch;
    line-height: 1.55;
  }
</style>
