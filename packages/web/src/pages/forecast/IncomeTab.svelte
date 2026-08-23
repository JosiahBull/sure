<script lang="ts">
  // Who earns what, grouped by person — the projection-facing readout.
  //
  // The reconciliation panels (modelled vs recorded) are the point of this screen. The streams
  // themselves are *configured under Household*, where the household lives, alongside the pay
  // matching that checks each payday off against its deposit — this tab reads them and links
  // there, so the forecast page never grows a second editor to drift from the first.
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../../lib/api";
  import { people, ensureLoaded, personColor, initials } from "../../lib/people.svelte";

  type IncomeStream = Schemas["IncomeStream"];

  let {
    result,
    currency,
    onchanged,
  }: {
    result: Schemas["ForecastResult"] | null;
    currency: string;
    onchanged?: () => void;
  } = $props();

  let streams = $state<IncomeStream[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let indexing = $state(false);

  // Streams that will be projected at a frozen nominal level. The forecast already warns about
  // these, but a warning that names a problem in a panel three tabs away from its fix is a
  // warning most people scroll past — so the action lives here, on the page that owns the data.
  const frozen = $derived(
    streams.filter((s) => s.enabled && !s.inflation_indexed && s.annual_increase_bps === 0)
  );

  /**
   * Turn on indexation for every frozen stream at once.
   *
   * A bulk action rather than six visits to the editor, because the frozen-ness is not a per-stream
   * opinion anybody formed — it is the column's default, applied to rows that predate it. What is
   * being corrected is the same mistake in every row, so it should be one decision.
   *
   * A full PUT per stream, because that is the only writer the API offers and it is a full replace.
   * Sequential rather than concurrent: six requests is not worth the failure semantics of a partial
   * `Promise.all`, and a serial loop can stop at the first error with everything before it saved.
   */
  async function indexAll() {
    indexing = true;
    for (const s of frozen) {
      const { error: e } = await api.PUT("/api/income-streams/{id}", {
        params: { path: { id: s.id } },
        body: {
          ownership: s.ownership,
          label: s.label,
          employer: s.employer,
          currency_code: s.currency_code,
          annual_amount_minor: s.annual_amount_minor,
          basis: s.basis,
          pay_frequency: s.pay_frequency,
          first_payment_on: s.first_payment_on,
          starts_on: s.starts_on,
          ends_on: s.ends_on,
          annual_increase_bps: s.annual_increase_bps,
          inflation_indexed: true,
          kiwisaver_bps: s.kiwisaver_bps,
          employer_kiwisaver_bps: s.employer_kiwisaver_bps,
          student_loan: s.student_loan,
          take_home_bps: s.take_home_bps,
          linked_category_id: s.linked_category_id,
          kiwisaver_account_id: s.kiwisaver_account_id,
          student_loan_account_id: s.student_loan_account_id,
          match_account_id: s.match_account_id,
          match_pattern: s.match_pattern,
          pay_treatment: s.pay_treatment,
          enabled: s.enabled,
          sort_order: s.sort_order,
          notes: s.notes,
          steps: s.steps.map((st) => ({
            effective_on: st.effective_on,
            annual_amount_minor: st.annual_amount_minor,
            label: st.label,
          })),
        },
      });
      if (e) {
        error = `Failed to index ${s.label}.`;
        break;
      }
    }
    indexing = false;
    await load();
    onchanged?.();
  }

  async function load() {
    loading = true;
    const { data, error: e } = await api.GET("/api/income-streams", {});
    streams = data ?? [];
    error = e ? "Failed to load income." : null;
    loading = false;
  }
  onMount(async () => {
    await ensureLoaded();
    await load();
  });

  // Keyed by person id, with `null` for the household's own income — rent from a flatmate
  // belongs to no one person, so it gets its own group rather than being filed under whichever
  // person happened to be first.
  const byPerson = $derived.by(() => {
    const m = new Map<number | null, IncomeStream[]>();
    for (const s of streams) {
      const key = s.ownership.kind === "person" ? s.ownership.person_id : null;
      const list = m.get(key);
      if (list) list.push(s);
      else m.set(key, [s]);
    }
    return m;
  });

  const reconByPerson = $derived.by(() => {
    const m = new Map<number | null, Schemas["StreamReconciliation"][]>();
    for (const r of result?.reconciliations ?? []) {
      const key = r.person_id ?? null;
      const list = m.get(key);
      if (list) list.push(r);
      else m.set(key, [r]);
    }
    return m;
  });

  // People, then the household itself when it has income of its own — rent from a flatmate
  // belongs to no one person, and filing it under whoever came first would put a figure on this
  // page that is not true of them.
  const owners = $derived.by(() => {
    const rows: { key: number | null; name: string; color: string; badge: string }[] =
      people.list.map((p) => ({
        key: p.id,
        name: p.name,
        color: personColor(p),
        badge: initials(p.name),
      }));
    const joint = (byPerson.get(null) ?? []).length + (reconByPerson.get(null) ?? []).length;
    if (joint > 0) {
      rows.push({ key: null, name: "Household", color: "var(--muted, #8a8f98)", badge: "HH" });
    }
    return rows;
  });

  function basisLabel(b: Schemas["IncomeBasis"]): string {
    switch (b) {
      case "net":
        return "take-home";
      case "gross_nz_paye":
        return "before tax";
    }
  }
  function freqLabel(f: Schemas["PayFrequency"]): string {
    switch (f) {
      case "weekly":
        return "weekly";
      case "fortnightly":
        return "fortnightly";
      case "four_weekly":
        return "every 4 weeks";
      case "semi_monthly":
        return "twice a month";
      case "monthly":
        return "monthly";
      case "quarterly":
        return "quarterly";
      case "annual":
        return "yearly";
    }
  }

  /**
   * How far the modelled figure is from what the category actually recorded.
   *
   * The hint, not the number, is what catches the mistake. A modelled figure roughly a fifth to a
   * half above the observed one is the exact signature of a salary entered before tax and modelled
   * as take-home at NZ marginal rates, so that band says so by name instead of leaving someone to
   * stare at "+31%".
   */
  function reconHint(r: Schemas["StreamReconciliation"]): string | null {
    const over = r.coverage_bps - 10_000;
    if (Math.abs(over) < 500) return null;
    if (over >= 1_800 && over <= 5_000) {
      return "That gap is about the size of PAYE — check whether a salary entered before tax is being modelled as take-home.";
    }
    if (over > 0) {
      return "The streams claim more than this category has ever recorded. Either a figure is too high, or one of them is linked to the wrong category.";
    }
    return "This category recorded more than the streams explain. The remainder is still projected from its own trend.";
  }
  function severity(r: Schemas["StreamReconciliation"]): "ok" | "warn" | "bad" {
    const off = Math.abs(r.coverage_bps - 10_000);
    if (off < 500) return "ok";
    return off < 1_500 ? "warn" : "bad";
  }
</script>

{#if error}<div class="error-banner" style="margin-bottom:16px">{error}</div>{/if}

{#if frozen.length > 0}
  <!-- The other half of the inflation decision. Indexing spending while income stays frozen in
       nominal terms removes the old overshoot and keeps the deficit: net worth still flattens,
       just for a new reason. So this is offered as one action rather than left to be discovered
       six times over. -->
  <div class="error-banner warn-banner frozen-banner" style="margin-bottom:16px">
    <div>
      <strong
        >{frozen.length === 1 ? "1 stream is" : `${frozen.length} streams are`} not indexed.</strong
      >
      {frozen.map((s) => s.label).join(", ")} —
      {frozen.length === 1 ? "it is" : "they are"} projected at today's level for the whole
      horizon, while spending rises with inflation. Over thirty years that is a projection of a
      compounding real pay cut.
    </div>
    <button class="btn btn-sm btn-primary" onclick={indexAll} disabled={indexing}>
      {indexing ? "Indexing…" : "Index with inflation"}
    </button>
  </div>
{/if}

{#if result?.unmodelled_streams.length}
  <!-- Left out of the projection entirely rather than counted at a guessed rate. Naming them is
       the whole point: a figure you can see is incomplete beats one you cannot. -->
  <div class="error-banner warn-banner" style="margin-bottom:16px">
    <strong>Not in the projection:</strong>
    {result.unmodelled_streams.join("; ")}
  </div>
{/if}

{#if loading && streams.length === 0}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else if people.list.length === 0}
  <div class="empty">
    The forecast attributes income to people. <a href="#/settings/household">Add the household</a>
    first.
  </div>
{:else}
  {#if streams.length === 0}
    <div class="empty" style="margin-bottom:16px">
      No income recorded yet. A stream is one salary or payment — what it pays, how often, and any
      steps you already know about; the forecast uses these instead of guessing from your bank
      statements. <a href="#/settings/household">Record it under Household.</a>
    </div>
  {/if}

  <div class="grid cards">
    {#each owners as p (p.key ?? "household")}
      {@const mine = byPerson.get(p.key) ?? []}
      {@const recon = reconByPerson.get(p.key) ?? []}
      <section class="card person-card" style="--who:{p.color}">
        <div class="card-title">
          <div class="row" style="gap:10px;min-width:0">
            <span class="avatar" style="background:{p.color}">{p.badge}</span>
            <h2 style="margin:0">{p.name}</h2>
          </div>
          <a class="btn btn-sm" href="#/settings/household">Configure</a>
        </div>

        {#each recon as r (r.category_id)}
          <div class="recon {severity(r)}">
            <div class="recon-pair">
              <div class="stat">
                <span class="label">Modelled, {r.category_label}</span>
                <span class="value tabular">{formatMoney(r.modelled_net_minor, currency)}/mo</span>
              </div>
              <div class="stat">
                <span class="label">Recorded, last 12 months</span>
                <span class="value tabular">{formatMoney(r.observed_net_minor, currency)}/mo</span>
              </div>
              <div class="stat">
                <span class="label">Covered</span>
                <span class="value tabular">{(r.coverage_bps / 100).toFixed(0)}%</span>
              </div>
            </div>
            {#if reconHint(r)}<div class="recon-hint small">{reconHint(r)}</div>{/if}
          </div>
        {/each}

        {#if mine.length === 0}
          <!-- Never hidden: an empty card is how you notice you forgot someone. -->
          <div class="empty">No income recorded for {p.name}.</div>
        {:else}
          <div class="stream-list">
            {#each mine as s (s.id)}
              <div class="stream-row" class:off={!s.enabled}>
                <div class="row spread">
                  <span class="row" style="gap:8px;min-width:0">
                    <span class="ell" style="font-weight:560">{s.label}</span>
                    <span class="badge">{basisLabel(s.basis)}</span>
                    {#if s.employer}<span class="faint small ell">{s.employer}</span>{/if}
                  </span>
                  <div class="row" style="gap:12px">
                    <span class="tabular small"
                      >{formatMoney(s.annual_amount_minor, s.currency_code)}/yr</span
                    >
                    <span class="faint small">{freqLabel(s.pay_frequency)}</span>
                  </div>
                </div>
                {#if s.steps.length > 0}
                  <div class="steps small faint">
                    {s.steps.length} scheduled {s.steps.length === 1 ? "step" : "steps"} · next
                    {s.steps[0].effective_on} at {formatMoney(
                      s.steps[0].annual_amount_minor,
                      s.currency_code
                    )}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

      </section>
    {/each}
  </div>
{/if}

<style>
  .frozen-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
  }
  /* Explicit basis so the action sits beside the sentence rather than being wrapped below it: a
     paragraph with no basis claims the whole row and pushes the button onto its own line, which
     reads as an afterthought rather than as the thing to do. */
  .frozen-banner > div {
    flex: 1 1 420px;
  }
  .frozen-banner .btn {
    flex: none;
  }

  .cards {
    gap: 16px;
  }
  .person-card {
    border-left: 3px solid var(--who);
  }
  .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    color: #fff;
    font-size: 11px;
    font-weight: 700;
    flex: none;
  }
  .recon {
    padding: 10px 12px;
    border-radius: var(--r-sm);
    border: 1px solid var(--border);
    background: var(--surface-2);
    margin-bottom: 12px;
  }
  /* Coloured only when there is something to say — a permanent amber panel is wallpaper. */
  .recon.warn {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
    background: color-mix(in srgb, var(--warn) 7%, var(--surface-2));
  }
  .recon.bad {
    border-color: color-mix(in srgb, var(--negative) 45%, var(--border));
    background: color-mix(in srgb, var(--negative) 7%, var(--surface-2));
  }
  .recon-pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: 12px;
  }
  .recon-hint {
    margin-top: 8px;
    color: var(--text-muted);
  }
  .stream-list {
    display: flex;
    flex-direction: column;
  }
  .stream-row {
    padding: 10px 0;
    border-top: 1px solid var(--border);
  }
  .stream-row:first-child {
    border-top: none;
  }
  .stream-row.off {
    opacity: 0.55;
  }
  .steps {
    margin-top: 3px;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .warn-banner {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
    background: color-mix(in srgb, var(--warn) 8%, transparent);
    color: var(--text);
  }
</style>
