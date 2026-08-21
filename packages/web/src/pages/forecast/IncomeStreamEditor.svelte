<script lang="ts">
  // One income stream, edited in place. Also the add form — same fields, so one component.
  import { untrack } from "svelte";
  import { api, formatMoney, type Schemas } from "../../lib/api";

  type IncomeStream = Schemas["IncomeStream"];

  let {
    stream = null,
    personId,
    onsaved,
    oncancel,
    ondelete,
  }: {
    stream?: IncomeStream | null;
    personId: number;
    onsaved: () => void;
    oncancel: () => void;
    ondelete?: () => void;
  } = $props();

  // Snapshot the incoming values once, so a reload of the list underneath does not stamp on what
  // is being typed (`AccountForm` does the same).
  const initial = untrack(() => stream);

  const today = new Date().toISOString().slice(0, 10);
  let f = $state({
    label: initial?.label ?? "",
    employer: initial?.employer ?? "",
    amount: initial ? (initial.annual_amount_minor / 100).toString() : "",
    basis: initial?.basis ?? ("gross_nz_paye" as Schemas["IncomeBasis"]),
    pay_frequency: initial?.pay_frequency ?? ("fortnightly" as Schemas["PayFrequency"]),
    first_payment_on: initial?.first_payment_on ?? today,
    starts_on: initial?.starts_on ?? today,
    ends_on: initial?.ends_on ?? "",
    annual_increase: ((initial?.annual_increase_bps ?? 0) / 100).toString(),
    kiwisaver: ((initial?.kiwisaver_bps ?? 350) / 100).toString(),
    // Left blank for a new stream and filled once the tax rates load, because the compulsory
    // employer minimum is a dated setting rather than a constant — hardcoding today's 3.5% here
    // would quietly outlive the next change to it. An existing stream keeps what it was saved with.
    employer_kiwisaver:
      initial != null ? (initial.employer_kiwisaver_bps / 100).toString() : "",
    kiwisaver_account_id: initial?.kiwisaver_account_id ?? null,
    student_loan: initial?.student_loan ?? false,
    student_loan_account_id: initial?.student_loan_account_id ?? null,
    take_home: initial?.take_home_bps != null ? (initial.take_home_bps / 100).toString() : "",
    linked_category_id: initial?.linked_category_id ?? null,
    currency_code: initial?.currency_code ?? "NZD",
    enabled: initial?.enabled ?? true,
    pay_treatment: initial?.pay_treatment ?? ("regular" as Schemas["PayTreatment"]),
    match_account_id: initial?.match_account_id ?? null,
    match_pattern: initial?.match_pattern ?? "",
  });
  let steps = $state<{ effective_on: string; amount: string; label: string }[]>(
    (initial?.steps ?? []).map((s) => ({
      effective_on: s.effective_on,
      amount: (s.annual_amount_minor / 100).toString(),
      label: s.label ?? "",
    }))
  );

  // Complain on submit, never on open: an empty form someone has just started is not an error yet.
  let submitted = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  const labelMissing = $derived(submitted && !f.label.trim());
  const amountMissing = $derived(submitted && !parseFloat(f.amount));

  let categories = $state<{ id: number; name: string }[]>([]);
  $effect(() => {
    api.GET("/api/categories", {}).then(({ data }) => {
      // Only income categories can receive pay, so offering an expense one would only ever create
      // a mis-link the reconciliation then has to explain.
      categories = (data ?? [])
        .filter((c) => c.kind === "income")
        .map((c) => ({ id: c.id, name: c.name }));
    });
  });

  /**
   * The compulsory employer contribution in force today, from Settings → Tax rates.
   *
   * Read rather than assumed: it was 3% until 1 April 2026 and is legislated to reach 4% in 2028,
   * so the number a new stream should start at is a question only the dated scales can answer.
   */
  let employerMinBps = $state<number | null>(null);
  $effect(() => {
    api.GET("/api/tax-scales", {}).then(({ data }) => {
      const inForce = (data ?? [])
        .filter((s) => s.effective_from <= today)
        .sort((a, b) => a.effective_from.localeCompare(b.effective_from))
        .at(-1);
      if (!inForce) return;
      employerMinBps = inForce.kiwisaver_employer_min_bps;
      // Only ever fills a blank field: someone who has already typed a figure while this was in
      // flight has said something more specific than the statutory default.
      untrack(() => {
        if (!initial && f.employer_kiwisaver === "") {
          f.employer_kiwisaver = (inForce.kiwisaver_employer_min_bps / 100).toString();
        }
      });
    });
  });

  let detected = $state<Schemas["DetectedStream"][]>([]);
  let detecting = $state(false);
  let dismissed = $state(false);

  /**
   * Salaries already visible in the ledger.
   *
   * Offered only when adding, because the details people get wrong are exactly the ones the ledger
   * already knows — whether "fortnightly" means every fourteen days or twice a month, which day it
   * lands, and what the net figure actually is after payroll.
   */
  $effect(() => {
    if (initial) return;
    detecting = true;
    api.GET("/api/income-streams/detect", { params: { query: {} } }).then(({ data }) => {
      detected = data ?? [];
      detecting = false;
    });
  });

  /** Fill the form from a detected salary. The figures are net, because they are what landed. */
  function useDetected(d: Schemas["DetectedStream"]) {
    f.label = d.label;
    f.amount = (d.annual_net_minor / 100).toString();
    f.basis = "net";
    f.pay_frequency = d.pay_frequency;
    f.first_payment_on = d.next_payment_on;
    f.starts_on = d.next_payment_on;
    f.currency_code = d.currency_code;
    if (d.category_id != null) f.linked_category_id = d.category_id;
    // The detector's grouping token is the memo's stable prefix — exactly what the matcher
    // should look for, in exactly the account it was seen in. Never `d.label`: that is one
    // whole memo, usually with a per-run suffix that would match a single deposit.
    f.match_account_id = d.account_id;
    f.match_pattern = d.match_pattern;
    dismissed = true;
  }

  function freqWords(freq: Schemas["PayFrequency"]): string {
    switch (freq) {
      case "weekly":
        return "weekly";
      case "fortnightly":
        return "every 2 weeks";
      case "four_weekly":
        return "every 4 weeks";
      case "semi_monthly":
        return "twice a month";
      case "monthly":
        return "monthly";
      case "quarterly":
        return "quarterly";
      case "annual":
        return "once a year";
    }
  }

  let accounts = $state<Schemas["Account"][]>([]);
  $effect(() => {
    api.GET("/api/accounts", {}).then(({ data }) => (accounts = data ?? []));
  });
  // KiwiSaver is an investment; a student loan is a student loan. Offering every account would let
  // someone point contributions at their mortgage, which the projection would then dutifully model.
  const kiwisaverAccounts = $derived(
    accounts.filter((a) => ["brokerage", "shares_nz", "shares_us", "shares_private"].includes(a.kind))
  );
  const studentLoanAccounts = $derived(accounts.filter((a) => a.kind === "student_loan"));

  /**
   * Append a step, pre-filled a year on from the last one at 3% more.
   *
   * A published pay scale is an annual anniversary step, so this turns "fill in six rows" into
   * "click six times and fix two numbers". The figures are deliberately round and obviously
   * editable — a starting point to argue with, not an estimate being asserted.
   */
  function addStep() {
    const last = steps.at(-1);
    const base = last ? parseFloat(last.amount) || 0 : parseFloat(f.amount) || 0;
    const from = last ? new Date(last.effective_on) : new Date(f.starts_on);
    from.setFullYear(from.getFullYear() + 1);
    steps.push({
      effective_on: from.toISOString().slice(0, 10),
      amount: Math.round(base * 1.03).toString(),
      label: "",
    });
  }

  const maxStep = $derived(
    Math.max(parseFloat(f.amount) || 0, ...steps.map((s) => parseFloat(s.amount) || 0))
  );

  async function save() {
    submitted = true;
    if (!f.label.trim() || !parseFloat(f.amount)) return;
    saving = true;
    error = null;
    const body: Schemas["SaveIncomeStream"] = {
      label: f.label.trim(),
      employer: f.employer.trim() || null,
      currency_code: f.currency_code,
      annual_amount_minor: Math.round(parseFloat(f.amount) * 100),
      basis: f.basis,
      pay_frequency: f.pay_frequency,
      first_payment_on: f.first_payment_on,
      starts_on: f.starts_on,
      ends_on: f.ends_on || null,
      annual_increase_bps: Math.round(parseFloat(f.annual_increase || "0") * 100),
      kiwisaver_bps: Math.round(parseFloat(f.kiwisaver || "0") * 100),
      employer_kiwisaver_bps: Math.round(parseFloat(f.employer_kiwisaver || "0") * 100),
      kiwisaver_account_id: f.kiwisaver_account_id,
      student_loan: f.student_loan,
      student_loan_account_id: f.student_loan ? f.student_loan_account_id : null,
      take_home_bps: f.take_home ? Math.round(parseFloat(f.take_home) * 100) : null,
      linked_category_id: f.linked_category_id,
      enabled: f.enabled,
      pay_treatment: f.pay_treatment,
      match_account_id: f.match_account_id,
      match_pattern: f.match_pattern.trim() || null,
      steps: steps
        .filter((s) => s.effective_on && parseFloat(s.amount))
        .map((s) => ({
          effective_on: s.effective_on,
          annual_amount_minor: Math.round(parseFloat(s.amount) * 100),
          label: s.label.trim() || null,
        })),
    };
    const res = initial
      ? await api.PUT("/api/income-streams/{id}", {
          params: { path: { id: initial.id } },
          body,
        })
      : await api.POST("/api/people/{person_id}/income-streams", {
          params: { path: { person_id: personId } },
          body,
        });
    saving = false;
    if (res.error) {
      // Every problem arrives in one message, so it is shown whole rather than split per field.
      error =
        (res.error as { error?: { message?: string } }).error?.message ??
        "Could not save this income.";
      return;
    }
    onsaved();
  }
</script>

<div class="editor">
  {#if error}<div class="error-banner small">{error}</div>{/if}

  {#if !initial && !dismissed && detected.length > 0}
    <div class="found">
      <div class="row spread" style="margin-bottom:6px">
        <strong class="small">Found in your transactions</strong>
        <button class="btn btn-sm" onclick={() => (dismissed = true)}>Enter it myself</button>
      </div>
      {#each detected.slice(0, 4) as d (d.label + d.last_paid_on)}
        <button type="button" class="found-row" onclick={() => useDetected(d)}>
          <span class="fr-main">
            <strong>{d.label}</strong>
            <span class="faint">·</span>
            <span class="tabular">{formatMoney(d.per_payment_minor, d.currency_code)}</span>
            <span class="faint">{freqWords(d.pay_frequency)}</span>
            {#if d.pay_frequency === "semi_monthly"}
              <!-- The distinction the detector exists for: two fixed days a month is 24 payments a
                   year, not the 26 that "fortnightly" implies. Show the evidence. -->
              <span class="badge">on the {d.days_of_month.join(" & ")}</span>
            {/if}
          </span>
          <span class="fr-sub small faint">
            {d.payments_seen} payments · about {formatMoney(d.annual_net_minor, d.currency_code)}/yr
            take-home
            {#if d.variability_bps > 2_000}
              · <span class="warn-text">the amount varies a lot, so check the figure</span>
            {/if}
          </span>
        </button>
      {/each}
    </div>
  {:else if !initial && detecting}
    <div class="small faint" style="margin-bottom:10px">Looking for salaries in your transactions…</div>
  {/if}

  <div class="grid-fields">
    <label class="field">
      <span class="lbl req">What is it</span>
      <input
        class="input"
        class:invalid={labelMissing}
        aria-invalid={labelMissing}
        placeholder="Salary"
        bind:value={f.label}
      />
    </label>
    <label class="field">
      <span class="lbl">Employer</span>
      <input class="input" placeholder="optional" bind:value={f.employer} />
    </label>
    <label class="field">
      <span class="lbl req">Amount per year</span>
      <input
        class="input tabular"
        class:invalid={amountMissing}
        aria-invalid={amountMissing}
        placeholder="88000"
        bind:value={f.amount}
      />
    </label>
  </div>

  <div class="grid-fields">
    <div class="field">
      <!-- A segmented control, never a select: the entire failure mode this screen guards against
           is not noticing which one is set, so both have to be visible without opening anything.
           And plain English — "take-home" is the word a payslip reader owns. -->
      <span class="lbl req">That amount is</span>
      <div class="seg" role="group" aria-label="Before tax or take-home">
        <button
          type="button"
          class="seg-btn"
          class:on={f.basis === "gross_nz_paye"}
          onclick={() => (f.basis = "gross_nz_paye")}>Before tax</button
        >
        <button
          type="button"
          class="seg-btn"
          class:on={f.basis === "net"}
          onclick={() => (f.basis = "net")}>Take-home</button
        >
      </div>
    </div>
    <label class="field">
      <span class="lbl req">Paid</span>
      <select class="select" bind:value={f.pay_frequency}>
        <option value="weekly">Weekly</option>
        <option value="fortnightly">Fortnightly</option>
        <option value="four_weekly">Every 4 weeks</option>
        <!-- Not the same as fortnightly, and constantly confused with it: twice a month is 24
             payments a year where every fourteen days is 26. -->
        <option value="semi_monthly">Twice a month</option>
        <option value="monthly">Monthly</option>
        <option value="quarterly">Quarterly</option>
        <option value="annual">Once a year</option>
      </select>
    </label>
    <label class="field">
      <span class="lbl req">First payment</span>
      <!-- Not cosmetic: this is what puts a quarterly payment in the month it really lands in. -->
      <input class="input" type="date" bind:value={f.first_payment_on} />
    </label>
    <label class="field">
      <span class="lbl">Lands in</span>
      <select class="select" bind:value={f.linked_category_id}>
        <option value={null}>Not linked</option>
        {#each categories as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
      </select>
    </label>
  </div>

  {#if f.basis === "gross_nz_paye"}
    <div class="grid-fields">
      <div class="field">
        <!-- A bonus paid inside the regular pay run is an IRD "extra pay": taxed as a lump on
             top of the salary, student loan with no threshold. It changes every reconstructed
             payslip, so it is a visible control rather than a details row. -->
        <span class="lbl">Paid as</span>
        <div class="seg" role="group" aria-label="Regular pay or bonus">
          <button
            type="button"
            class="seg-btn"
            class:on={f.pay_treatment === "regular"}
            onclick={() => (f.pay_treatment = "regular")}>Regular pay</button
          >
          <button
            type="button"
            class="seg-btn"
            class:on={f.pay_treatment === "extra_pay"}
            onclick={() => (f.pay_treatment = "extra_pay")}>Bonus / extra pay</button
          >
        </div>
      </div>
      <label class="field">
        <span class="lbl">KiwiSaver, you %</span>
        <input class="input tabular" bind:value={f.kiwisaver} />
      </label>
      <label class="field">
        <span class="lbl">KiwiSaver, employer %</span>
        <input class="input tabular" bind:value={f.employer_kiwisaver} />
        {#if employerMinBps != null}
          <span class="small faint">
            minimum {(employerMinBps / 100).toFixed(2)}%
          </span>
        {/if}
      </label>
      <label class="field">
        <span class="lbl">Student loan</span>
        <label class="switch"><input type="checkbox" bind:checked={f.student_loan} /><span
          ></span></label>
      </label>
      <label class="field">
        <span class="lbl">Take-home % override</span>
        <input class="input tabular" placeholder="from tax rates" bind:value={f.take_home} />
      </label>
    </div>

    <div class="grid-fields">
      <label class="field">
        <span class="lbl">KiwiSaver goes to</span>
        <select class="select" bind:value={f.kiwisaver_account_id}>
          <option value={null}>Not tracked</option>
          {#each kiwisaverAccounts as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
        </select>
      </label>
      {#if f.student_loan}
        <label class="field">
          <span class="lbl">Repayments pay down</span>
          <select class="select" bind:value={f.student_loan_account_id}>
            <option value={null}>Not tracked</option>
            {#each studentLoanAccounts as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
          </select>
        </label>
      {/if}
    </div>
    {#if f.kiwisaver_account_id !== null || f.student_loan_account_id !== null}
      <!-- Saying this up front, because it changes what those accounts' numbers mean and the
           consequence is invisible otherwise: a balance that grew while contributions were flowing
           in cannot tell market growth and contributions apart, so the measured rate has to go. -->
      <p class="small faint" style="margin:0 0 10px">
        Linking an account means its own measured growth rate is set aside — otherwise the money
        would be counted twice. Set an expected return on it in the Assumptions tab, or it is
        projected flat.
      </p>
    {/if}
  {/if}

  <!-- Always visible, never folded into a details row: matching is what turns a configured
       stream into checked-off paydays and the payslip layer on the cash-flow chart, and a
       collapsed section proved invisible in practice — a household set everything else up and
       could not see why nothing matched. Both halves or neither: an account to look in and a
       memo token to look for. -->
  <div class="match-block">
    <div class="row spread" style="margin-bottom:6px">
      <strong class="small">Match deposits automatically</strong>
      {#if f.match_account_id === null}
        <span class="badge">off</span>
      {/if}
    </div>
    <div class="grid-fields">
      <label class="field">
        <span class="lbl">Lands in account</span>
        <select class="select" bind:value={f.match_account_id}>
          <option value={null}>Not matched</option>
          {#each accounts as a (a.id)}<option value={a.id}>{a.name}</option>{/each}
        </select>
      </label>
      <label class="field">
        <span class="lbl">Deposit memo contains</span>
        <input class="input" placeholder="e.g. the employer's name" bind:value={f.match_pattern} />
      </label>
    </div>
    <p class="small faint" style="margin:0">
      With these set, each payday is checked off against the deposit that satisfied it and the
      cash-flow chart draws the payslip behind it. A bonus paid inside the salary run should use
      the same account and memo as the salary — the two are matched against the one deposit.
    </p>
  </div>

  <details class="more" open={steps.length > 0}>
    <summary>Pay scale, start and end</summary>
    <div class="grid-fields" style="margin-top:10px">
      <label class="field">
        <span class="lbl req">Starts</span>
        <input class="input" type="date" bind:value={f.starts_on} />
      </label>
      <label class="field">
        <span class="lbl">Ends</span>
        <input class="input" type="date" bind:value={f.ends_on} />
      </label>
      <label class="field">
        <span class="lbl">Rise per year after the last step %</span>
        <input class="input tabular" bind:value={f.annual_increase} />
      </label>
    </div>

    <div class="steps">
      {#each steps as s, i (i)}
        <div class="step-row">
          <input class="input" type="date" bind:value={s.effective_on} />
          <input class="input tabular" bind:value={s.amount} />
          <input class="input" placeholder="Step 5" bind:value={s.label} />
          <!-- A bar per step: a mis-keyed order of magnitude is invisible in a number column and
               obvious the moment it is drawn. -->
          <div class="bar-track">
            <div
              class="bar"
              style="width:{maxStep > 0 ? ((parseFloat(s.amount) || 0) / maxStep) * 100 : 0}%"
            ></div>
          </div>
          <button class="btn btn-sm btn-danger" onclick={() => steps.splice(i, 1)}>✕</button>
        </div>
      {/each}
      <button class="btn btn-sm" onclick={addStep}>+ Add step</button>
    </div>
  </details>

  <div class="row" style="justify-content:space-between;margin-top:12px">
    {#if initial && ondelete}
      <button class="btn btn-sm btn-danger" onclick={ondelete}>Remove</button>
    {:else}<span></span>{/if}
    <div class="row" style="gap:8px">
      <label class="row small faint" style="gap:6px">
        <input type="checkbox" bind:checked={f.enabled} /> Include in the forecast
      </label>
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
  .seg {
    display: inline-flex;
    padding: 2px;
    gap: 2px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
  }
  .seg-btn {
    appearance: none;
    border: none;
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: 12px;
    padding: 4px 10px;
    border-radius: calc(var(--r-sm) - 2px);
    cursor: pointer;
    flex: 1;
  }
  .seg-btn.on {
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 600;
  }
  .seg-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .more > summary {
    cursor: pointer;
    font-size: 12px;
    color: var(--text-muted);
  }
  .match-block {
    margin-bottom: 10px;
    padding: 10px;
    border-radius: var(--r-sm);
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .steps {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 8px;
  }
  .step-row {
    display: grid;
    grid-template-columns: minmax(120px, 1fr) minmax(80px, 0.7fr) minmax(80px, 1fr) 60px auto;
    gap: 8px;
    align-items: center;
  }
  .bar-track {
    height: 6px;
    background: var(--surface);
    border-radius: 3px;
    overflow: hidden;
  }
  .bar {
    height: 100%;
    background: var(--who, var(--accent));
  }
  .found {
    margin-bottom: 12px;
    padding: 10px;
    border-radius: var(--r-sm);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, var(--border));
    background: color-mix(in srgb, var(--accent) 6%, transparent);
  }
  .found-row {
    all: unset;
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    box-sizing: border-box;
    cursor: pointer;
    padding: 6px 8px;
    border-radius: var(--r-sm);
  }
  .found-row:hover {
    background: var(--hover);
  }
  .found-row:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .fr-main {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    font-size: 13px;
  }
  .warn-text {
    color: var(--warn);
  }
</style>
