<script lang="ts">
  /**
   * Opt in a property to a third-party automated valuation estimate, refreshed monthly.
   *
   * The flow is deliberately two-step — **look, then decide** — rather than one "turn this on"
   * switch. An address is personal data, so the panel asks the upstream once, shows exactly what
   * it matched (the upstream's *own* spelling of the address, which is how you tell "my house"
   * from "the one next door"), and only then offers to subscribe. Nothing is stored, and nothing
   * is polled, until that confirmation.
   *
   * The estimate is one model's guess, not a valuation, so it is labelled as such and lands in
   * the valuation series under its own `estimate` source — beside, never on top of, a figure
   * somebody typed.
   */
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "./api";

  let { accountId, onchange }: { accountId: number; onchange?: () => void } = $props();

  type Preview = Schemas["EstimatePreview"];
  type HousePricerLink = Schemas["HousePricerLink"];

  /** The subscription as stored on the account, or `null` when this property has not opted in. */
  let link = $state<HousePricerLink | null>(null);
  /** Which source is configured and what it covers — so no city is hardcoded here. */
  let source = $state<Schemas["EstimateCoverage"] | null>(null);
  /** A match found but not yet confirmed. The whole point of the pre-flight. */
  let found = $state<Preview | null>(null);
  let busy = $state<null | "checking" | "saving" | "removing">(null);
  let error = $state<string | null>(null);
  /** Set when the upstream had no match, so the miss can be explained rather than just shown. */
  let missed = $state(false);
  /** Shown only when asked for, or when the stored address didn't match. */
  let overriding = $state(false);
  let query = $state("");

  async function load() {
    const [acc, src] = await Promise.all([
      api.GET("/api/accounts/{id}", { params: { path: { id: accountId } } }),
      api.GET("/api/property-estimate-source"),
    ]);
    const metadata = acc.data?.metadata;
    link =
      metadata && metadata.profile === "property" ? (metadata.house_pricer ?? null) : null;
    source = src.data ?? null;
  }
  onMount(load);

  /** The error body's `message`, when the API sent one worth reading. */
  function messageOf(err: unknown, fallback: string): string {
    return err && typeof err === "object" && typeof (err as { message?: unknown }).message === "string"
      ? (err as { message: string }).message
      : fallback;
  }

  /** Ask the upstream, store nothing. */
  async function check() {
    busy = "checking";
    error = null;
    missed = false;
    found = null;
    const trimmed = query.trim();
    const { data, error: err, response } = await api.GET(
      "/api/accounts/{id}/property-estimate/preview",
      { params: { path: { id: accountId }, query: trimmed ? { q: trimmed } : {} } }
    );
    busy = null;
    if (data) {
      found = data;
      return;
    }
    // 404 is the ordinary answer for an address the source doesn't cover, so it gets the
    // coverage note rather than an error banner. Anything else is a fault worth reporting.
    if (response.status === 404) {
      missed = true;
      overriding = true;
      return;
    }
    error = messageOf(err, "Could not reach the estimate service. Try again later.");
  }

  /** Confirm the match shown: subscribe, and record the first estimate now. */
  async function subscribe() {
    if (!found) return;
    busy = "saving";
    error = null;
    // The query that produced the match, so the confirm re-runs the same lookup the person saw.
    const { error: err } = await api.POST("/api/accounts/{id}/property-estimate", {
      params: { path: { id: accountId }, query: { q: found.query } },
    });
    busy = null;
    if (err) {
      error = messageOf(err, "Could not turn on monthly updates.");
      return;
    }
    found = null;
    overriding = false;
    query = "";
    await load();
    onchange?.();
  }

  async function unsubscribe() {
    busy = "removing";
    error = null;
    const { error: err } = await api.DELETE("/api/accounts/{id}/property-estimate", {
      params: { path: { id: accountId } },
    });
    busy = null;
    if (err) {
      error = messageOf(err, "Could not turn off monthly updates.");
      return;
    }
    await load();
    onchange?.();
  }

  const sourceName = $derived(
    source?.source === "house_pricer" ? "House Pricer" : (source?.source ?? "the estimate service")
  );
</script>

<div class="estimate">
  <div class="row spread">
    <div class="small muted">Automated estimate</div>
    {#if link}<span class="badge on">Monthly</span>{/if}
  </div>

  {#if error}<div class="error-banner small" style="margin-top:8px">{error}</div>{/if}

  {#if link}
    <!-- Subscribed. The matched address is kept on screen because it is the thing that could
         silently become wrong: the lookup is a fuzzy address match, and the monthly poll refuses
         to record anything if it ever starts resolving to a different property. -->
    <div class="small" style="margin-top:6px">
      Matched <strong>{link.matched_address}</strong>
    </div>
    <div class="small faint">
      {sourceName} re-checks this about once a month and records the estimate against this
      property. It is a model's guess, not a valuation.
    </div>
    <div class="row" style="gap:8px;margin-top:10px">
      <button class="btn btn-sm" onclick={check} disabled={busy !== null}>
        {busy === "checking" ? "Checking…" : "Check now"}
      </button>
      <button class="btn btn-sm btn-danger" onclick={unsubscribe} disabled={busy !== null}>
        {busy === "removing" ? "Turning off…" : "Turn off"}
      </button>
    </div>
  {:else}
    <div class="small faint" style="margin-top:6px">
      Look this property up with {sourceName} and, if it matches, record an estimate of its value
      each month. Nothing is sent until you press the button.
      {#if source}<br />Covers {source.coverage}.{/if}
    </div>
    {#if !found}
      <div class="row" style="gap:8px;margin-top:10px">
        <button class="btn btn-sm btn-primary" onclick={check} disabled={busy !== null}>
          {busy === "checking" ? "Checking…" : "Check for an estimate"}
        </button>
        {#if !overriding}
          <button class="btn btn-sm" onclick={() => (overriding = true)} disabled={busy !== null}>
            Use a different address
          </button>
        {/if}
      </div>
    {/if}
  {/if}

  {#if missed}
    <div class="small" style="margin-top:8px">
      No match for that address.
      {#if source}{sourceName} only covers {source.coverage}.{/if}
      Try the street and suburb as they would appear on a listing.
    </div>
  {/if}

  {#if overriding && !found}
    <!-- The escape hatch: the source normalises to "<street>, <suburb>", which is not always
         how the address was typed into the form above. -->
    <div class="row" style="gap:8px;margin-top:8px">
      <input
        class="input"
        style="flex:1"
        placeholder="123 Kowhai Street, Riccarton"
        bind:value={query}
        onkeydown={(e) => e.key === "Enter" && check()}
      />
      <button class="btn btn-sm" onclick={check} disabled={busy !== null || query.trim() === ""}>
        Check
      </button>
    </div>
  {/if}

  {#if found}
    <!-- The decision step. Everything needed to answer "is this my property?" is here: the
         address the source matched, what it thinks the place is worth, and which of its models
         said so. -->
    <div class="match">
      <div class="small muted">Matched</div>
      <div><strong>{found.matched_address}</strong></div>
      <div class="value tabular">{formatMoney(found.value_minor, found.currency_code)}</div>
      <div class="small faint">{found.model_note} · an estimate, not a valuation</div>
      <div class="row" style="gap:8px;margin-top:10px">
        <button class="btn btn-sm btn-primary" onclick={subscribe} disabled={busy !== null}>
          {busy === "saving"
            ? "Turning on…"
            : link
              ? "Update and keep monthly updates"
              : "Yes — record this monthly"}
        </button>
        <button
          class="btn btn-sm"
          onclick={() => {
            found = null;
            missed = false;
          }}
          disabled={busy !== null}
        >
          Not my property
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .estimate {
    border-top: 1px solid var(--border);
    margin-top: 12px;
    padding-top: 10px;
  }
  .match {
    margin-top: 10px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .match .value {
    font-size: 20px;
    margin-top: 2px;
  }
  .badge.on {
    background: var(--surface-2);
    color: var(--muted);
  }
</style>
