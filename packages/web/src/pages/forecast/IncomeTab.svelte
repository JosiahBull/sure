<script lang="ts">
  // Who earns what, grouped by person.
  //
  // The reconciliation readout at the top of each card is the point of this screen, and it is why
  // editing is an inline row rather than a modal: people come here because the modelled figure does
  // not match reality, and a dialog covering the number they are correcting toward is hostile.
  // Flipping "before tax" to "take-home" and watching the difference converge is the whole
  // interaction. (`Forecast.svelte`'s own assumption editor and `Household.svelte` both already
  // expand in place for the same reason.)
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../../lib/api";
  import { people, ensureLoaded, personColor, initials } from "../../lib/people.svelte";
  import IncomeStreamEditor from "./IncomeStreamEditor.svelte";

  type IncomeStream = Schemas["IncomeStream"];

  let {
    result,
    currency,
    onchanged,
  }: {
    result: Schemas["ForecastResult"] | null;
    currency: string;
    onchanged: () => void;
  } = $props();

  let streams = $state<IncomeStream[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  /** A stream id, or `{ person }` while adding. Only one editor is open at a time. */
  let editing = $state<number | null>(null);
  let addingFor = $state<number | null>(null);
  let confirmDelete = $state<number | null>(null);
  let delError = $state<string | null>(null);

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

  /** Reload both this list and the projection above — the chart has to follow. */
  async function saved() {
    editing = null;
    addingFor = null;
    await load();
    onchanged();
  }

  async function remove(id: number) {
    const { error: e, response } = await api.DELETE("/api/income-streams/{id}", {
      params: { path: { id } },
    });
    if (e) {
      // The 409 names the forecast changes still pointing at this stream. Rendered verbatim:
      // `ErrorDetail` is `{ code, message }` with no structured payload, so there is nothing to
      // parse and pretending otherwise would break the moment the wording changed.
      delError =
        response.status === 409
          ? ((e as { error?: { message?: string } }).error?.message ?? "Still in use.")
          : "Failed to remove this income.";
      return;
    }
    confirmDelete = null;
    delError = null;
    await saved();
  }

  const byPerson = $derived.by(() => {
    const m = new Map<number, IncomeStream[]>();
    for (const s of streams) {
      const list = m.get(s.person_id);
      if (list) list.push(s);
      else m.set(s.person_id, [s]);
    }
    return m;
  });

  const reconByPerson = $derived.by(() => {
    const m = new Map<number, Schemas["StreamReconciliation"][]>();
    for (const r of result?.reconciliations ?? []) {
      const list = m.get(r.person_id);
      if (list) list.push(r);
      else m.set(r.person_id, [r]);
    }
    return m;
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
      steps you already know about. The forecast uses these instead of guessing from your bank
      statements.
    </div>
  {/if}

  <div class="grid cards">
    {#each people.list as p (p.id)}
      {@const mine = byPerson.get(p.id) ?? []}
      {@const recon = reconByPerson.get(p.id) ?? []}
      <section class="card person-card" style="--who:{personColor(p)}">
        <div class="card-title">
          <div class="row" style="gap:10px;min-width:0">
            <span class="avatar" style="background:{personColor(p)}">{initials(p.name)}</span>
            <h2 style="margin:0">{p.name}</h2>
          </div>
          <button class="btn btn-sm" onclick={() => ((addingFor = p.id), (editing = null))}>
            + Add income
          </button>
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
                    <button
                      class="btn btn-sm"
                      onclick={() => ((editing = editing === s.id ? null : s.id), (addingFor = null))}
                    >
                      {editing === s.id ? "Cancel" : "Edit"}
                    </button>
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
                {#if confirmDelete === s.id}
                  <div class="confirm row" style="gap:8px">
                    <span class="small">Remove this income?</span>
                    <button class="btn btn-sm btn-danger" onclick={() => remove(s.id)}>Remove</button
                    >
                    <button
                      class="btn btn-sm"
                      onclick={() => ((confirmDelete = null), (delError = null))}>Keep</button
                    >
                  </div>
                  {#if delError}<div class="error-banner small">{delError}</div>{/if}
                {/if}
                {#if editing === s.id}
                  <IncomeStreamEditor
                    stream={s}
                    personId={p.id}
                    onsaved={saved}
                    oncancel={() => (editing = null)}
                    ondelete={() => (confirmDelete = s.id)}
                  />
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        {#if addingFor === p.id}
          <IncomeStreamEditor
            stream={null}
            personId={p.id}
            onsaved={saved}
            oncancel={() => (addingFor = null)}
          />
        {/if}
      </section>
    {/each}
  </div>
{/if}

<style>
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
  .confirm {
    margin-top: 8px;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--negative) 8%, var(--surface-2));
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
