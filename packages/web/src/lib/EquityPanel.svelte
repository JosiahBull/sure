<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "./api";
  import InvestmentTabs from "./InvestmentTabs.svelte";
  import ValuationPanel from "./ValuationPanel.svelte";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  type Grant = Schemas["EquityGrant"];
  type Status = Schemas["VestingStatus"];
  type Mark = Schemas["EquityMark"];
  type Event = Schemas["EquityEvent"];

  // The account's two halves, deliberately kept apart: how many units are held (grants, and what
  // they have vested) and what one unit is worth (the mark ledger). The account's value is the
  // product, so neither tab on its own is the answer.
  let equity = $state<Schemas["AccountEquity"] | null>(null);
  let terms = $state<Record<number, Grant>>({});
  let marks = $state<Mark[]>([]);
  let events = $state<Event[]>([]);
  let tab = $state<"holdings" | "activity" | "valuations">("holdings");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const ccy = $derived(equity?.currency_code ?? "NZD");

  async function load() {
    const [e, g, m, ev] = await Promise.all([
      api.GET("/api/accounts/{id}/equity", { params: { path: { id: accountId } } }),
      api.GET("/api/accounts/{id}/equity-grants", { params: { path: { id: accountId } } }),
      api.GET("/api/accounts/{id}/equity-marks", { params: { path: { id: accountId } } }),
      api.GET("/api/accounts/{id}/equity-events", { params: { path: { id: accountId } } }),
    ]);
    equity = e.data ?? null;
    terms = Object.fromEntries((g.data ?? []).map((row) => [row.id, row]));
    marks = m.data ?? [];
    events = ev.data ?? [];
  }
  onMount(load);

  function fail(e: unknown, fallback: string) {
    error = (e as { error?: { message?: string } })?.error?.message ?? fallback;
    notice = null;
  }

  const minorOf = (s: string) => Math.round(parseFloat(s.replace(/[^0-9.-]/g, "")) * 100);
  const majorOf = (minor: number) => String(minor / 100);

  // ---- grants: the quantity side ---------------------------------------------------
  const blankGrant = () => ({
    company: "",
    grant_date: new Date().toISOString().slice(0, 10),
    quantity: "",
    strike: "",
    vest_months: "48",
    cliff_months: "0",
    note: "",
  });
  let showGrantForm = $state(false);
  let editingGrant = $state<number | null>(null);
  let grantForm = $state(blankGrant());

  function startAddGrant() {
    editingGrant = null;
    grantForm = blankGrant();
    showGrantForm = true;
    error = null;
  }

  function startEditGrant(grantId: number) {
    const row = terms[grantId];
    if (!row) return;
    editingGrant = grantId;
    grantForm = {
      company: row.company,
      grant_date: row.grant_date.slice(0, 10),
      quantity: String(row.quantity),
      strike: majorOf(row.strike_minor),
      vest_months: String(row.vest_months),
      cliff_months: String(row.cliff_months),
      note: row.note ?? "",
    };
    showGrantForm = true;
    error = null;
  }

  async function saveGrant() {
    const quantity = parseInt(grantForm.quantity.replace(/[^0-9]/g, ""), 10);
    if (!grantForm.company.trim() || !quantity) return;
    const cliff = parseInt(grantForm.cliff_months, 10);
    const body = {
      company: grantForm.company.trim(),
      grant_date: grantForm.grant_date,
      quantity,
      strike_minor: grantForm.strike.trim() ? minorOf(grantForm.strike) : 0,
      // No unit value: price lives in the Valuations tab. Sending one here would write a mark
      // dated at the grant, which is rarely what someone editing vesting terms means.
      vest_months: parseInt(grantForm.vest_months, 10) || 48,
      // Not `|| 0`-guarded: zero is the common case (only the first grant in a series usually
      // carries a cliff), and falsy-coalescing would turn "no cliff" into twelve months.
      cliff_months: Number.isFinite(cliff) ? cliff : 0,
      note: grantForm.note.trim() || null,
    };
    busy = true;
    error = null;
    const { error: e } =
      editingGrant !== null
        ? await api.PUT("/api/equity-grants/{id}", {
            params: { path: { id: editingGrant } },
            body,
          })
        : await api.POST("/api/accounts/{id}/equity-grants", {
            params: { path: { id: accountId } },
            body,
          });
    busy = false;
    if (e) return fail(e, "Couldn't save that grant.");
    showGrantForm = false;
    editingGrant = null;
    await load();
    onchange?.();
  }

  // Deleting a grant takes its whole vesting history with it and there is no undo, so the
  // button asks twice — the same two-step the valuation rows use, for the same reason.
  let confirmDeleteGrant = $state<number | null>(null);

  async function deleteGrant(grantId: number) {
    busy = true;
    confirmDeleteGrant = null;
    const { error: e } = await api.DELETE("/api/equity-grants/{id}", {
      params: { path: { id: grantId } },
    });
    busy = false;
    if (e) return fail(e, "Couldn't delete that grant.");
    await load();
    onchange?.();
  }

  // ---- exercises: also the quantity side -------------------------------------------
  let exerciseFor = $state<number | null>(null);
  const blankExercise = () => ({
    exercise_date: new Date().toISOString().slice(0, 10),
    quantity: "",
    price: "",
    note: "",
  });
  let exerciseForm = $state(blankExercise());

  function startExercise(grantId: number) {
    exerciseFor = grantId;
    exerciseForm = blankExercise();
    error = null;
  }

  async function saveExercise() {
    if (exerciseFor === null) return;
    const quantity = parseInt(exerciseForm.quantity.replace(/[^0-9]/g, ""), 10);
    if (!quantity) return;
    busy = true;
    error = null;
    const { error: e } = await api.POST("/api/equity-grants/{id}/exercises", {
      params: { path: { id: exerciseFor } },
      body: {
        exercise_date: exerciseForm.exercise_date,
        quantity,
        price_minor: exerciseForm.price.trim() ? minorOf(exerciseForm.price) : 0,
        note: exerciseForm.note.trim() || null,
      },
    });
    busy = false;
    // The server refuses more than had vested by that date, so a wrong date surfaces here.
    if (e) return fail(e, "Couldn't record that exercise.");
    exerciseFor = null;
    await load();
    onchange?.();
  }

  // ---- marks: the price side -------------------------------------------------------
  const blankMark = () => ({
    as_of: new Date().toISOString().slice(0, 10),
    unit_value: "",
    note: "",
  });
  let markForm = $state(blankMark());
  let confirmDeleteMark = $state<number | null>(null);

  async function saveMark() {
    if (!markForm.unit_value.trim()) return;
    busy = true;
    error = null;
    const { error: e } = await api.POST("/api/accounts/{id}/equity-marks", {
      params: { path: { id: accountId } },
      body: {
        as_of: markForm.as_of,
        unit_value_minor: minorOf(markForm.unit_value),
        note: markForm.note.trim() || null,
      },
    });
    busy = false;
    if (e) return fail(e, "Couldn't save that price.");
    markForm = blankMark();
    await load();
    // Deliberately not rebuilt for you: a new mark changes every valuation from its date
    // forward, and silently rewriting net-worth history on a keystroke is not something to do
    // unasked. This is the ask.
    notice = "Price saved. Rebuild history to apply it to past valuations.";
  }

  async function deleteMark(id: number) {
    busy = true;
    confirmDeleteMark = null;
    const { error: e } = await api.DELETE("/api/equity-marks/{id}", { params: { path: { id } } });
    busy = false;
    if (e) return fail(e, "Couldn't delete that price.");
    await load();
    notice = "Price removed. Rebuild history to apply it.";
  }

  async function rebuild() {
    busy = true;
    error = null;
    const { data, error: e } = await api.POST("/api/accounts/{id}/equity/rebuild", {
      params: { path: { id: accountId } },
    });
    busy = false;
    if (e) return fail(e, "Couldn't rebuild the history.");
    notice = data
      ? `Rebuilt ${data.written} valuation${data.written === 1 ? "" : "s"}${
          data.from && data.to ? `, ${formatDate(data.from)} to ${formatDate(data.to)}` : ""
        }.`
      : null;
    await load();
    onchange?.();
  }

  async function revalue() {
    busy = true;
    error = null;
    const { error: e } = await api.POST("/api/accounts/{id}/equity/revalue", {
      params: { path: { id: accountId } },
    });
    busy = false;
    if (e) return fail(e, "Couldn't record today's value.");
    notice = "Today's value recorded.";
    await load();
    onchange?.();
  }

  const pct = (g: Status) => (g.quantity ? Math.round((g.vested / g.quantity) * 100) : 0);
  // Unrounded, so the three bar segments add up to exactly the track rather than to 100.03%.
  const share = (n: number, total: number) => (total ? (n / total) * 100 : 0);

  // Several grants from one employer is the normal case, so the company name distinguishes
  // none of them — the note does, which is why the server sends it as an event's `grant_label`.
  // Lead with it here for the same reason, and demote the company to the line underneath.
  const grantTitle = (g: Status) => terms[g.grant_id]?.note?.trim() || g.company;

  // Assembled here rather than interpolated in the markup: Svelte trims the whitespace either
  // side of a block boundary, so a `{#if}` inside the line ate the separator's leading space.
  function termsLine(g: Status): string {
    const t = terms[g.grant_id];
    const bits = [g.company];
    if (t) {
      bits.push(
        `vests from ${formatDate(t.grant_date)} over ${t.vest_months} months`,
        t.cliff_months ? `${t.cliff_months}-month cliff` : "no cliff",
        `strike ${formatMoney(t.strike_minor, g.currency_code)}`
      );
    }
    return bits.join(" · ");
  }

  // Exhaustive by type: adding an `EquityEventKind` variant is a compile error here. One map
  // rather than three parallel ones, so a new kind cannot pick up a label while silently
  // inheriting some other kind's weight or its effect on the running total.
  //
  // `adds` is the honest distinction the list has to draw: vesting and the cliff release units,
  // while an exercise moves the same units from exercisable to owned. Summing all three into a
  // month's total would double-count every exercise.
  const EVENT: Record<
    Schemas["EquityEventKind"],
    { label: string; tone: "quiet" | "mark" | "strong"; adds: boolean }
  > = {
    // Monthly vesting is most of any ledger here, so it is the one kind that gets no chip:
    // badging the ordinary case is what buries the two that matter.
    vest: { label: "vested", tone: "quiet", adds: true },
    cliff: { label: "cliff", tone: "mark", adds: true },
    exercise: { label: "exercised", tone: "strong", adds: false },
  };

  type Month = { key: string; label: string; released: number; rows: Event[] };

  // A four-year grant is around fifty near-identical rows, which is unreadable flat. The cadence
  // they actually have is monthly, so the month is the grouping — and its heading carries what
  // the month did, which no individual row can say.
  const months = $derived.by(() => {
    const by = new Map<string, Month>();
    for (const e of events) {
      const key = e.date.slice(0, 7);
      let m = by.get(key);
      if (!m) {
        m = { key, label: monthLabel(e.date), released: 0, rows: [] };
        by.set(key, m);
      }
      m.rows.push(e);
      if (EVENT[e.kind].adds) m.released += e.quantity;
    }
    return [...by.values()];
  });

  // Same local-midnight guard as `formatDate`: an ISO day string parsed as UTC lands in the
  // previous month for half the world.
  function monthLabel(iso: string): string {
    const d = new Date(`${iso.slice(0, 10)}T00:00:00`);
    if (isNaN(d.getTime())) return iso;
    return d.toLocaleDateString("en-NZ", { month: "long", year: "numeric" });
  }

</script>

<div class="equity">
  {#if equity}
    <header class="head">
      <div class="totals">
        <div class="tabular total">{formatMoney(equity.total_value_minor, ccy)}</div>
        <!-- The product, then its two factors, with the operator shown. An exercise moves value
             from the right figure to the left while changing neither the units held nor the
             price, so showing only one makes a conversion read as a gain or a loss. -->
        <div class="halves">
          <span class="half">
            <span class="tabular half-figure">{formatMoney(equity.total_owned_minor, ccy)}</span>
            <span class="half-label">shares held</span>
          </span>
          <span class="plus" aria-hidden="true">+</span>
          <span class="half">
            <span class="tabular half-figure">{formatMoney(equity.total_intrinsic_minor, ccy)}</span
            >
            <span class="half-label">exercisable options</span>
          </span>
        </div>
        {#if equity.unit_value_minor != null}
          <div class="small muted">
            at <span class="tabular">{formatMoney(equity.unit_value_minor, ccy)}</span> a unit{equity.unit_value_as_of
              ? `, set ${formatDate(equity.unit_value_as_of)}`
              : ""}
          </div>
        {:else}
          <!-- Framed rather than coloured red: `--warn` fails contrast as small text, so it
               carries the frame and the words stay at full ink. -->
          <div class="small warn-line">
            No price set — units are tracked exactly, but this values at zero until you add one
            under Valuations.
          </div>
        {/if}
      </div>

      <div class="head-acts">
        <span class="eyebrow">Writes net-worth history</span>
        <div class="row btn-row">
          <button
            class="btn btn-sm"
            onclick={revalue}
            disabled={busy}
            title="Write one valuation dated today, from the units held now at the current price."
            >Record today</button
          >
          <button
            class="btn btn-sm"
            onclick={rebuild}
            disabled={busy}
            title="Rewrite every valuation from the vesting schedules and the price ledger."
            >Rebuild history</button
          >
        </div>
        <span class="small faint">Today only, or every date your prices cover.</span>
      </div>
    </header>

    <InvestmentTabs
      tabs={[
        { id: "holdings", label: "Holdings", count: equity.grants.length },
        { id: "activity", label: "Activity", count: events.length },
        { id: "valuations", label: "Valuations", count: marks.length },
      ]}
      active={tab}
      onselect={(id) => (tab = id as typeof tab)}
    />

    {#if error}<div class="error-banner banner">{error}</div>{/if}
    {#if notice && !error}<div class="small notice">{notice}</div>{/if}

    {#if tab === "holdings"}
      {#each equity.grants as g (g.grant_id)}
        <article class="grant">
          <div class="grant-top">
            <strong class="grant-name">{grantTitle(g)}</strong>
            <span class="tabular grant-value"
              >{formatMoney(g.total_value_minor, g.currency_code)}</span
            >
          </div>

          <!-- One muted line for everything the grant *is*: who issued it and the schedule it
               vests on. Separating the issuer onto its own row spent a line on the fact four
               grants have in common. -->
          <div class="small muted terms">{termsLine(g)}</div>

          <div class="progress">
            <!-- The two filled segments are the first two figures below, in the same order and
                 the same shades, and the empty remainder is the third. Hidden from assistive
                 tech because those figures restate every number in it as text. -->
            <div class="bar" aria-hidden="true">
              <span class="seg owned" style="width:{share(g.owned, g.quantity)}%"></span>
              <span
                class="seg exercisable"
                style="width:{share(g.vested_unexercised, g.quantity)}%"
              ></span>
            </div>
            <span class="small muted vested">
              <span class="tabular">{g.vested.toLocaleString()}</span> of
              <span class="tabular">{g.quantity.toLocaleString()}</span> vested · {pct(g)}%
            </span>
          </div>

          <div class="grant-foot">
            <div class="figures">
              <span class="figure">
                <i class="swatch owned"></i>
                <span class="tabular">{g.owned.toLocaleString()}</span> owned
              </span>
              <span class="figure">
                <i class="swatch exercisable"></i>
                <span class="tabular">{g.vested_unexercised.toLocaleString()}</span> exercisable
              </span>
              <span class="figure">
                <i class="swatch unvested"></i>
                <span class="tabular">{g.unvested.toLocaleString()}</span> unvested
              </span>
            </div>
            <div class="row acts">
              <button class="btn btn-sm" onclick={() => startExercise(g.grant_id)}>Exercise</button>
              <button class="btn btn-sm" onclick={() => startEditGrant(g.grant_id)}>Edit</button>
              {#if confirmDeleteGrant === g.grant_id}
                <button class="btn btn-sm btn-danger" onclick={() => deleteGrant(g.grant_id)}
                  >Delete grant and its history?</button
                >
                <button class="btn btn-sm" onclick={() => (confirmDeleteGrant = null)}>Keep</button>
              {:else}
                <button
                  class="btn btn-sm"
                  aria-label="Delete {grantTitle(g)}"
                  onclick={() => (confirmDeleteGrant = g.grant_id)}>✕</button
                >
              {/if}
            </div>
          </div>

          {#if exerciseFor === g.grant_id}
            <div class="sub">
              <div class="sub-title">Record an exercise · {grantTitle(g)}</div>
              <p class="small muted sub-intro">
                Only what had vested by the date below can be exercised —
                {g.vested_unexercised.toLocaleString()} available today.
              </p>
              <div class="form-grid">
                <label class="field">
                  <span class="small muted">Date</span>
                  <input class="input" type="date" bind:value={exerciseForm.exercise_date} />
                </label>
                <label class="field">
                  <span class="small muted">Units</span>
                  <input class="input tabular" bind:value={exerciseForm.quantity} />
                </label>
                <label class="field">
                  <span class="small muted">Price paid</span>
                  <input
                    class="input tabular"
                    placeholder="0.01"
                    bind:value={exerciseForm.price}
                  />
                </label>
                <label class="field wide">
                  <span class="small muted">Note (optional)</span>
                  <input class="input" bind:value={exerciseForm.note} />
                </label>
              </div>
              <div class="row form-acts">
                <button class="btn btn-sm btn-primary" onclick={saveExercise} disabled={busy}
                  >Record exercise</button
                >
                <button class="btn btn-sm" onclick={() => (exerciseFor = null)}>Cancel</button>
              </div>
            </div>
          {/if}
        </article>
      {/each}

      {#if equity.grants.length === 0}
        <p class="empty-line">No grants yet. Add one and its vesting fills in the Activity tab.</p>
      {/if}

      {#if showGrantForm}
        <div class="sub">
          <div class="sub-title">{editingGrant !== null ? "Edit grant" : "Add a grant"}</div>
          <p class="small muted sub-intro">
            Vesting runs from the date below, not the date the deed was signed — the two often
            differ. Cliff 0 vests from the first month. The share price is set under Valuations.
          </p>
          <div class="form-grid">
            <label class="field">
              <span class="small muted">Company</span>
              <input class="input" bind:value={grantForm.company} />
            </label>
            <label class="field">
              <span class="small muted">Vesting starts</span>
              <input class="input" type="date" bind:value={grantForm.grant_date} />
            </label>
            <label class="field">
              <span class="small muted">Units granted</span>
              <input class="input tabular" bind:value={grantForm.quantity} />
            </label>
            <label class="field">
              <span class="small muted">Strike</span>
              <input class="input tabular" placeholder="0.00" bind:value={grantForm.strike} />
            </label>
            <label class="field">
              <span class="small muted">Vest months</span>
              <input class="input tabular" bind:value={grantForm.vest_months} />
            </label>
            <label class="field">
              <span class="small muted">Cliff months</span>
              <input class="input tabular" placeholder="0" bind:value={grantForm.cliff_months} />
            </label>
          </div>
          <label class="field note-field">
            <span class="small muted">Note — what tells this grant from the others</span>
            <input
              class="input"
              placeholder="e.g. offer #4, deed 5 Aug 2026, expires 2036"
              bind:value={grantForm.note}
            />
          </label>
          <div class="row form-acts">
            <button class="btn btn-sm btn-primary" onclick={saveGrant} disabled={busy}>
              {editingGrant !== null ? "Save changes" : "Add grant"}
            </button>
            <button class="btn btn-sm" onclick={() => (showGrantForm = false)}>Cancel</button>
          </div>
        </div>
      {:else}
        <button class="btn btn-sm add" onclick={startAddGrant}>+ Grant</button>
      {/if}
    {/if}

    {#if tab === "activity"}
      <p class="small muted lede">
        Every change in what this account holds. Vesting is computed from each grant's schedule —
        it isn't entered, it's what the deed already says happens.
      </p>
      {#if events.length}
        <div class="log-scroll">
          <table class="log">
            <thead>
              <tr>
                <th class="c-date">Date</th>
                <th class="c-kind">Event</th>
                <th class="c-units num">Units</th>
                <th class="c-grant">Grant</th>
                <th class="c-value num">Value then</th>
              </tr>
            </thead>
            <tbody>
              {#each months as m (m.key)}
                <tr class="month">
                  <th colspan="5" scope="colgroup">
                    <div class="band">
                      <span>{m.label}</span>
                      {#if m.released > 0}
                        <span class="released">
                          <span class="tabular">+{m.released.toLocaleString()}</span> units released
                        </span>
                      {/if}
                    </div>
                  </th>
                </tr>
                {#each m.rows as e, i (`${e.grant_id}:${e.date}:${e.kind}:${e.quantity}`)}
                  <tr>
                    <!-- The full date rather than the day-of-month, because a row read on its own
                         still has to say when it happened — but hidden, not omitted, where the
                         row above already carries it: four tranches vest on the same day and
                         restating the date four times is most of what made this a wall. It stays
                         in the DOM for assistive tech, and the narrow layout shows it again,
                         where rows are separate blocks and "same as above" no longer reads. -->
                    <td
                      class="c-date tabular muted"
                      class:repeat={i > 0 && m.rows[i - 1]!.date === e.date}>{formatDate(e.date)}</td
                    >
                    <td class="c-kind"
                      ><span class="kind kind-{EVENT[e.kind].tone}">{EVENT[e.kind].label}</span></td
                    >
                    <td class="c-units num tabular">{e.quantity.toLocaleString()}</td>
                    <td class="c-grant small muted"
                      >{e.grant_label ?? ""}{e.note ? ` · ${e.note}` : ""}</td
                    >
                    <td class="c-value num tabular small">
                      {#if e.unit_value_minor != null}
                        {formatMoney(e.quantity * e.unit_value_minor, ccy)}
                      {:else}
                        <span class="faint">no price then</span>
                      {/if}
                    </td>
                  </tr>
                {/each}
              {/each}
            </tbody>
          </table>
        </div>
      {:else}
        <p class="empty-line">Nothing yet — add a grant first.</p>
      {/if}
    {/if}

    {#if tab === "valuations"}
      <!-- Two ledgers, not one list with bold labels in it: what a unit is worth is an input,
           and what went into net-worth history is an output of it. -->
      <section class="vsec">
        <div class="vsec-head">
          <h3>Price per unit</h3>
          <span class="small faint">{marks.length === 1 ? "1 price" : `${marks.length} prices`}</span
          >
        </div>
        <p class="small muted vsec-lede">
          What one unit is worth, from each date until the next. An unlisted company has no price
          feed, so these come from funding rounds or internal revaluations — and the account's
          value on any date is this price times the units held then.
        </p>
        <div class="box">
          <div class="row wrap mark-form">
            <label class="field f-value">
              <span class="small muted">Value per unit</span>
              <input class="input tabular" placeholder="29.37" bind:value={markForm.unit_value} />
            </label>
            <label class="field">
              <span class="small muted">From</span>
              <input class="input" type="date" bind:value={markForm.as_of} />
            </label>
            <label class="field f-note">
              <span class="small muted">Note (optional)</span>
              <input
                class="input"
                placeholder="e.g. Series B, US$500m post"
                bind:value={markForm.note}
              />
            </label>
            <button
              class="btn btn-sm btn-primary"
              onclick={saveMark}
              disabled={busy || !markForm.unit_value.trim()}
            >
              Set price
            </button>
          </div>

          {#each marks as m (m.id)}
            <div class="mark">
              <div class="mark-head">
                <span class="tabular mark-value"
                  >{formatMoney(m.unit_value_minor, m.currency_code)}</span
                >
                <span class="small faint">a unit</span>
                <span class="small muted">from {formatDate(m.as_of)}</span>
              </div>
              <div class="mark-act">
                {#if confirmDeleteMark === m.id}
                  <button class="btn btn-sm btn-danger" onclick={() => deleteMark(m.id)}
                    >Delete?</button
                  >
                  <button class="btn btn-sm" onclick={() => (confirmDeleteMark = null)}>Keep</button>
                {:else}
                  <button
                    class="btn btn-sm"
                    aria-label="Delete the price from {m.as_of}"
                    onclick={() => (confirmDeleteMark = m.id)}>✕</button
                  >
                {/if}
              </div>
              <!-- At `--text-muted`, not `--text-faint`: a mark's note is where a caveat like
                   "ESTIMATE — confirm before relying on this" lives, and it fails contrast at
                   13px in the faint shade. -->
              {#if m.note}<p class="small mark-note">{m.note}</p>{/if}
            </div>
          {/each}
          {#if marks.length === 0}
            <p class="empty-line">
              No price set. Units are still tracked exactly; this values at zero until there's a
              price to multiply them by.
            </p>
          {/if}
        </div>
      </section>

      <section class="vsec">
        <div class="vsec-head"><h3>Recorded values</h3></div>
        <p class="small muted vsec-lede">
          What went into net-worth history. Rebuild history writes one of these per price above,
          and they start hidden — tick <em>Show synced &amp; scheduled</em> to see them. A value
          you set by hand overrides the computed one from its date until the next, which is the way
          to record something the grants and prices can't know: a tender offer, or a mark you'd
          rather not restate the whole history with.
        </p>
        <ValuationPanel
          {accountId}
          accountClass="investment"
          currency={ccy}
          onchange={() => {
            load();
            onchange?.();
          }}
        />
      </section>
    {/if}
  {/if}
</div>

<style>
  .equity {
    /* `--surface-2` is the same colour as `--bg-elev` in both palettes, so any fill built on it
       — the shared `.badge`, the inset form blocks — disappears inside a panel painted with
       `--bg-elev`. Derive the insets from the ink instead: it steps away from the panel in
       whichever direction the active theme needs. */
    --inset: color-mix(in srgb, var(--text) 5%, transparent);
    --inset-strong: color-mix(in srgb, var(--text) 9%, transparent);
    /* The vesting bar and the three figures under it share these, which is what makes the
       segments readable without a legend. */
    --seg-owned: var(--accent);
    --seg-exercisable: color-mix(in srgb, var(--accent) 42%, var(--bg-elev));
    --seg-unvested: color-mix(in srgb, var(--text) 13%, transparent);

    /* The panel is the query container: it renders both above a full-width ledger and inside a
       row on the accounts page, so its own width — not the viewport's — is what decides whether
       the activity table has room for five columns. */
    container-type: inline-size;
    background: var(--bg-elev);
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 14px;
    margin: 2px 0 12px;
  }

  /* ---- header ------------------------------------------------------------- */
  .head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: flex-start;
    gap: 12px 20px;
    margin-bottom: 12px;
  }
  .totals {
    min-width: 0;
  }
  .total {
    font-size: 1.5rem;
    font-weight: 680;
    letter-spacing: -0.02em;
    line-height: 1.2;
  }
  .halves {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    gap: 4px 12px;
    margin: 6px 0 4px;
  }
  .half {
    display: flex;
    flex-direction: column;
  }
  .half-figure {
    font-size: 14px;
    font-weight: 600;
  }
  .half-label {
    font-size: 12px;
    color: var(--text-muted);
  }
  .plus {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-faint);
  }
  .head-acts {
    display: flex;
    flex-direction: column;
    /* Left-aligned inside a block that sits at the right end of the header: the caption stays
       readable whether the block is beside the totals or wrapped under them. */
    align-items: flex-start;
    gap: 6px;
  }
  .eyebrow {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-faint);
  }
  .btn-row {
    gap: 8px;
    flex-wrap: wrap;
  }
  .warn-line {
    margin-top: 4px;
    padding: 6px 9px;
    border-radius: var(--r-sm);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--warn) 42%, transparent);
    color: var(--text);
    max-width: 46ch;
  }

  .banner {
    margin-bottom: 10px;
  }
  .notice {
    margin-bottom: 10px;
    padding: 7px 10px;
    border-radius: var(--r-sm);
    background: var(--inset);
    border: 1px solid var(--border);
    color: var(--text);
  }
  .lede {
    margin: 0 0 10px;
    max-width: 78ch;
  }
  .empty-line {
    margin: 0;
    padding: 8px 0;
    font-size: 13px;
    color: var(--text-muted);
    max-width: 70ch;
  }

  /* ---- holdings ----------------------------------------------------------- */
  .grant {
    padding: 12px 0;
    border-top: 1px solid var(--border);
  }
  .grant-top {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 12px;
  }
  /* A basis rather than `1fr`: on a phone the longer grant names run to four lines, and a value
     pinned beside line one of four reads as belonging to that line. Past this width the value
     drops to its own line instead. */
  .grant-name {
    flex: 1 1 220px;
    font-size: 15px;
    font-weight: 620;
  }
  .grant-value {
    margin-left: auto;
    font-size: 15px;
    font-weight: 600;
    white-space: nowrap;
  }
  .terms {
    margin-top: 2px;
  }
  .progress {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 12px;
    margin: 10px 0 8px;
  }
  .bar {
    display: flex;
    /* Capped rather than greedy: at full width the "n of m vested" caption ended up half a
       panel away from the bar it describes. */
    flex: 1 1 200px;
    max-width: 380px;
    height: 6px;
    border-radius: 999px;
    background: var(--seg-unvested);
    overflow: hidden;
  }
  .seg {
    height: 100%;
  }
  .seg.owned {
    background: var(--seg-owned);
  }
  .seg.exercisable {
    background: var(--seg-exercisable);
  }
  .vested {
    white-space: nowrap;
  }
  .grant-foot {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 8px 16px;
  }
  .figures {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 16px;
    font-size: 13px;
    color: var(--text-muted);
  }
  .figure {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .figure .tabular {
    color: var(--text);
    font-weight: 550;
  }
  .swatch {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    flex: none;
  }
  .swatch.owned {
    background: var(--seg-owned);
  }
  .swatch.exercisable {
    background: var(--seg-exercisable);
  }
  .swatch.unvested {
    background: var(--seg-unvested);
  }
  .acts {
    gap: 6px;
    flex-wrap: wrap;
  }
  .add {
    margin-top: 12px;
  }

  /* ---- inset forms -------------------------------------------------------- */
  .sub {
    margin: 12px 0 2px;
    padding: 12px;
    background: var(--inset);
    border: 1px solid var(--border);
    border-radius: var(--r);
  }
  .sub-title {
    font-size: 14px;
    font-weight: 620;
  }
  .sub-intro {
    margin: 4px 0 10px;
    max-width: 74ch;
  }
  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 10px;
  }
  /* The note is the field with the most to say, so it takes the rest of its row rather than
     sharing a 140px cell with the numbers. */
  .form-grid .wide {
    grid-column: span 2;
  }
  .note-field {
    margin-top: 10px;
  }
  .form-acts {
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 12px;
  }

  /* ---- activity ----------------------------------------------------------- */
  .log-scroll {
    overflow-x: auto;
  }
  .log {
    width: 100%;
    border-collapse: collapse;
    font-size: 14px;
  }
  .log thead th {
    text-align: left;
    font-weight: 550;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
    padding: 4px 8px;
    border-bottom: 1px solid var(--border);
  }
  .log td {
    padding: 5px 8px;
    border-bottom: 1px solid var(--border);
    vertical-align: baseline;
  }
  .log tbody tr:hover td {
    background: var(--hover);
  }
  .log tr.month th {
    text-align: left;
    padding: 7px 8px;
    background: var(--inset);
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    font-size: 13px;
    font-weight: 620;
  }
  /* Flexed inside the cell, not on it: `display: flex` on a `<th>` takes it out of the table
     layout and its colspan stops applying, collapsing the band to one column wide. The band is
     a heading rather than a row of data, so nothing in it wants the columns anyway. */
  .band {
    display: flex;
    justify-content: space-between;
    gap: 12px;
  }
  .log tr.month .released {
    font-weight: 450;
    color: var(--text-muted);
  }
  .log .num {
    text-align: right;
  }
  /* Every column but the grant label is short and fixed: `width: 1%` collapses each to its
     content so the one cell with prose to show takes whatever is left. */
  .log .c-date,
  .log .c-kind,
  .log .c-units,
  .log .c-value {
    width: 1%;
    white-space: nowrap;
  }
  /* Hidden rather than dropped, so the column keeps its width and the text keeps existing. */
  .log .c-date.repeat {
    visibility: hidden;
  }
  .kind {
    display: inline-block;
    font-size: 12px;
    border-radius: 999px;
    padding: 1px 8px;
  }
  /* No chip on the ordinary case — see EVENT above. */
  .kind-quiet {
    padding-left: 0;
    padding-right: 0;
    color: var(--text-faint);
  }
  .kind-mark {
    border: 1px solid var(--border-strong);
    color: var(--text-muted);
  }
  .kind-strong {
    background: var(--inset-strong);
    color: var(--text);
    font-weight: 550;
  }

  /* Five columns need about 520px. Below that the ledger becomes two lines per event — when and
     what on the first, which grant and what it was worth on the second — rather than a table
     scrolled sideways with a 60px grant column breaking one word per line.
     Keyed on the *panel*, not the viewport: this component also renders inside an account row on
     the accounts page, where the viewport says nothing about how much room it has. */
  @container (max-width: 520px) {
    .log,
    .log tbody,
    .log tr.month,
    .log tr.month th,
    .log td {
      display: block;
    }
    .log thead {
      display: none;
    }
    .log tbody tr {
      display: grid;
      grid-template-columns: auto 1fr auto;
      column-gap: 8px;
      row-gap: 1px;
      padding: 7px 0;
      border-bottom: 1px solid var(--border);
    }
    .log td {
      padding: 0;
      border: 0;
    }
    .log .c-date,
    .log .c-kind,
    .log .c-units,
    .log .c-value {
      width: auto;
    }
    .log .c-date,
    .log .c-date.repeat {
      grid-area: 1 / 1;
      font-size: 13px;
      visibility: visible;
    }
    .log .c-kind {
      grid-area: 1 / 2;
    }
    .log .c-units {
      grid-area: 1 / 3;
    }
    .log .c-grant {
      grid-area: 2 / 1 / 2 / 3;
      white-space: normal;
    }
    .log .c-value {
      grid-area: 2 / 3;
    }
    .log tr.month th {
      /* Its colspan is gone with `display: block`, so the band spans the row by being one. */
      border-radius: var(--r-sm);
    }
  }

  /* ---- valuations --------------------------------------------------------- */
  .vsec + .vsec {
    margin-top: 22px;
  }
  .vsec-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }
  .vsec-head h3 {
    font-size: 14px;
    font-weight: 620;
  }
  .vsec-lede {
    margin: 4px 0 8px;
    max-width: 80ch;
  }
  /* Border-only, and the same radius and padding the nested ValuationPanel draws for itself, so
     the two ledgers read as siblings rather than one box floating inside another. */
  .box {
    border: 1px solid var(--border);
    border-radius: var(--r);
    padding: 12px;
  }
  .mark-form {
    gap: 10px;
    align-items: flex-end;
  }
  /* Floors, not just weights: a flex row shrinks its items before it wraps, so without these
     the note collapsed to "e.g. Se" on a phone instead of dropping to its own line. */
  .mark-form .f-value {
    flex: 1 1 120px;
    min-width: 110px;
  }
  .mark-form .f-note {
    flex: 4 1 180px;
    min-width: 160px;
  }
  .mark {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: start;
    gap: 2px 12px;
    padding: 9px 0;
    border-top: 1px solid var(--border);
  }
  .mark:first-of-type {
    margin-top: 10px;
  }
  .mark-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 8px;
  }
  .mark-value {
    font-size: 15px;
    font-weight: 600;
  }
  .mark-act {
    display: flex;
    gap: 6px;
    grid-row: 1;
    grid-column: 2;
  }
  .mark-note {
    grid-column: 1;
    margin: 0;
    color: var(--text-muted);
    max-width: 88ch;
  }
</style>
