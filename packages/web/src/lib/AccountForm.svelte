<script lang="ts">
  import { untrack } from "svelte";
  import { api, type Schemas } from "./api";
  import Icon from "./Icon.svelte";
  import {
    KINDS,
    FIELDS,
    kindToProfile,
    isLiabilityKind,
    showsInstitution,
    offersInstitution,
    isFieldRequired,
    requiresInstitution,
    requiresOpeningBalance,
    buildMetadata,
    metadataToRaw,
    valueProblem,
    type MetaField,
  } from "./accountMeta";

  let {
    account = null,
    initialKind = null,
    currencies,
    accounts,
    onsave,
    oncancel,
  }: {
    account?: Schemas["Account"] | null;
    /**
     * Pre-selected kind when creating a new account (e.g. picked from the New-account menu).
     * When set, the caller owns that choice — it names the type in its own chrome and offers a
     * way back to change it — so the Type field is left out rather than asked twice.
     */
    initialKind?: Schemas["AccountKind"] | null;
    currencies: Schemas["Currency"][];
    accounts: Schemas["Account"][];
    onsave: () => void;
    oncancel: () => void;
  } = $props();

  // The parent (re)mounts one form per account, so we snapshot the incoming values once.
  const initial = untrack(() => account);
  const editing = !!initial;
  const kindLocked = !editing && untrack(() => initialKind) != null;

  let nameEl = $state<HTMLInputElement | null>(null);
  let name = $state(initial?.name ?? "");
  let kind = $state<Schemas["AccountKind"]>(untrack(() => initial?.kind ?? initialKind ?? "bank"));
  let currency = $state(initial?.currency_code ?? "NZD");
  let institution = $state(initial?.institution ?? "");
  let raw = $state<Record<string, string>>(metadataToRaw(initial?.metadata));
  let securedBy = $state<number | "">(initial?.secured_by_account_id ?? "");
  let error = $state<string | null>(null);
  let busy = $state(false);
  /**
   * Set by the first submit attempt. The asterisks say up front which fields are required; what
   * waits for a submit is the complaint about them — a form that opens shouting is worse than one
   * that answers. From then on the complaint is derived from the live values, so filling a field
   * clears its own error.
   */
  let submitted = $state(false);
  // Required fields all live in the main section today, but a future one in "Additional details"
  // must not be reported behind a closed disclosure — `save` opens it when that happens.
  let detailsOpen = $state(false);

  // Opening balance (create only) — sent as part of the create body so the server seeds it in the
  // same transaction that inserts the account.
  let openingAmount = $state("");
  // A date past today would leave the opening balance invisible until then — `account_value_at`
  // reads the latest valuation *at or before* a date, and a transaction posted in the future is
  // simply not "as of" yet either — so today is as far forward as the input should allow.
  const today = new Date().toISOString().slice(0, 10);
  let openingDate = $state(today);

  // `kinds` on a field narrows it to specific account kinds within a shared profile (e.g. only
  // credit cards show a credit limit, though every depository kind round-trips the value).
  const visible = $derived(
    FIELDS[kindToProfile(kind)].filter((f) => !f.kinds || f.kinds.includes(kind)),
  );
  const mainFields = $derived(visible.filter((f) => (f.section ?? "main") === "main"));
  const detailFields = $derived(visible.filter((f) => f.section === "details"));
  const showSecured = $derived(isLiabilityKind(kind));
  const showInstitution = $derived(showsInstitution(kind));
  // Every kind offers an institution input (see `offersInstitution`); `showInstitution` above
  // only decides whether it earns a spot in the main row or gets tucked into "Additional
  // details" — never whether it's on the form at all.
  const offerInstitution = $derived(offersInstitution(kind));
  // Assets this liability could be secured against (never itself).
  const assets = $derived(accounts.filter((a) => a.class === "asset" && a.id !== initial?.id));

  /**
   * Kinds whose balance is accumulated from transactions, so the server records an opening
   * balance as a transaction rather than a valuation: `account_value_at`
   * (packages/app/src/reports.rs) returns the latest valuation at or before a date verbatim and
   * ignores transactions posted after it, so seeding one of these with a valuation would freeze
   * its balance for good. Every loan-shaped liability (mortgage, student loan, personal loan,
   * credit card, revolving credit, generic liability) moves through drawdowns/repayments the same
   * way a bank account moves through deposits/withdrawals, so it's transaction-seeded too —
   * property, vehicles, other assets and manually-tracked shares/crypto are the only
   * valuation-seeded kinds left. Mirrors `opening_balance_ledger` in
   * packages/dal/src/accounts.rs (`brokerage` sits in neither set — `requiresOpeningBalance`
   * hides its opening-balance fields entirely); this set is only used to word the hint below.
   */
  const TXN_SEEDED = new Set([
    "cash",
    "bank",
    "savings",
    "credit_card",
    "revolving_credit",
    "mortgage",
    "student_loan",
    "loan",
    "liability",
  ]);
  /**
   * The pair is asked for on create only, and never for a brokerage account (its value comes from
   * the holdings ledger) — which is exactly the server's rule, so `requiresOpeningBalance` decides
   * rather than a second copy of the exception here.
   */
  const showOpening = $derived(!editing && requiresOpeningBalance(kind));
  // The reference's per-accountable vocabulary for the same field.
  const openingLabel = $derived(
    isLiabilityKind(kind)
      ? "Amount owed"
      : kind === "real_estate"
        ? "Estimated market value"
        : kind === "vehicle"
          ? "Estimated value"
          : "Opening balance",
  );
  const openingHint = $derived(
    (TXN_SEEDED.has(kind)
      ? "Saved with the account as an “Opening balance” transaction on that date — this account's balance builds up from its transactions."
      : "Saved with the account as a valuation on that date — the value carries forward until the next one.") +
      (isLiabilityKind(kind) ? " Enter what you owe as a positive amount." : ""),
  );

  // Arriving here from the type picker, the name is the only thing left to decide — start there.
  $effect(() => {
    if (kindLocked) nameEl?.focus();
  });

  /** The values a select can legally hold, flat or grouped. */
  const legalValues = (f: MetaField) =>
    f.groups
      ? f.groups.flatMap((g) => g.options.map((o) => o.value))
      : (f.options ?? []).map((o) => o.value);

  /**
   * Give this kind's selects a starting value, and clear any left holding one the kind's options
   * don't offer (switching type mid-form, or editing a row whose stored subtype predates the
   * current list — the server value-checks subtypes now, so carrying it over would only earn a
   * 422).
   *
   * A *required* select is the case that needs the seed. Left undefined, Svelte adopts whichever
   * option the browser preselected — the first one — so the field would quietly answer itself
   * with a guess; seeding it to `""` (which its placeholder option matches, see `metaField`)
   * leaves it visibly unanswered instead, and a field naming a `default` gets that rather than
   * depending on its position in the list. Optional selects are left alone: an unset area unit
   * legitimately falls back to the first option, as it always has.
   */
  function seedSelects(k: Schemas["AccountKind"]) {
    for (const f of FIELDS[kindToProfile(k)]) {
      if (f.type !== "select") continue;
      const value = raw[f.key];
      if (value !== undefined && legalValues(f).includes(value)) continue;
      if (isFieldRequired(f, k) || f.default !== undefined) raw[f.key] = f.default ?? "";
      else if (value !== undefined) delete raw[f.key];
    }
  }
  seedSelects(untrack(() => kind));
  // A kind change swaps the whole field set; re-seed for the new one (a no-op on mount).
  $effect(() => {
    const k = kind;
    untrack(() => seedSelects(k));
  });

  /**
   * What this field would actually submit — its `default` stands in while the input is untouched
   * (see above), so a blank one with a default behind it counts as answered, not missing.
   */
  const effective = (f: MetaField) => (raw[f.key] ?? f.default ?? "").trim();

  // `int` fields render as real number inputs (which bring their own keypad), so only the
  // free-text numeric fields need a hint.
  const numericHint = (t: MetaField["type"]) =>
    t === "money" || t === "percent" ? "decimal" : undefined;

  const parseMajor = (v: string) => parseFloat(v.replace(/[^0-9.-]/g, ""));

  // Ids for the non-metadata inputs, kept out of the metadata key namespace.
  const NAME = "@name";
  const INSTITUTION = "@institution";
  const OPENING_AMOUNT = "@opening_amount";
  const OPENING_DATE = "@opening_date";

  /**
   * Every required field left blank, in form order, so one submit can report all of them. The
   * server checks the same table (plus the value rules — a legal subtype, a positive amount) and
   * answers with a 422; this exists so the common case never needs the round trip.
   */
  const missing = $derived.by(() => {
    const out: { id: string; label: string }[] = [];
    const blank = (v: string | undefined) => !(v ?? "").trim();
    if (blank(name)) out.push({ id: NAME, label: "Name" });
    if (offerInstitution && requiresInstitution(kind) && blank(institution)) {
      out.push({ id: INSTITUTION, label: "Institution" });
    }
    if (showOpening) {
      if (blank(openingAmount)) out.push({ id: OPENING_AMOUNT, label: openingLabel });
      if (blank(openingDate)) out.push({ id: OPENING_DATE, label: "As of" });
    }
    for (const f of visible) {
      if (isFieldRequired(f, kind, raw) && !effective(f)) out.push({ id: f.key, label: f.label });
    }
    return out;
  });

  /**
   * Per-field value-shape problems (a `0` credit limit, a negative rate, a zero model year)
   * among the currently visible fields — the client-side mirror of the server's
   * `Required::problem`, checked here so a bad value is marked on its own input instead of
   * round-tripping into a 422 naming a wire key the user never saw. A blank value is `missing`'s
   * problem, not this one's — see {@link valueProblem}.
   */
  const valueProblems = $derived.by(() => {
    const out: { id: string; message: string }[] = [];
    for (const f of visible) {
      const msg = valueProblem(f, effective(f));
      if (msg) out.push({ id: f.key, message: msg });
    }
    return out;
  });

  /** Mark an input once either its emptiness or its value has actually been reported. */
  const invalid = (id: string) =>
    submitted && (missing.some((m) => m.id === id) || valueProblems.some((p) => p.id === id));
  const missingMessage = $derived(
    missing.length === 0
      ? null
      : `${missing.length === 1 ? "This field is" : "These fields are"} required: ${missing
          .map((m) => m.label)
          .join(", ")}.`,
  );

  /** The API's error body is `{ error: { code, message } }` — show what it says when it says it. */
  function apiErrorMessage(e: unknown, fallback: string): string {
    return (e as { error?: { message?: string } })?.error?.message ?? fallback;
  }

  // Whether reporting problem `id` requires opening "Additional details" first — either it's one
  // of that section's metadata fields, or (institution's case) it's rendered there because this
  // kind doesn't earn it a spot in the main row.
  const inDetails = (id: string) =>
    detailFields.some((f) => f.key === id) || (id === INSTITUTION && !showInstitution);

  async function save() {
    // Check the required set before anything is sent: nothing is created, and every gap is named
    // at once rather than one 422 at a time.
    submitted = true;
    error = null;
    if (missing.length) {
      if (missing.some((m) => inDetails(m.id))) detailsOpen = true;
      return;
    }
    // Same idea for value-shape problems (a `0` amount, a negative rate): caught here, on the
    // field, instead of round-tripping into a 422 that names a wire key the user never saw.
    if (valueProblems.length) {
      if (valueProblems.some((p) => inDetails(p.id))) detailsOpen = true;
      error = valueProblems.map((p) => p.message).join(" ");
      return;
    }

    // The pair is required together for the kinds that show it, so `openingAmount` is non-blank
    // here; it still has to be a number.
    let opening: { opening_balance_minor: number; opening_balance_date: string } | null = null;
    if (showOpening) {
      const major = parseMajor(openingAmount);
      if (isNaN(major)) {
        error = `${openingLabel} must be a number.`;
        return;
      }
      const minor = Math.round(major * 100);
      opening = {
        // Our balances are signed and money owed is negative; the field asks for what's owed.
        opening_balance_minor: isLiabilityKind(kind) ? -Math.abs(minor) : minor,
        opening_balance_date: openingDate,
      };
    }

    busy = true;
    const body = {
      name: name.trim(),
      kind,
      currency_code: currency,
      // Offered for every kind (see `offersInstitution`); null only when the user actually left
      // it blank, never just because this kind isn't one of the ones that put it in the main
      // row — a loan's lender or a broker's broker are separate fields, not a reason to discard
      // whatever was typed here too.
      institution: institution.trim() || null,
      // `initial?.metadata` overlays onto the stored value so provider-written keys this kind's
      // form has no field for (see `buildMetadata`) survive a save instead of being dropped.
      metadata: buildMetadata(kind, raw, initial?.metadata),
      archived: initial?.archived ?? false,
      sort_order: initial?.sort_order ?? 0,
    };

    let id = initial?.id;
    if (editing && id !== undefined) {
      // No opening balance on the update path — the server refuses it, because the balance is
      // maintained through transactions/valuations once the account exists.
      const { error: e } = await api.PUT("/api/accounts/{id}", { params: { path: { id } }, body });
      if (e) {
        error = apiErrorMessage(e, "Failed to save account.");
        busy = false;
        return;
      }
    } else {
      const { data, error: e } = await api.POST("/api/accounts", { body: { ...body, ...opening } });
      if (e || !data) {
        error = apiErrorMessage(e, "Failed to create account.");
        busy = false;
        return;
      }
      id = data.id;
    }

    // Keep the secured-against link in sync for liabilities (a separate endpoint).
    if (showSecured && id !== undefined) {
      const target = securedBy === "" ? null : Number(securedBy);
      if (target !== (initial?.secured_by_account_id ?? null)) {
        await api.PUT("/api/accounts/{id}/secured-by", {
          params: { path: { id } },
          body: { secured_by_account_id: target },
        });
      }
    }

    busy = false;
    onsave();
  }
</script>

{#snippet metaField(f: MetaField)}
  {@const required = isFieldRequired(f, kind, raw)}
  {@const bad = invalid(f.key)}
  <label class="field">
    <span class="lbl" class:req={required}>{f.label}</span>
    {#if f.type === "textarea"}
      <textarea
        rows="4"
        class:invalid={bad}
        placeholder={f.placeholder ?? ""}
        aria-required={required || undefined}
        aria-invalid={bad || undefined}
        bind:value={raw[f.key]}
      ></textarea>
    {:else if f.type === "select"}
      <select
        class="select"
        class:invalid={bad}
        aria-required={required || undefined}
        aria-invalid={bad || undefined}
        bind:value={raw[f.key]}
      >
        {#if f.groups}
          <!-- Flat option lists carry their own "None" entry; grouped ones are pure
               category groups, so the empty choice belongs to the select itself. -->
          <option value="">None</option>
          {#each f.groups as g (g.label)}
            <optgroup label={g.label}>
              {#each g.options as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
            </optgroup>
          {/each}
        {:else}
          <!--
            A required list has no "None" (answering "none" isn't answering), so it needs a
            placeholder to sit on until it's answered — otherwise the browser preselects the first
            real option and the field looks answered when nobody chose anything. Fields naming a
            `default` are answered from the start and get no placeholder.
          -->
          {#if required && f.default === undefined && !legalValues(f).includes("")}
            <option value="">Select…</option>
          {/if}
          {#each f.options ?? [] as o (o.value)}<option value={o.value}>{o.label}</option>{/each}
        {/if}
      </select>
    {:else if f.type === "date"}
      <input
        class="input"
        class:invalid={bad}
        type="date"
        aria-required={required || undefined}
        aria-invalid={bad || undefined}
        bind:value={raw[f.key]}
      />
    {:else if f.type === "int"}
      <!--
        Whole numbers get a real number input so min/max (e.g. a plausible year built) and the
        stepper come for free. `bind:value` would coerce the value to a number and `raw` has to
        stay all-strings for buildMetadata, hence the explicit oninput.
      -->
      <input
        class="input tabular"
        class:invalid={bad}
        type="number"
        step="1"
        min={f.min}
        max={f.max}
        placeholder={f.placeholder ?? ""}
        aria-required={required || undefined}
        aria-invalid={bad || undefined}
        value={raw[f.key] ?? ""}
        oninput={(e) => (raw[f.key] = e.currentTarget.value)}
        onwheel={(e) => e.currentTarget.blur()}
      />
    {:else}
      <input
        class="input"
        class:tabular={f.type === "money" || f.type === "percent"}
        class:invalid={bad}
        inputmode={numericHint(f.type)}
        placeholder={f.placeholder ?? ""}
        aria-required={required || undefined}
        aria-invalid={bad || undefined}
        bind:value={raw[f.key]}
      />
    {/if}
    {#if f.hint}
      <span class="hint small faint">{f.hint}</span>
    {/if}
  </label>
{/snippet}

{#snippet institutionField()}
  {@const required = requiresInstitution(kind)}
  <label class="field">
    <span class="lbl" class:req={required}>Institution</span>
    <input
      class="input"
      class:invalid={invalid(INSTITUTION)}
      placeholder="e.g. ANZ"
      aria-required={required || undefined}
      aria-invalid={invalid(INSTITUTION) || undefined}
      bind:value={institution}
    />
  </label>
{/snippet}

<section class="card" style="margin-bottom:14px">
  <h2 style="margin-bottom:4px">{editing ? `Edit ${initial?.name}` : "New account"}</h2>

  {#if error}<div class="error-banner" style="margin:8px 0">{error}</div>{/if}
  {#if submitted && missingMessage}
    <div class="error-banner" style="margin:8px 0">{missingMessage}</div>
  {/if}

  <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-top:8px">
    <label class="field">
      <span class="lbl req">Name</span>
      <input
        class="input"
        class:invalid={invalid(NAME)}
        aria-required="true"
        aria-invalid={invalid(NAME) || undefined}
        bind:this={nameEl}
        bind:value={name}
      />
    </label>
    {#if !kindLocked}
      <label class="field">
        <span class="lbl">Type</span>
        <select class="select" bind:value={kind}>
          {#each KINDS as k}<option value={k.value}>{k.label}</option>{/each}
        </select>
      </label>
    {/if}
    <label class="field">
      <span class="lbl">Currency</span>
      <select class="select" bind:value={currency}>
        {#each currencies as c}<option value={c.code}>{c.code}</option>{/each}
      </select>
    </label>
    {#if showInstitution}
      {@render institutionField()}
    {/if}
  </div>

  {#if showOpening}
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-top:12px">
      <label class="field">
        <span class="lbl req">{openingLabel}</span>
        <input
          class="input tabular"
          class:invalid={invalid(OPENING_AMOUNT)}
          inputmode="decimal"
          placeholder="0.00"
          aria-required="true"
          aria-invalid={invalid(OPENING_AMOUNT) || undefined}
          bind:value={openingAmount}
        />
      </label>
      <label class="field">
        <span class="lbl req">As of</span>
        <input
          class="input"
          class:invalid={invalid(OPENING_DATE)}
          type="date"
          max={today}
          aria-required="true"
          aria-invalid={invalid(OPENING_DATE) || undefined}
          bind:value={openingDate}
        />
      </label>
    </div>
    <p class="small faint" style="margin-top:6px">{openingHint}</p>
  {/if}

  {#if mainFields.length}
    <div class="details">Details</div>
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr))">
      {#each mainFields as f (f.key)}{@render metaField(f)}{/each}
    </div>
  {/if}

  {#if showSecured}
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr));margin-top:6px">
      <label class="field">
        <span class="lbl">Secured against</span>
        <select class="select" bind:value={securedBy}>
          <option value="">Not secured</option>
          {#each assets as a}<option value={a.id}>{a.name}</option>{/each}
        </select>
      </label>
    </div>
  {/if}

  {#if detailFields.length || (offerInstitution && !showInstitution)}
    <details class="more" bind:open={detailsOpen}>
      <summary>
        <span class="chev"><Icon name="chevron-right" size={16} /></span>
        Additional details
      </summary>
      <div class="more-body grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr))">
        {#if offerInstitution && !showInstitution}{@render institutionField()}{/if}
        {#each detailFields as f (f.key)}{@render metaField(f)}{/each}
      </div>
    </details>
  {/if}

  <div class="row" style="justify-content:flex-end;gap:8px;margin-top:14px">
    <button class="btn" onclick={oncancel} disabled={busy}>Cancel</button>
    <!-- Deliberately not disabled while fields are missing: the click is what reports them. -->
    <button class="btn btn-primary" onclick={save} disabled={busy}>
      {editing ? "Save" : "Create"}
    </button>
  </div>
</section>

<style>
  /* label.field is a flex column, so the label text lives in its own row-level span — that keeps
     the required marker on the same line as the text rather than making it a third row. */
  .lbl {
    display: inline-flex;
    align-items: baseline;
    gap: 2px;
  }
  /* The required marker is generated content, not a DOM node: the control already carries
     `aria-required`, so the glyph is pure decoration — and keeping it out of the DOM keeps the
     label's *text* exactly the field's name, which is what everything matching a field by its
     label (assistive tech, `getByLabel`, an autofill heuristic) reads. */
  .lbl.req::after {
    content: "*";
    color: var(--negative);
    font-weight: 600;
  }
  /* Matches the error banner's colour so a field and its report read as one thing. */
  .invalid {
    border-color: color-mix(in srgb, var(--negative) 60%, var(--border-strong));
  }
  .invalid:focus {
    outline-color: color-mix(in srgb, var(--negative) 40%, transparent);
    border-color: var(--negative);
  }
  .details {
    margin: 16px 0 8px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
    border-top: 1px solid var(--border);
    padding-top: 12px;
  }
  /* The reference's collapsed disclosure: chevron that rotates open, contents indented behind a
     rule so they read as a nested aside rather than more of the main form. */
  details.more {
    margin-top: 16px;
    border-top: 1px solid var(--border);
    padding-top: 6px;
  }
  details.more > summary {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 0;
    font-size: 13px;
    color: var(--text-muted);
    cursor: pointer;
    list-style: none;
  }
  details.more > summary::-webkit-details-marker {
    display: none;
  }
  details.more > summary:hover {
    color: var(--text);
  }
  .chev {
    display: inline-flex;
    transition: transform 120ms ease;
  }
  details.more[open] .chev {
    transform: rotate(90deg);
  }
  .more-body {
    margin-top: 8px;
    padding-left: 14px;
    border-left: 1px solid var(--border);
  }
</style>
