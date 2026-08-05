<script lang="ts">
  // One event, edited in place — also the add form, since the fields are identical.
  //
  // Two parts are genuinely hard, and both are solved by not offering the wrong thing rather than by
  // rejecting it afterwards: the relation picker only lists events that cannot close a cycle, and
  // picking a kind seeds the effects it usually has.
  import { untrack } from "svelte";
  import { api, type Schemas } from "../../lib/api";
  import { people } from "../../lib/people.svelte";
  import {
    EVENT_KINDS,
    eligibleTargets,
    seedEffects,
    seedTiming,
    type ForecastEvent,
    type LifeEffectSpec,
  } from "../../lib/forecast/lifeEvents";

  let {
    event = null,
    streams,
    categories,
    accounts,
    allEvents,
    onsaved,
    oncancel,
    ondelete,
  }: {
    event?: ForecastEvent | null;
    streams: Schemas["IncomeStream"][];
    categories: Schemas["Category"][];
    accounts: Schemas["Account"][];
    allEvents: ForecastEvent[];
    onsaved: () => void;
    oncancel: () => void;
    ondelete?: () => void;
  } = $props();

  const initial = untrack(() => event);
  const today = new Date().toISOString().slice(0, 10);

  let f = $state({
    label: initial?.label ?? "",
    kind: initial?.kind ?? ("promotion" as Schemas["LifeEventKind"]),
    person_id: initial?.person_id ?? null,
    expected_on: initial?.expected_on ?? today,
    spread: (initial?.timing_spread_months ?? 18).toString(),
    probability: ((initial?.probability_bps ?? 7_000) / 100).toString(),
    notes: initial?.notes ?? "",
  });
  // The stored row carries identity alongside the spec; the write body wants the spec alone. Copied
  // key-by-key rather than rest-destructured, because a rest on a discriminated union widens it to
  // something TypeScript will not narrow again.
  let effects = $state<LifeEffectSpec[]>(
    (initial?.effects ?? []).map((e) => {
      const out: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(e as unknown as Record<string, unknown>)) {
        if (k !== "id" && k !== "event_id" && k !== "sort_order") out[k] = v;
      }
      return out as unknown as LifeEffectSpec;
    })
  );
  let relations = $state<Schemas["SaveForecastEventRelation"][]>(
    (initial?.relations ?? []).map((r) => ({
      depends_on_event_id: r.depends_on_event_id,
      kind: r.kind,
      min_gap_months: r.min_gap_months,
    }))
  );

  let submitted = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  const labelMissing = $derived(submitted && !f.label.trim());

  const incomeCategories = $derived(categories.filter((c) => c.kind === "income"));
  const expenseCategories = $derived(categories.filter((c) => c.kind === "expense"));

  /**
   * Only the events that cannot close a loop.
   *
   * Computed client-side so a cycle is unbuildable by clicking; the server's 409 is the backstop for
   * a second tab, not the primary experience.
   */
  const eligible = $derived(eligibleTargets(allEvents, initial?.id ?? null));

  /**
   * Picking a kind seeds the effects and timing that kind usually has.
   *
   * Prompts before replacing hand-edited effects rather than silently discarding them — the seed is a
   * head start, not an opinion about what you already typed.
   */
  function changeKind(next: Schemas["LifeEventKind"]) {
    f.kind = next;
    const t = seedTiming(next);
    f.probability = (t.probability_bps / 100).toString();
    f.spread = t.spread.toString();
    const seeded = seedEffects(next, {
      personId: f.person_id ?? people.list[0]?.id ?? null,
      streamId: streams.find((s) => s.person_id === f.person_id)?.id ?? streams[0]?.id ?? null,
      categoryId: expenseCategories[0]?.id ?? null,
    });
    if (effects.length === 0) {
      effects = seeded;
      return;
    }
    if (
      seeded.length > 0 &&
      confirm(`Replace the ${effects.length} effect(s) with the usual ones for a ${next}?`)
    ) {
      effects = seeded;
    }
  }

  function addEffect(kind: string) {
    const stream = streams[0]?.id ?? 0;
    const person = f.person_id ?? people.list[0]?.id ?? 0;
    const cat = expenseCategories[0]?.id ?? incomeCategories[0]?.id ?? 0;
    const acct = accounts[0]?.id ?? 0;
    const made: Record<string, LifeEffectSpec> = {
      income_step: {
        kind: "income_step",
        income_stream_id: stream,
        amount: { basis: "percent", rate_bps: 1_000 },
      } as never,
      income_start: { kind: "income_start", income_stream_id: stream } as never,
      income_end: { kind: "income_end", income_stream_id: stream } as never,
      income_pause: {
        kind: "income_pause",
        person_id: person,
        months: 6,
        replacement_rate_bps: 0,
      } as never,
      recurring_delta: {
        kind: "recurring_delta",
        category_id: cat,
        amount_minor: 500_00,
        delay_months: 0,
        ramp_months: 0,
        duration_months: null,
      } as never,
      set_baseline: {
        kind: "set_baseline",
        target: { kind: "category", category_id: cat },
        amount_minor: 0,
      } as never,
      one_off_amount: {
        kind: "one_off_amount",
        target: { kind: "account", account_id: acct },
        amount_minor: 0,
      } as never,
    };
    effects.push(made[kind]);
  }

  /** A cast helper: the generated union is wide, and the row below switches on `kind` anyway. */
  function rec(e: LifeEffectSpec): Record<string, never> {
    return e as unknown as Record<string, never>;
  }

  async function save() {
    submitted = true;
    if (!f.label.trim()) return;
    saving = true;
    error = null;
    const body: Schemas["SaveForecastEvent"] = {
      label: f.label.trim(),
      kind: f.kind,
      person_id: f.person_id,
      expected_on: f.expected_on,
      timing_spread_months: Math.max(0, Math.round(parseFloat(f.spread || "0"))),
      probability_bps: Math.round(parseFloat(f.probability || "0") * 100),
      notes: f.notes.trim() || null,
      effects,
      relations,
    };
    const res = initial
      ? await api.PUT("/api/forecast/events/{id}", {
          params: { path: { id: initial.id } },
          body,
        })
      : await api.POST("/api/forecast/events", { body });
    saving = false;
    if (res.error) {
      // Every problem across every effect arrives in one message, so it is shown whole.
      error =
        (res.error as { error?: { message?: string } }).error?.message ??
        "Could not save this event.";
      return;
    }
    onsaved();
  }
</script>

<div class="editor">
  {#if error}<div class="error-banner small">{error}</div>{/if}

  <div class="grid-fields">
    <label class="field">
      <span class="lbl req">What is it</span>
      <input
        class="input"
        class:invalid={labelMissing}
        aria-invalid={labelMissing}
        placeholder="First child"
        bind:value={f.label}
      />
    </label>
    <label class="field">
      <span class="lbl req">Kind</span>
      <select
        class="select"
        value={f.kind}
        onchange={(e) =>
          changeKind((e.currentTarget as HTMLSelectElement).value as Schemas["LifeEventKind"])}
      >
        {#each EVENT_KINDS as k (k.kind)}<option value={k.kind}>{k.label}</option>{/each}
      </select>
    </label>
    <label class="field">
      <span class="lbl">Whose</span>
      <select class="select" bind:value={f.person_id}>
        <option value={null}>The household</option>
        {#each people.list as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
      </select>
    </label>
  </div>

  <div class="grid-fields">
    <label class="field">
      <span class="lbl req">Around when</span>
      <input class="input" type="date" bind:value={f.expected_on} />
    </label>
    <label class="field">
      <!-- A hard window: every month inside it is equally likely, and nothing lands outside. So
           "± 24" means what it says, rather than being some distribution's 90th percentile. -->
      <span class="lbl">Give or take (months)</span>
      <input class="input tabular" bind:value={f.spread} />
    </label>
    <label class="field">
      <span class="lbl req">Chance of happening %</span>
      <input class="input tabular" bind:value={f.probability} />
    </label>
  </div>

  <!-- ---- effects ---------------------------------------------------------------- -->
  <div class="section-head small">What it does</div>
  {#if effects.length === 0}
    <div class="small faint" style="margin-bottom:6px">
      Nothing yet — this event will be simulated but change no figure.
    </div>
  {/if}
  <div class="rows">
    {#each effects as e, i (i)}
      {@const r = rec(e)}
      <div class="eff-row">
        <span class="eff-kind small">{(r.kind as unknown as string).replace(/_/g, " ")}</span>

        {#if r.kind === "income_step"}
          <select class="select" bind:value={r.income_stream_id}>
            {#each streams as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
          </select>
          <select class="select" bind:value={(r.amount as never as { basis: string }).basis}>
            <option value="percent">by %</option>
            <option value="absolute">to a figure</option>
          </select>
          {#if (r.amount as never as { basis: string }).basis === "percent"}
            <input
              class="input tabular"
              value={((r.amount as never as { rate_bps?: number }).rate_bps ?? 0) / 100}
              oninput={(ev) =>
                ((r.amount as never as { rate_bps: number }).rate_bps = Math.round(
                  parseFloat((ev.currentTarget as HTMLInputElement).value || "0") * 100
                ))}
            />
            <span class="unit small faint">%</span>
          {:else}
            <input
              class="input tabular"
              value={((r.amount as never as { annual_amount_minor?: number })
                .annual_amount_minor ?? 0) / 100}
              oninput={(ev) =>
                ((r.amount as never as { annual_amount_minor: number }).annual_amount_minor =
                  Math.round(
                    parseFloat((ev.currentTarget as HTMLInputElement).value || "0") * 100
                  ))}
            />
            <span class="unit small faint">/yr</span>
          {/if}
        {:else if r.kind === "income_start" || r.kind === "income_end"}
          <select class="select grow" bind:value={r.income_stream_id}>
            {#each streams as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
          </select>
        {:else if r.kind === "income_pause"}
          <select class="select" bind:value={r.person_id}>
            {#each people.list as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select>
          <input class="input tabular" bind:value={r.months} />
          <span class="unit small faint">months at</span>
          <input
            class="input tabular"
            value={(r.replacement_rate_bps as unknown as number) / 100}
            oninput={(ev) =>
              ((r.replacement_rate_bps as unknown as number) = Math.round(
                parseFloat((ev.currentTarget as HTMLInputElement).value || "0") * 100
              ))}
          />
          <span class="unit small faint">% pay</span>
        {:else if r.kind === "recurring_delta"}
          <select class="select" bind:value={r.category_id}>
            {#each categories as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
          </select>
          <input
            class="input tabular"
            value={(r.amount_minor as unknown as number) / 100}
            oninput={(ev) =>
              ((r.amount_minor as unknown as number) = Math.round(
                parseFloat((ev.currentTarget as HTMLInputElement).value || "0") * 100
              ))}
          />
          <span class="unit small faint">/mo from +</span>
          <input class="input tabular narrow" bind:value={r.delay_months} />
          <span class="unit small faint">mo, ramp</span>
          <input class="input tabular narrow" bind:value={r.ramp_months} />
          <span class="unit small faint">for</span>
          <input class="input tabular narrow" bind:value={r.duration_months} placeholder="∞" />
        {:else}
          <select
            class="select"
            value={(r.target as never as { kind: string }).kind}
            onchange={(ev) => {
              const k = (ev.currentTarget as HTMLSelectElement).value;
              (r.target as unknown) =
                k === "account"
                  ? { kind: "account", account_id: accounts[0]?.id ?? 0 }
                  : { kind: "category", category_id: categories[0]?.id ?? 0 };
            }}
          >
            <option value="account">an account</option>
            <option value="category">a category</option>
          </select>
          {#if (r.target as never as { kind: string }).kind === "account"}
            <select
              class="select"
              bind:value={(r.target as never as { account_id: number }).account_id}
            >
              {#each accounts as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
            </select>
          {:else}
            <select
              class="select"
              bind:value={(r.target as never as { category_id: number }).category_id}
            >
              {#each categories as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
            </select>
          {/if}
          <input
            class="input tabular"
            value={(r.amount_minor as unknown as number) / 100}
            oninput={(ev) =>
              ((r.amount_minor as unknown as number) = Math.round(
                parseFloat((ev.currentTarget as HTMLInputElement).value || "0") * 100
              ))}
          />
        {/if}
        <button class="btn btn-sm btn-danger" onclick={() => effects.splice(i, 1)}>✕</button>
      </div>
    {/each}
  </div>
  <div class="row wrap" style="gap:6px;margin-top:6px">
    {#if streams.length > 0}
      <button class="btn btn-sm" onclick={() => addEffect("income_step")}>+ Pay change</button>
      <button class="btn btn-sm" onclick={() => addEffect("income_pause")}>+ Pause pay</button>
      <button class="btn btn-sm" onclick={() => addEffect("income_start")}>+ Income starts</button>
      <button class="btn btn-sm" onclick={() => addEffect("income_end")}>+ Income ends</button>
    {/if}
    <button class="btn btn-sm" onclick={() => addEffect("recurring_delta")}>
      + Ongoing cost
    </button>
    <button class="btn btn-sm" onclick={() => addEffect("one_off_amount")}>+ One-off</button>
    <button class="btn btn-sm" onclick={() => addEffect("set_baseline")}>+ Set a baseline</button>
  </div>

  <!-- ---- relations -------------------------------------------------------------- -->
  <div class="section-head small">Timing rules</div>
  <div class="rows">
    {#each relations as r, i (i)}
      <div class="eff-row">
        <span class="unit small faint">Happens</span>
        <select class="select" bind:value={r.kind}>
          <option value="after">after</option>
          <option value="only_if">only if</option>
        </select>
        {#if r.kind === "after"}
          <input class="input tabular narrow" bind:value={r.min_gap_months} />
          <span class="unit small faint">months after</span>
        {/if}
        <select class="select grow" bind:value={r.depends_on_event_id}>
          {#each eligible as o (o.id)}<option value={o.id}>{o.label}</option>{/each}
        </select>
        <button class="btn btn-sm btn-danger" onclick={() => relations.splice(i, 1)}>✕</button>
      </div>
    {/each}
  </div>
  {#if eligible.length > 0}
    <button
      class="btn btn-sm"
      style="margin-top:6px"
      onclick={() =>
        relations.push({
          depends_on_event_id: eligible[0].id,
          kind: "after",
          min_gap_months: 0,
        })}
    >
      + Timing rule
    </button>
  {:else if allEvents.length > 1}
    <!-- Saying *why* the list is empty is the difference between a considered design and a bug. -->
    <p class="small faint" style="margin:6px 0 0">
      Every other change already depends on this one, so it cannot be made to wait for any of them.
    </p>
  {:else}
    <p class="small faint" style="margin:6px 0 0">
      Add another change first, then this one can be made to wait for it.
    </p>
  {/if}

  <div class="row" style="justify-content:space-between;margin-top:14px">
    {#if initial && ondelete}
      <button class="btn btn-sm btn-danger" onclick={ondelete}>Remove</button>
    {:else}<span></span>{/if}
    <div class="row" style="gap:8px">
      <button class="btn" onclick={oncancel}>Cancel</button>
      <!-- Never disabled: the click is what reports what is missing. -->
      <button class="btn btn-primary" onclick={save}>
        {saving ? "Saving…" : initial ? "Save" : "Add"}
      </button>
    </div>
  </div>
</div>

<style>
  .editor {
    margin-top: 10px;
    padding: 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .grid-fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 10px;
    margin-bottom: 10px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .lbl {
    font-size: 11px;
    color: var(--text-faint);
  }
  .lbl.req::after {
    content: " *";
    color: var(--negative);
  }
  .input.invalid {
    border-color: var(--negative);
  }
  .section-head {
    margin: 12px 0 6px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .eff-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
  }
  .eff-kind {
    min-width: 96px;
    color: var(--text-faint);
    text-transform: capitalize;
  }
  .eff-row :global(.input),
  .eff-row :global(.select) {
    width: auto;
    min-width: 90px;
  }
  .eff-row :global(.input.narrow) {
    min-width: 52px;
    width: 52px;
  }
  .unit {
    white-space: nowrap;
  }
</style>
