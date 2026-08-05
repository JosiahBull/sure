<script lang="ts">
  // Things that might happen, and what they'd do.
  //
  // Each row reads as a sentence rather than a field dump, because the interesting content is the
  // relationship between four numbers — how likely, roughly when, how wide, and what it changes —
  // and a table of columns makes you reassemble that yourself every time.
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../../lib/api";
  import { people, ensureLoaded, personColor, initials } from "../../lib/people.svelte";
  import LifeEventEditor from "./LifeEventEditor.svelte";
  import { effectSummary, kindIcon, kindLabel } from "../../lib/forecast/lifeEvents";

  type ForecastEvent = Schemas["ForecastEvent"];

  let {
    result,
    currency,
    onchanged,
    focusEventId = null,
  }: {
    result: Schemas["ForecastResult"] | null;
    currency: string;
    onchanged: () => void;
    /** Set when a chart marker was clicked, so the matching row opens and flashes. */
    focusEventId?: number | null;
  } = $props();

  let events = $state<ForecastEvent[]>([]);
  let streams = $state<Schemas["IncomeStream"][]>([]);
  let categories = $state<Schemas["Category"][]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let editing = $state<number | null>(null);
  let adding = $state(false);
  let confirmDelete = $state<number | null>(null);
  let delError = $state<string | null>(null);

  async function load() {
    loading = true;
    const [ev, st, ca, ac] = await Promise.all([
      api.GET("/api/forecast/events", {}),
      api.GET("/api/income-streams", {}),
      api.GET("/api/categories", {}),
      api.GET("/api/accounts", {}),
    ]);
    events = ev.data ?? [];
    streams = st.data ?? [];
    categories = ca.data ?? [];
    accounts = ac.data ?? [];
    error = ev.error ? "Failed to load life events." : null;
    loading = false;
  }
  onMount(async () => {
    await ensureLoaded();
    await load();
  });

  // A chart marker was clicked: open that row.
  $effect(() => {
    if (focusEventId !== null) editing = focusEventId;
  });

  async function saved() {
    editing = null;
    adding = false;
    await load();
    onchanged();
  }

  async function remove(id: number) {
    const { error: e, response } = await api.DELETE("/api/forecast/events/{id}", {
      params: { path: { id } },
    });
    if (e) {
      // The 409 names the events that only happen *if* this one does — they would silently become
      // certain. Rendered verbatim: the error envelope carries a message and no structured payload,
      // so there is nothing to parse.
      delError =
        response.status === 409
          ? ((e as { error?: { message?: string } }).error?.message ?? "Still depended on.")
          : "Failed to remove this event.";
      return;
    }
    confirmDelete = null;
    delError = null;
    await saved();
  }

  const names = $derived({
    stream: (id: number) => streams.find((s) => s.id === id)?.label ?? `#${id}`,
    person: (id: number) => people.list.find((p) => p.id === id)?.name ?? `#${id}`,
    category: (id: number) => categories.find((c) => c.id === id)?.name ?? `#${id}`,
    account: (id: number) => accounts.find((a) => a.id === id)?.name ?? `#${id}`,
    money: (minor: number) => formatMoney(minor, currency),
  });

  const outcomes = $derived(new Map((result?.events ?? []).map((o) => [o.event_id, o])));
  const eventName = $derived(new Map(events.map((e) => [e.id, e.label])));

  /** Sorted by when they are actually expected to land, which is how you read a plan. */
  const sorted = $derived(
    events.slice().sort((a, b) => a.expected_on.localeCompare(b.expected_on))
  );

  function spreadLabel(months: number): string {
    if (months === 0) return "on the date";
    if (months % 12 === 0) return `± ${months / 12} ${months === 12 ? "year" : "years"}`;
    return `± ${months} months`;
  }
</script>

{#if error}<div class="error-banner" style="margin-bottom:16px">{error}</div>{/if}

<div class="row spread" style="margin-bottom:12px">
  <span class="muted small">
    might happen, might not — each carries its own odds and timing, and the chart above shows the
    window rather than a single date
  </span>
  <button class="btn btn-sm btn-primary" onclick={() => ((adding = true), (editing = null))}>
    + Add
  </button>
</div>

{#if loading && events.length === 0}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  {#if adding}
    <section class="card" style="margin-bottom:16px">
      <LifeEventEditor
        event={null}
        {streams}
        {categories}
        {accounts}
        allEvents={events}
        onsaved={saved}
        oncancel={() => (adding = false)}
      />
    </section>
  {/if}

  {#if sorted.length === 0 && !adding}
    <div class="empty">
      Nothing planned yet. A life event is something you think will probably happen — a promotion, a
      child, a career break — with rough timing rather than a date.
      <div class="small faint" style="margin-top:6px">
        Certain about both the date and the amount? Add it as a <em>certain change</em> — same form,
        100% likely, no spread.
      </div>
    </div>
  {/if}

  <div class="event-list">
    {#each sorted as e (e.id)}
      {@const o = outcomes.get(e.id)}
      {@const who = e.person_id != null ? people.list.find((p) => p.id === e.person_id) : null}
      <section
        class="card ev"
        class:flash={focusEventId === e.id}
        style="--who:{who ? personColor(who) : 'var(--text-faint)'}"
      >
        <div class="row spread wrap" style="gap:10px">
          <div class="row" style="gap:9px;min-width:0">
            <span class="ev-icon" aria-hidden="true">{kindIcon(e.kind)}</span>
            {#if who}
              <span class="avatar" style="background:{personColor(who)}">{initials(who.name)}</span>
            {/if}
            <span class="sentence">
              <strong>{e.label}</strong>
              <span class="faint">·</span>
              <!-- Opacity tracks probability here and on the chart, so the list and the picture
                   encode certainty the same way. -->
              <span class="pct" style="--o:{0.5 + 0.5 * (e.probability_bps / 10_000)}">
                {(e.probability_bps / 100).toFixed(0)}% likely
              </span>
              <span class="faint">·</span>
              <span>around {formatDate(e.expected_on)}</span>
              <span class="faint small">{spreadLabel(e.timing_spread_months)}</span>
              {#each e.relations as r (r.id)}
                <span class="faint small">
                  ·
                  {r.kind === "after"
                    ? `after ${eventName.get(r.depends_on_event_id) ?? "another change"}${
                        r.min_gap_months ? ` +${r.min_gap_months}mo` : ""
                      }`
                    : `only if ${eventName.get(r.depends_on_event_id) ?? "another change"} happens`}
                </span>
              {/each}
            </span>
          </div>
          <div class="row" style="gap:6px">
            <span class="badge">{kindLabel(e.kind)}</span>
            <button
              class="btn btn-sm"
              onclick={() => ((editing = editing === e.id ? null : e.id), (adding = false))}
            >
              {editing === e.id ? "Cancel" : "Edit"}
            </button>
          </div>
        </div>

        {#if e.effects.length > 0}
          <ul class="effects small">
            {#each e.effects as ef (ef.id)}
              <li>{effectSummary(ef as unknown as Schemas["LifeEffectSpec"], names)}</li>
            {/each}
          </ul>
        {:else}
          <div class="small faint" style="margin-top:4px">
            No effects yet — this changes nothing until you add one.
          </div>
        {/if}

        {#if o}
          <!-- What the simulation actually did with it, which is not always what was typed: a
               relation can push the date, and an `only_if` can stop it happening at all. -->
          <div class="realised small faint">
            happened in {(o.occurrence_rate_bps / 100).toFixed(0)}% of runs
            {#if o.in_window_rate_bps < o.occurrence_rate_bps}
              · only {(o.in_window_rate_bps / 100).toFixed(0)}% inside this horizon
            {/if}
            {#if o.date_median}· median {formatDate(o.date_median)}{/if}
            {#if o.constrained_rate_bps > 0}
              · a timing rule moved it in {(o.constrained_rate_bps / 100).toFixed(0)}% of runs
            {/if}
            {#if o.clamped_early_rate_bps > 0}
              · <span class="warn-text">its expected date is in the past</span>
            {/if}
          </div>
        {/if}

        {#if confirmDelete === e.id}
          <div class="confirm row" style="gap:8px">
            <span class="small">Remove this event?</span>
            <button class="btn btn-sm btn-danger" onclick={() => remove(e.id)}>Remove</button>
            <button class="btn btn-sm" onclick={() => ((confirmDelete = null), (delError = null))}>
              Keep
            </button>
          </div>
          {#if delError}<div class="error-banner small">{delError}</div>{/if}
        {/if}

        {#if editing === e.id}
          <LifeEventEditor
            event={e}
            {streams}
            {categories}
            {accounts}
            allEvents={events}
            onsaved={saved}
            oncancel={() => (editing = null)}
            ondelete={() => (confirmDelete = e.id)}
          />
        {/if}
      </section>
    {/each}
  </div>
{/if}

<style>
  .event-list {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .ev {
    border-left: 3px solid var(--who);
  }
  /* A chart marker was clicked: say which row it was, briefly. */
  .ev.flash {
    outline: 2px solid var(--who);
    outline-offset: 2px;
  }
  .ev-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--who) 18%, transparent);
    color: var(--who);
    font-size: 12px;
    flex: none;
  }
  .avatar {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    color: #fff;
    font-size: 10px;
    font-weight: 700;
    flex: none;
  }
  .sentence {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 5px;
    min-width: 0;
    font-size: 13px;
  }
  .pct {
    opacity: var(--o);
    font-weight: 560;
  }
  .effects {
    margin: 6px 0 0;
    padding-left: 18px;
    color: var(--text-muted);
  }
  .realised {
    margin-top: 6px;
    padding-top: 6px;
    border-top: 1px dashed var(--border);
  }
  .warn-text {
    color: var(--warn);
  }
  .confirm {
    margin-top: 8px;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--negative) 8%, var(--surface-2));
  }
</style>
