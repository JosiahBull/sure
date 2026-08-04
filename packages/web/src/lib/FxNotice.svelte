<script lang="ts">
  /**
   * The footnote that keeps a converted total honest.
   *
   * Two things the backend now tells us and the old UI had no way to show:
   *
   * - `unconverted` — currencies with no exchange rate to the report currency. Their money is
   *   *absent* from the total beside this notice. It used to be silently counted at 1:1, so a
   *   US$600 holding padded net worth by NZ$600 of money that didn't exist. A total that is
   *   visibly incomplete is recoverable; a confidently wrong one isn't.
   * - `ratesAsOf` — the newest date across the rates used. The poller only writes on success,
   *   so a feed that has been down for a year leaves last year's rates in place looking
   *   exactly like this morning's. Shown once it is more than a week old.
   *
   * Deliberately a single muted line rather than a banner: on a healthy database it never
   * renders at all, and when it does the number above it is still the best available answer —
   * it just isn't the whole answer.
   */
  let {
    unconverted = [],
    ratesAsOf = null,
    currency = null,
  }: {
    unconverted?: string[];
    ratesAsOf?: string | null;
    currency?: string | null;
  } = $props();

  /** A rate older than this is worth mentioning; the poller runs daily, so a week is many
   *  missed runs, not a quiet market. */
  const STALE_AFTER_DAYS = 7;

  const rateAgeDays = $derived.by(() => {
    if (!ratesAsOf) return null;
    const then = new Date(ratesAsOf + "T00:00:00Z").getTime();
    if (Number.isNaN(then)) return null;
    return Math.floor((Date.now() - then) / 86_400_000);
  });
  const stale = $derived(rateAgeDays != null && rateAgeDays > STALE_AFTER_DAYS);
  // No rates at all *and* nothing foreign to convert is the ordinary single-currency case —
  // there is nothing to warn about, so say nothing.
  const show = $derived(unconverted.length > 0 || stale);
</script>

{#if show}
  <p class="fx-notice small">
    {#if unconverted.length > 0}
      <span class="warn"
        >Excludes {unconverted.join(", ")}{currency ? ` — no exchange rate to ${currency}` : " — no exchange rate"}.</span
      >
      <!-- There is no rate-entry screen: rates arrive from the daily poller or a config
           import, so this is the accurate advice rather than pointing at a setting. -->
      Nothing has been polled or imported for it yet.
    {/if}
    {#if stale && rateAgeDays != null}
      <span class:muted={unconverted.length > 0}>
        Converted at rates from {ratesAsOf} ({rateAgeDays} days old).
      </span>
    {/if}
  </p>
{/if}

<style>
  .fx-notice {
    margin: 8px 2px 0;
    color: var(--text-muted);
    line-height: 1.45;
  }
  /* Enough colour to be noticed under a headline figure, not enough to look like a failure —
     the total is still the best number available, just not a complete one. */
  .fx-notice .warn {
    color: var(--negative);
    font-weight: 560;
  }
</style>
