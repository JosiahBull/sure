<script lang="ts">
  // Each person's income, configured where the household lives. The Forecast → Income tab keeps
  // the projection-facing readout (modelled vs recorded); the streams themselves — what someone
  // earns, how it is taxed, and how its deposits are recognised — are household facts and are
  // edited here. Editing expands in place, the same convention as the people table above.
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../../lib/api";
  import { people, personColor, initials } from "../../lib/people.svelte";
  import IncomeStreamEditor from "../forecast/IncomeStreamEditor.svelte";

  type IncomeStream = Schemas["IncomeStream"];

  let { onchanged }: { onchanged?: () => void } = $props();

  let streams = $state<IncomeStream[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  /** A stream id, or `addingFor` a person, never both — one editor open at a time. */
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
  onMount(load);

  async function saved() {
    editing = null;
    addingFor = null;
    await load();
    onchanged?.();
  }

  async function remove(id: number) {
    const { error: e, response } = await api.DELETE("/api/income-streams/{id}", {
      params: { path: { id } },
    });
    if (e) {
      // The 409 names the forecast changes still pointing at this stream — shown verbatim.
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
</script>

<div class="row spread wrap" style="margin:22px 0 12px;gap:10px">
  <div>
    <h2 style="margin:0">Income</h2>
    <div class="muted small">
      What each person earns, and how their pay is recognised when it lands. The forecast and the
      cash-flow chart both read these.
    </div>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if loading && streams.length === 0}
  <div class="row" style="justify-content:center;padding:24px"><span class="spinner"></span></div>
{:else}
  <div class="grid cards">
    {#each people.list as p (p.id)}
      {@const mine = byPerson.get(p.id) ?? []}
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
                    {#if s.pay_treatment === "extra_pay"}<span class="badge">bonus</span>{/if}
                    {#if s.match_account_id != null}
                      <span class="badge matched-badge">auto-matched</span>
                    {:else if s.enabled}
                      <!-- The absence of a badge proved too quiet a signal that matching is off. -->
                      <span class="badge">not matched</span>
                    {/if}
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
  .matched-badge {
    border-color: color-mix(in srgb, var(--positive) 50%, var(--border));
    color: var(--positive);
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
</style>
