<script lang="ts">
  import { onMount, tick } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "./api";
  import Icon from "./Icon.svelte";
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
  let openGrantMenu = $state<number | null>(null);
  let scheduleFor = $state<number | null>(null);

  async function toggleGrantMenu(grantId: number) {
    openGrantMenu = openGrantMenu === grantId ? null : grantId;
    if (openGrantMenu !== null) {
      await tick();
      document.getElementById(`grant-menu-${accountId}-${grantId}`)
        ?.querySelector<HTMLButtonElement>("button")?.focus();
    }
  }

  function closeGrantMenu(event: MouseEvent | FocusEvent) {
    if (!(event.target instanceof Element) || !event.target.closest(".grant-menu")) {
      openGrantMenu = null;
    }
  }

  function menuKeys(event: KeyboardEvent) {
    const menu = event.currentTarget as HTMLElement;
    const buttons = [...menu.querySelectorAll<HTMLButtonElement>("button")];
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    let next: number;
    if (event.key === "ArrowDown") next = (index + 1) % buttons.length;
    else if (event.key === "ArrowUp") next = (index + buttons.length - 1) % buttons.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = buttons.length - 1;
    else return;
    event.preventDefault();
    buttons[next]?.focus();
  }

  function dismissGrantMenu(event: KeyboardEvent) {
    if (event.key === "Escape" && openGrantMenu !== null) {
      document.getElementById(`grant-actions-${accountId}-${openGrantMenu}`)?.focus();
      openGrantMenu = null;
    }
  }

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
  // Native number inputs accept exponent notation too; convert their actual numeric value.
  const nativeMinorOf = (s: string) => Math.round(Number(s) * 100);
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
  let grantNoteOpen = $state(false);

  function startAddGrant() {
    exerciseFor = null;
    confirmDeleteGrant = null;
    editingGrant = null;
    grantForm = blankGrant();
    grantNoteOpen = false;
    showGrantForm = true;
    error = null;
  }

  function startEditGrant(grantId: number) {
    const row = terms[grantId];
    if (!row) return;
    exerciseFor = null;
    confirmDeleteGrant = null;
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
    grantNoteOpen = !!row.note;
    showGrantForm = true;
    error = null;
  }

  async function saveGrant() {
    const quantity = Number(grantForm.quantity);
    if (!grantForm.company.trim() || !quantity) return;
    const cliff = Number(grantForm.cliff_months);
    const body = {
      company: grantForm.company.trim(),
      grant_date: grantForm.grant_date,
      quantity,
      strike_minor: grantForm.strike.trim() ? nativeMinorOf(grantForm.strike) : 0,
      // No unit value: price lives in the Valuations tab. Sending one here would write a mark
      // dated at the grant, which is rarely what someone editing vesting terms means.
      vest_months: Number(grantForm.vest_months),
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
  let exerciseNoteOpen = $state(false);

  function startExercise(grantId: number) {
    showGrantForm = false;
    editingGrant = null;
    confirmDeleteGrant = null;
    exerciseFor = grantId;
    exerciseForm = blankExercise();
    exerciseNoteOpen = false;
    error = null;
  }

  async function saveExercise() {
    if (exerciseFor === null) return;
    const quantity = Number(exerciseForm.quantity);
    if (!quantity) return;
    busy = true;
    error = null;
    const { error: e } = await api.POST("/api/equity-grants/{id}/exercises", {
      params: { path: { id: exerciseFor } },
      body: {
        exercise_date: exerciseForm.exercise_date,
        quantity,
        price_minor: exerciseForm.price.trim() ? nativeMinorOf(exerciseForm.price) : 0,
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
    // The server nudges the equity-rebuild task on this write, so every valuation from this
    // date forward is restated within a second or two. Said in the future tense rather than the
    // past because the rebuild is asynchronous: claiming it is already done would be a lie on a
    // slow machine, and asking the person to press a button — which is what this notice used to
    // do — was the bug.
    notice = "Price saved. Past valuations are being restated to match.";
  }

  async function deleteMark(id: number) {
    busy = true;
    confirmDeleteMark = null;
    const { error: e } = await api.DELETE("/api/equity-marks/{id}", { params: { path: { id } } });
    busy = false;
    if (e) return fail(e, "Couldn't delete that price.");
    await load();
    notice = "Price removed. Past valuations are being restated to match.";
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

  // Exhaustive by type: a new event kind must get a readable record label.
  const EVENT: Record<Schemas["EquityEventKind"], string> = {
    vest: "Vested",
    cliff: "Cliff vested",
    exercise: "Exercised",
  };

</script>

<svelte:window onclick={closeGrantMenu} onkeydown={dismissGrantMenu} />
<svelte:document onfocusin={closeGrantMenu} />

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
      </div>
    </header>

    <InvestmentTabs
      tabs={[
        { id: "holdings", label: "Holdings", count: equity.grants.length },
        { id: "activity", label: "Activity", count: events.length },
        { id: "valuations", label: "Valuations", count: marks.length },
      ]}
      active={tab}
      onselect={(id) => {
        tab = id as typeof tab;
        openGrantMenu = null;
      }}
    />

    {#if error}<div class="error-banner banner">{error}</div>{/if}
    {#if notice && !error}<div class="small notice">{notice}</div>{/if}

    {#if tab === "holdings"}
      {#each equity.grants as g (g.grant_id)}
        <article class="grant">
          <div class="grant-top">
            <div class="grant-heading">
              <strong class="grant-name">{grantTitle(g)}</strong>
              <span class="tabular grant-value">{formatMoney(g.total_value_minor, g.currency_code)}</span>
            </div>
            <div class="grant-menu">
              <button
                id="grant-actions-{accountId}-{g.grant_id}"
                class="btn btn-sm menu-trigger"
                aria-label="Actions for {grantTitle(g)}"
                aria-haspopup="menu"
                aria-expanded={openGrantMenu === g.grant_id}
                aria-controls="grant-menu-{accountId}-{g.grant_id}"
                onclick={() => toggleGrantMenu(g.grant_id)}
              ><Icon name="more-vertical" size={20} /></button>
              {#if openGrantMenu === g.grant_id}
                <div
                  id="grant-menu-{accountId}-{g.grant_id}"
                  class="grant-actions"
                  role="menu"
                  tabindex="-1"
                  aria-label="Grant actions"
                  onkeydown={menuKeys}
                >
                  <button role="menuitem" onclick={() => {
                    openGrantMenu = null;
                    startExercise(g.grant_id);
                  }}>Exercise</button>
                  <button role="menuitem" onclick={() => {
                    openGrantMenu = null;
                    startEditGrant(g.grant_id);
                  }}>Edit</button>
                  <button
                    role="menuitem"
                    aria-expanded={scheduleFor === g.grant_id}
                    aria-controls="grant-schedule-{accountId}-{g.grant_id}"
                    onclick={() => {
                      scheduleFor = scheduleFor === g.grant_id ? null : g.grant_id;
                      document.getElementById(`grant-actions-${accountId}-${g.grant_id}`)?.focus();
                      openGrantMenu = null;
                    }}
                  >Vesting schedule</button>
                  <button role="menuitem" class="delete-action" onclick={() => {
                    openGrantMenu = null;
                    showGrantForm = false;
                    exerciseFor = null;
                    confirmDeleteGrant = g.grant_id;
                  }}>Delete</button>
                </div>
              {/if}
            </div>
            {#if scheduleFor === g.grant_id}
              <p id="grant-schedule-{accountId}-{g.grant_id}" class="grant-terms small muted">
                {termsLine(g)}
              </p>
            {/if}
          </div>

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
                <span class="tabular">{g.owned.toLocaleString()}</span>
                <span class="figure-label">owned</span>
              </span>
              <span class="figure">
                <i class="swatch exercisable"></i>
                <span class="tabular">{g.vested_unexercised.toLocaleString()}</span>
                <span class="figure-label">exercisable</span>
              </span>
              <span class="figure">
                <i class="swatch unvested"></i>
                <span class="tabular">{g.unvested.toLocaleString()}</span>
                <span class="figure-label">unvested</span>
              </span>
            </div>
          </div>
          {#if confirmDeleteGrant === g.grant_id}
            <div class="sub">
              <p class="small sub-intro">Delete {grantTitle(g)} and its vesting history? This can't be undone.</p>
              <div class="row form-acts">
                <button class="btn btn-sm btn-danger" onclick={() => deleteGrant(g.grant_id)} disabled={busy}>Delete grant</button>
                <button class="btn btn-sm" onclick={() => (confirmDeleteGrant = null)}>Keep grant</button>
              </div>
            </div>
          {/if}

          {#if exerciseFor === g.grant_id}
            <form class="sub equity-form" onsubmit={(event) => { event.preventDefault(); saveExercise(); }}>
              <div class="sub-title">Record an exercise</div>
              <p class="small muted sub-intro">
                {g.vested_unexercised.toLocaleString()} units available today. Availability depends on the exercise date.
              </p>
              <div class="form-grid">
                <label class="field wide">
                  <span class="small muted">Date</span>
                  <input class="input" type="date" required bind:value={exerciseForm.exercise_date} />
                </label>
                <label class="field">
                  <span class="small muted">Units</span>
                  <input
                    class="input tabular"
                    type="number"
                    inputmode="numeric"
                    min="1"
                    step="1"
                    required
                    value={exerciseForm.quantity}
                    oninput={(event) => (exerciseForm.quantity = event.currentTarget.value)}
                  />
                </label>
                <label class="field">
                  <span class="small muted">Price paid per unit <span aria-hidden="true">({g.currency_code})</span></span>
                  <input
                    class="input tabular"
                    aria-label="Price paid per unit"
                    type="number"
                    inputmode="decimal"
                    min="0"
                    step="0.01"
                    placeholder="0.01"
                    value={exerciseForm.price}
                    oninput={(event) => (exerciseForm.price = event.currentTarget.value)}
                  />
                </label>
              </div>
              <details class="form-optional" bind:open={exerciseNoteOpen}>
                <summary>Note (optional)</summary>
                <label class="field">
                  <span class="sr-only">Note (optional)</span>
                  <input class="input" bind:value={exerciseForm.note} />
                </label>
              </details>
              <div class="row form-acts">
                <button class="btn btn-sm btn-primary" type="submit" disabled={busy}
                  >Record exercise</button
                >
                <button class="btn btn-sm" type="button" onclick={() => (exerciseFor = null)}>Cancel</button>
              </div>
            </form>
          {/if}
        </article>
      {/each}

      {#if equity.grants.length === 0}
        <p class="empty-line">No grants yet. Add one and its vesting fills in the Activity tab.</p>
      {/if}

      {#if showGrantForm}
        <form class="sub equity-form" onsubmit={(event) => { event.preventDefault(); saveGrant(); }}>
          <div class="sub-title">{editingGrant !== null ? "Edit grant" : "Add a grant"}</div>
          <p class="small muted sub-intro">
            Use the vesting terms from your grant deed.
          </p>
          <div class="form-grid">
            <label class="field wide">
              <span class="small muted">Company</span>
              <input class="input" autocomplete="organization" required bind:value={grantForm.company} />
            </label>
            <label class="field wide">
              <span class="small muted">Vesting starts</span>
              <input class="input" type="date" required bind:value={grantForm.grant_date} />
            </label>
            <label class="field">
              <span class="small muted">Units granted</span>
              <input class="input tabular" type="number" inputmode="numeric" min="1" step="1" required value={grantForm.quantity} oninput={(event) => (grantForm.quantity = event.currentTarget.value)} />
            </label>
            <label class="field">
              <span class="small muted">Strike price <span aria-hidden="true">({ccy})</span></span>
              <input
                class="input tabular"
                aria-label="Strike price"
                type="number"
                inputmode="decimal"
                min="0"
                step="0.01"
                placeholder="0.00"
                value={grantForm.strike}
                oninput={(event) => (grantForm.strike = event.currentTarget.value)}
              />
            </label>
            <label class="field">
              <span class="small muted">Vesting months</span>
              <input class="input tabular" aria-label="Vesting (months)" type="number" inputmode="numeric" min="1" step="1" required value={grantForm.vest_months} oninput={(event) => (grantForm.vest_months = event.currentTarget.value)} />
            </label>
            <label class="field">
              <span class="small muted">Cliff months</span>
              <input
                class="input tabular"
                aria-label="Cliff (months)"
                type="number"
                inputmode="numeric"
                min="0"
                step="1"
                placeholder="0"
                value={grantForm.cliff_months}
                oninput={(event) => (grantForm.cliff_months = event.currentTarget.value)}
              />
            </label>
          </div>
          <details class="form-optional" bind:open={grantNoteOpen}>
            <summary>Grant label (optional)</summary>
            <label class="field">
              <span class="sr-only">Grant label (optional)</span>
              <input class="input" placeholder="e.g. Initial grant" bind:value={grantForm.note} />
            </label>
          </details>
          <div class="row form-acts">
            <button class="btn btn-sm btn-primary" type="submit" disabled={busy}>
              {editingGrant !== null ? "Save changes" : "Add grant"}
            </button>
            <button class="btn btn-sm" type="button" onclick={() => (showGrantForm = false)}>Cancel</button>
          </div>
        </form>
      {:else}
        <div class="add-grant">
          <button class="btn btn-primary add" onclick={startAddGrant}>
            <Icon name="plus" size={18} /> Add grant
          </button>
        </div>
      {/if}
    {/if}

    {#if tab === "activity"}
      {#if events.length}
        <ul class="activity-list" aria-label="Equity activity">
          {#each events as e (`${e.grant_id}:${e.date}:${e.kind}:${e.quantity}`)}
            <li class="activity-record">
              <div class="record-main">
                <span class="record-kind">{EVENT[e.kind]}</span>
                <div class="record-meta">
                  <time datetime={e.date}>{formatDate(e.date)}</time>
                  {#if equity.grants.length > 1 && e.grant_label}
                    <span class="record-grant">{e.grant_label}</span>
                  {/if}
                  {#if e.note}<span>{e.note}</span>{/if}
                </div>
              </div>
              <div class="record-amount tabular">
                <span>{e.quantity.toLocaleString()} units</span>
                {#if e.unit_value_minor != null}
                  <span class="record-value" aria-label="Value then {formatMoney(e.quantity * e.unit_value_minor, ccy)}">
                    {formatMoney(e.quantity * e.unit_value_minor, ccy)}
                  </span>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
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
          Set a price from a funding round or internal valuation. It applies until the next price.
        </p>
        <div class="box">
          <div class="row wrap mark-form">
            <label class="field f-value">
              <span class="small muted">Value per unit</span>
              <input
                class="input tabular"
                inputmode="decimal"
                placeholder="29.37"
                bind:value={markForm.unit_value}
              />
            </label>
            <label class="field">
              <span class="small muted">From</span>
              <input class="input" type="date" bind:value={markForm.as_of} />
            </label>
            <label class="field f-note">
              <span class="small muted">Note (optional)</span>
              <input
                class="input"
                placeholder="e.g. Series B funding round"
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
        <div class="vsec-head">
          <h3>Recorded values</h3>
          <button class="btn btn-sm" onclick={revalue} disabled={busy}>Record today</button>
        </div>
        <p class="small muted vsec-lede">
          History updates automatically when grants, exercises or prices change. Record today to
          save a value between changes.
        </p>
        <details class="history-help">
          <summary>About recorded values</summary>
          <p class="small muted">
            Enable Show synced &amp; scheduled to see computed values. A value you set by hand
            overrides the computed value from its date until the next recorded value.
          </p>
        </details>
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
    container: equity / inline-size;
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
    width: 100%;
  }
  .total {
    text-align: left;
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
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: start;
    gap: 0 8px;
  }
  .grant-heading {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .grant-name {
    font-size: 15px;
    font-weight: 620;
    overflow-wrap: anywhere;
  }
  .grant-value {
    font-size: 18px;
    font-weight: 600;
    white-space: nowrap;
  }
  .grant-top .grant-terms {
    grid-column: 1 / -1;
    margin: 8px 0 0;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .grant-menu {
    position: relative;
    justify-self: end;
  }
  .menu-trigger {
    width: 44px;
    height: 44px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-color: transparent;
    background: transparent;
  }
  .grant-actions {
    position: absolute;
    top: 100%;
    right: 0;
    z-index: 10;
    width: 160px;
    padding: 4px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-sm);
    background: var(--surface);
    box-shadow: var(--shadow);
  }
  .grant-actions button {
    display: block;
    width: 100%;
    min-height: 44px;
    text-align: left;
    padding: 10px 12px;
    border: 0;
    border-radius: var(--r-sm);
    font: inherit;
    font-size: 14px;
    color: var(--text);
    background: transparent;
    cursor: pointer;
  }
  .grant-actions button:hover, .grant-actions button:focus-visible {
    background: var(--hover);
  }
  .grant-actions .delete-action {
    color: var(--negative);
  }
  .grant-terms, .history-help {
    margin: 8px 0 12px;
    font-size: 13px;
  }
  summary {
    color: var(--text-muted);
    cursor: pointer;
    padding: 8px 0;
  }
  details p {
    margin: 4px 0 0;
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
  .add-grant {
    margin-top: 16px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
  }
  .add {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 44px;
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
    text-align: left;
  }
  .sub-intro {
    margin: 4px 0 10px;
    max-width: 74ch;
  }
  .form-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px 10px;
  }
  .equity-form .field {
    min-width: 0;
    gap: 4px;
    text-align: left;
  }
  .equity-form .form-grid .field {
    justify-content: flex-end;
  }
  .equity-form .input {
    width: 100%;
    min-width: 0;
    min-height: 44px;
    font-size: 16px;
    text-align: left;
  }
  .equity-form input[type="date"] {
    appearance: none;
  }
  .form-optional {
    margin: 4px 0 0;
    font-size: 13px;
  }
  .form-optional summary {
    min-height: 44px;
    padding: 12px 0;
    text-align: left;
  }
  /* The note is the field with the most to say, so it takes the rest of its row rather than
     sharing a 140px cell with the numbers. */
  .form-grid .wide {
    grid-column: 1 / -1;
  }
  .form-acts {
    gap: 8px;
    flex-wrap: wrap;
    margin-top: 8px;
  }

  /* ---- activity ----------------------------------------------------------- */
  .activity-list {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .activity-record {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: start;
    gap: 12px;
    padding: 9px 0;
    border-bottom: 1px solid var(--border);
    font-size: 13px;
    line-height: 1.4;
  }
  .activity-record:last-child {
    border-bottom: 0;
  }
  .record-kind {
    font-weight: 550;
  }
  .record-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0 8px;
    color: var(--text-muted);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .record-meta time {
    white-space: nowrap;
  }
  .record-amount {
    display: grid;
    text-align: right;
    font-weight: 550;
    white-space: nowrap;
  }
  .record-value {
    color: var(--text-muted);
    font-size: 12px;
    font-weight: 400;
  }

  @container (max-width: 520px) {
    .head, .totals, .grant-foot {
      width: 100%;
    }
    .progress {
      display: grid;
      gap: 8px;
    }
    .bar {
      width: 100%;
      max-width: none;
      height: 8px;
    }
    .figures {
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 12px;
      width: 100%;
      margin: 4px 0 8px;
    }
    .figure {
      display: grid;
      grid-template-columns: 8px minmax(0, 1fr);
      gap: 4px;
      white-space: normal;
    }
    .figure .tabular {
      font-size: 16px;
      overflow-wrap: anywhere;
    }
    .swatch {
      align-self: center;
    }
    .figure-label {
      grid-column: 1 / -1;
      font-size: 12px;
    }
    .mark-form {
      display: grid;
      grid-template-columns: minmax(0, 1fr);
      gap: 14px;
    }
    .mark-form .f-value, .mark-form .f-note, .field {
      min-width: 0;
    }
    .field .input {
      width: 100%;
      min-width: 0;
    }
    .form-acts .btn, .add, .mark-act .btn, .vsec-head .btn,
    .mark-form .btn {
      min-height: 44px;
    }
    .form-acts .btn-primary, .mark-form .btn {
      flex: 1;
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
