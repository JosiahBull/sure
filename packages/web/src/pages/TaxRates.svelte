<script lang="ts">
  // The tax rules the forecast uses, as data you can edit.
  //
  // These are external facts with a shelf life: IRD moves a threshold and every projection is
  // quietly wrong until someone ships a new binary. Scales are dated and the latest one not after a
  // given date wins, so recording next year's rates is adding a row rather than editing this one —
  // which also keeps last year's projection reproducible.
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../lib/api";

  type Scale = Schemas["StoredTaxScale"];
  type Band = [number | null, number];

  let scales = $state<Scale[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let editing = $state<number | null>(null);
  let adding = $state(false);
  let confirmRestore = $state(false);

  async function load() {
    loading = true;
    const { data, error: e } = await api.GET("/api/tax-scales", {});
    scales = data ?? [];
    error = e ? "Failed to load tax rates." : null;
    loading = false;
  }
  onMount(load);

  // A draft is plain strings, because a half-typed percentage is not a number yet and coercing on
  // every keystroke fights the person typing it.
  type Draft = {
    effective_from: string;
    brackets: { upper: string; rate: string }[];
    esct: { upper: string; rate: string }[];
    acc_levy: string;
    acc_cap: string;
    sl_threshold: string;
    sl_rate: string;
    employer_min: string;
    govt_match: string;
    govt_max: string;
    govt_cap: string;
    source_note: string;
  };

  const bands = (b: Band[]) =>
    b.map(([upper, rate]) => ({
      upper: upper == null ? "" : (upper / 100).toString(),
      rate: (rate / 100).toString(),
    }));

  function draftOf(s: Scale | null): Draft {
    if (!s) {
      // A new scale starts as a copy of the newest one — next year's rules are almost always this
      // year's with one number moved, and starting from an empty table would be pointless typing.
      const latest = scales.at(-1);
      if (latest) {
        const d = draftOf(latest);
        d.effective_from = "";
        d.source_note = "";
        return d;
      }
    }
    return {
      effective_from: s?.effective_from ?? "",
      brackets: s ? bands(s.brackets as Band[]) : [{ upper: "", rate: "" }],
      esct: s ? bands(s.esct_brackets as Band[]) : [{ upper: "", rate: "" }],
      acc_levy: s ? (s.acc_levy_bps / 100).toString() : "",
      acc_cap: s ? (s.acc_income_cap_minor / 100).toString() : "",
      sl_threshold: s ? (s.student_loan_threshold_minor / 100).toString() : "",
      sl_rate: s ? (s.student_loan_rate_bps / 100).toString() : "",
      employer_min: s ? (s.kiwisaver_employer_min_bps / 100).toString() : "",
      govt_match: s ? (s.kiwisaver_govt_match_bps / 100).toString() : "",
      govt_max: s ? (s.kiwisaver_govt_max_minor / 100).toString() : "",
      govt_cap:
        s?.kiwisaver_govt_income_cap_minor == null
          ? ""
          : (s.kiwisaver_govt_income_cap_minor / 100).toString(),
      source_note: s?.source_note ?? "",
    };
  }

  let f = $state<Draft>(draftOf(null));

  function startEdit(s: Scale) {
    editing = s.id;
    adding = false;
    f = draftOf(s);
  }
  function startAdd() {
    adding = true;
    editing = null;
    f = draftOf(null);
  }

  const money = (v: string) => Math.round(parseFloat(v || "0") * 100);
  const bps = (v: string) => Math.round(parseFloat(v || "0") * 100);
  const toBands = (rows: { upper: string; rate: string }[]): Band[] =>
    rows.map((r) => [r.upper.trim() === "" ? null : money(r.upper), bps(r.rate)]);

  async function save() {
    const body = {
      effective_from: f.effective_from,
      brackets: toBands(f.brackets),
      esct_brackets: toBands(f.esct),
      acc_levy_bps: bps(f.acc_levy),
      acc_income_cap_minor: money(f.acc_cap),
      student_loan_threshold_minor: money(f.sl_threshold),
      student_loan_rate_bps: bps(f.sl_rate),
      kiwisaver_employer_min_bps: bps(f.employer_min),
      kiwisaver_govt_match_bps: bps(f.govt_match),
      kiwisaver_govt_max_minor: money(f.govt_max),
      kiwisaver_govt_income_cap_minor: f.govt_cap.trim() === "" ? null : money(f.govt_cap),
      source_note: f.source_note.trim() || null,
    } as Schemas["SaveTaxScale"];

    const res = editing
      ? await api.PUT("/api/tax-scales/{id}", { params: { path: { id: editing } }, body })
      : await api.POST("/api/tax-scales", { body });
    if (res.error) {
      // Every problem arrives in one message, so it is shown whole rather than split per field.
      error =
        (res.error as { error?: { message?: string } }).error?.message ??
        "Could not save these rates.";
      return;
    }
    editing = null;
    adding = false;
    error = null;
    await load();
  }

  async function remove(id: number) {
    const { error: e } = await api.DELETE("/api/tax-scales/{id}", { params: { path: { id } } });
    if (e) {
      error =
        (e as { error?: { message?: string } }).error?.message ?? "Could not remove these rates.";
      return;
    }
    await load();
  }

  async function restore() {
    const { error: e } = await api.POST("/api/tax-scales/restore", {});
    if (e) error = "Could not restore the built-in rates.";
    confirmRestore = false;
    await load();
  }

  /** "$15,600 – $53,500 at 17.5%", built from the band above it. */
  function bandLabel(b: Band[], i: number): string {
    const lower = i === 0 ? 0 : (b[i - 1][0] ?? 0);
    const upper = b[i][0];
    const from = formatMoney(lower, "NZD");
    return upper == null
      ? `over ${from}`
      : `${from} – ${formatMoney(upper, "NZD")}`;
  }
</script>

<div class="row spread" style="margin-bottom:14px">
  <h1 style="font-size:20px;margin:0">Tax rates</h1>
  <div class="row" style="gap:8px">
    <button class="btn btn-sm" onclick={() => (confirmRestore = true)}>Restore built-in</button>
    <button class="btn btn-sm btn-primary" onclick={startAdd}>+ Add a year</button>
  </div>
</div>

<p class="muted small" style="margin:0 0 14px">
  What the forecast uses to turn a salary into take-home. Rates are dated: the latest set that has
  taken effect wins, so recording a change is adding a year rather than editing this one — which
  keeps last year's projection reproducible.
</p>

{#if error}<div class="error-banner" style="margin-bottom:16px">{error}</div>{/if}

{#if confirmRestore}
  <div class="card confirm" style="margin-bottom:16px">
    <div class="row spread wrap" style="gap:10px">
      <span class="small">
        Replace every set of rates with the figures built into this release? Anything you have
        edited here is discarded.
      </span>
      <div class="row" style="gap:8px">
        <button class="btn btn-sm btn-danger" onclick={restore}>Restore</button>
        <button class="btn btn-sm" onclick={() => (confirmRestore = false)}>Cancel</button>
      </div>
    </div>
  </div>
{/if}

{#if loading && scales.length === 0}
  <div class="row" style="justify-content:center;padding:40px"><span class="spinner"></span></div>
{:else}
  {#if adding}
    <section class="card" style="margin-bottom:16px">
      <h2 style="margin:0 0 10px">New rates</h2>
      {@render editor()}
    </section>
  {/if}

  <div class="grid cards">
    {#each scales as s (s.id)}
      <section class="card">
        <div class="card-title">
          <h2>From {s.effective_from}</h2>
          <div class="row" style="gap:6px">
            <button class="btn btn-sm" onclick={() => (editing === s.id ? (editing = null) : startEdit(s))}>
              {editing === s.id ? "Cancel" : "Edit"}
            </button>
            {#if scales.length > 1}
              <button class="btn btn-sm btn-danger" onclick={() => remove(s.id)}>✕</button>
            {/if}
          </div>
        </div>

        {#if editing === s.id}
          {@render editor()}
        {:else}
          <div class="summary">
            <div>
              <div class="lbl">Income tax</div>
              <ul class="bandlist small">
                {#each s.brackets as _b, i (i)}
                  <li>
                    <span class="faint">{bandLabel(s.brackets as Band[], i)}</span>
                    <span class="tabular">{((s.brackets as Band[])[i][1] / 100).toFixed(2)}%</span>
                  </li>
                {/each}
              </ul>
            </div>
            <div>
              <div class="lbl">Other deductions</div>
              <ul class="bandlist small">
                <li>
                  <span class="faint">ACC earner levy</span>
                  <span class="tabular">
                    {(s.acc_levy_bps / 100).toFixed(2)}% to {formatMoney(
                      s.acc_income_cap_minor,
                      "NZD"
                    )}
                  </span>
                </li>
                <li>
                  <span class="faint">Student loan</span>
                  <span class="tabular">
                    {(s.student_loan_rate_bps / 100).toFixed(0)}% over {formatMoney(
                      s.student_loan_threshold_minor,
                      "NZD"
                    )}
                  </span>
                </li>
                <li>
                  <span class="faint">Employer KiwiSaver, minimum</span>
                  <span class="tabular">{(s.kiwisaver_employer_min_bps / 100).toFixed(2)}%</span>
                </li>
                <li>
                  <span class="faint">Government KiwiSaver</span>
                  <span class="tabular">
                    {(s.kiwisaver_govt_match_bps / 100).toFixed(0)}c per $1, max {formatMoney(
                      s.kiwisaver_govt_max_minor,
                      "NZD"
                    )}
                    {#if s.kiwisaver_govt_income_cap_minor != null}
                      <span class="faint">
                        · none over {formatMoney(s.kiwisaver_govt_income_cap_minor, "NZD")}</span
                      >
                    {/if}
                  </span>
                </li>
              </ul>
            </div>
          </div>
          {#if s.source_note}
            <!-- Where the figures came from, so a future reader can check them rather than trust
                 them — which matters more here than anywhere else in the app. -->
            <p class="small faint" style="margin:8px 0 0">{s.source_note}</p>
          {/if}
        {/if}
      </section>
    {/each}
  </div>
{/if}

{#snippet bandEditor(rows: { upper: string; rate: string }[], label: string)}
  <div class="field">
    <span class="lbl">{label}</span>
    <div class="bands">
      {#each rows as r, i (i)}
        <div class="band-row">
          <input
            class="input tabular"
            placeholder={i === rows.length - 1 ? "and above" : "up to"}
            bind:value={r.upper}
          />
          <input class="input tabular narrow" bind:value={r.rate} />
          <span class="unit small faint">%</span>
          <button class="btn btn-sm btn-danger" onclick={() => rows.splice(i, 1)}>✕</button>
        </div>
      {/each}
      <button class="btn btn-sm" onclick={() => rows.push({ upper: "", rate: "" })}>+ Band</button>
    </div>
    <span class="small faint">
      Leave the top band's limit empty — income above it has to be taxed at something.
    </span>
  </div>
{/snippet}

{#snippet editor()}
  <div class="editor">
    <div class="grid-fields">
      <label class="field">
        <span class="lbl req">In effect from</span>
        <input class="input" type="date" bind:value={f.effective_from} />
      </label>
      <label class="field">
        <span class="lbl">Where these came from</span>
        <input class="input" placeholder="ird.govt.nz, read today" bind:value={f.source_note} />
      </label>
    </div>

    <div class="grid-fields">
      {@render bandEditor(f.brackets, "Income tax bands")}
      {@render bandEditor(f.esct, "ESCT bands (employer KiwiSaver tax)")}
    </div>

    <div class="grid-fields">
      <label class="field">
        <span class="lbl">ACC earner levy %</span>
        <input class="input tabular" bind:value={f.acc_levy} />
      </label>
      <label class="field">
        <span class="lbl">…up to income of</span>
        <input class="input tabular" bind:value={f.acc_cap} />
      </label>
      <label class="field">
        <span class="lbl">Student loan %</span>
        <input class="input tabular" bind:value={f.sl_rate} />
      </label>
      <label class="field">
        <span class="lbl">…over income of</span>
        <input class="input tabular" bind:value={f.sl_threshold} />
      </label>
    </div>

    <div class="grid-fields">
      <label class="field">
        <span class="lbl">Employer KiwiSaver minimum %</span>
        <input class="input tabular" bind:value={f.employer_min} />
      </label>
    </div>
    <p class="small faint" style="margin:0 0 10px">
      The least an employer may pay in — 3% until 31 March 2026, 3.5% from 1 April 2026, 4% from 1
      April 2028. It is the figure a new income stream starts at, not a floor: a
      total-remuneration package or a member under 18 can genuinely be below it, so what an
      employer actually pays stays a per-job number.
    </p>

    <div class="grid-fields">
      <label class="field">
        <span class="lbl">Government KiwiSaver, cents per $1</span>
        <input class="input tabular" bind:value={f.govt_match} />
      </label>
      <label class="field">
        <span class="lbl">…up to a year</span>
        <input class="input tabular" bind:value={f.govt_max} />
      </label>
      <label class="field">
        <span class="lbl">…and none above income of</span>
        <input class="input tabular" placeholder="no limit" bind:value={f.govt_cap} />
      </label>
    </div>
    <p class="small faint" style="margin:0 0 10px">
      The government matches only what you put in yourself — an employer's contributions do not
      count toward it.
    </p>

    <div class="row" style="justify-content:flex-end;gap:8px">
      <button class="btn" onclick={() => ((editing = null), (adding = false))}>Cancel</button>
      <button class="btn btn-primary" onclick={save}>Save</button>
    </div>
  </div>
{/snippet}

<style>
  .cards {
    gap: 16px;
  }
  .summary {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 16px;
  }
  .lbl {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
  }
  .lbl.req::after {
    content: " *";
    color: var(--negative);
  }
  .bandlist {
    list-style: none;
    margin: 6px 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .bandlist li {
    display: flex;
    justify-content: space-between;
    gap: 12px;
  }
  .editor {
    padding: 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .grid-fields {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 10px;
    margin-bottom: 10px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }
  .bands {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin-top: 4px;
  }
  .band-row {
    display: grid;
    grid-template-columns: 1fr 70px auto auto;
    gap: 6px;
    align-items: center;
  }
  .confirm {
    border-color: color-mix(in srgb, var(--negative) 40%, var(--border));
    background: color-mix(in srgb, var(--negative) 6%, transparent);
  }
  .unit {
    white-space: nowrap;
  }
</style>
