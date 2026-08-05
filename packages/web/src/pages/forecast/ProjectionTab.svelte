<script lang="ts">
  // Where the projection lands, and how confident it is about getting there.
  import { formatMoney, type Schemas } from "../../lib/api";
  import { horizonLabel } from "../../lib/charts/forecastScale";

  let {
    result,
    checkpoints,
    currency,
  }: {
    result: Schemas["ForecastResult"] | null;
    checkpoints: number[];
    currency: string;
  } = $props();

  const checkpointMonths = $derived(
    checkpoints
      .filter((m) => m <= (result?.months.length ?? 0))
      .map((m) => ({ months: m, month: result!.months[m - 1] }))
  );

  /**
   * The worst month, by share of paths that ran out of cash.
   *
   * Reported as a single worst point rather than a series because that is the shape of the
   * question: "is there a moment this doesn't work" has one answer, and a 360-entry sparkline of
   * mostly-zeroes buries it. The month is named so it can be acted on.
   */
  const cashRisk = $derived.by(() => {
    const rates = result?.negative_cash_rate_bps ?? [];
    if (!rates.length) return null;
    let worst = 0;
    for (let i = 1; i < rates.length; i++) if (rates[i] > rates[worst]) worst = i;
    if (rates[worst] === 0) return null;
    return { bps: rates[worst], as_of: result!.months[worst]?.as_of ?? "", month: worst + 1 };
  });
</script>

<div class="grid cards">
  {#if checkpointMonths.length > 0}
    <section class="card">
      <h2>Checkpoints</h2>
      <div class="checkpoints">
        {#each checkpointMonths as c (c.months)}
          <div class="checkpoint">
            <div class="cp-label">+{horizonLabel(c.months)}</div>
            <div class="cp-value tabular">
              {formatMoney(c.month.net_worth.median_minor, currency)}
            </div>
            <div class="cp-range tabular small faint">
              {formatMoney(c.month.net_worth.p10_minor, currency)} – {formatMoney(
                c.month.net_worth.p90_minor,
                currency
              )}
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  <section class="card">
    <div class="card-title">
      <h2>How this was run</h2>
      <span class="muted small">what the simulation actually did, not what was asked of it</span>
    </div>
    {#if result}
      <div class="checkpoints">
        <div class="checkpoint">
          <div class="cp-label">Horizon</div>
          <div class="cp-value tabular">{horizonLabel(result.horizon_months)}</div>
          <div class="cp-range small faint">{result.horizon_months} monthly steps</div>
        </div>
        <div class="checkpoint">
          <div class="cp-label">Paths</div>
          <div class="cp-value tabular">{result.simulations.toLocaleString()}</div>
          <!-- Long horizons trade paths for months against a fixed budget, so a 30-year run
               does not cost thirty times a 5-year one. Saying so beats letting someone wonder
               why the bands look grainier than they asked for. -->
          <div class="cp-range small faint">
            {result.simulations < 2000 ? "reduced for this horizon" : "independent futures"}
          </div>
        </div>
        <div class="checkpoint" class:risk={cashRisk !== null}>
          <div class="cp-label">Runs out of cash</div>
          <div class="cp-value tabular">
            {cashRisk ? `${(cashRisk.bps / 100).toFixed(0)}%` : "never"}
          </div>
          <div class="cp-range small faint">
            {#if cashRisk}
              of paths, at worst — around {cashRisk.as_of.slice(0, 7)}
            {:else}
              in every simulated path
            {/if}
          </div>
        </div>
      </div>
    {:else}
      <div class="empty">Nothing projected yet.</div>
    {/if}
  </section>
</div>

<style>
  .cards {
    gap: 16px;
  }
  .checkpoints {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
    gap: 14px;
  }
  .checkpoint {
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }
  /* Only when there is something to warn about — a permanent amber tile is wallpaper. */
  .checkpoint.risk {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--border));
    background: color-mix(in srgb, var(--warn) 7%, var(--surface-2));
  }
  .cp-label {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-faint);
    margin-bottom: 4px;
  }
  .cp-value {
    font-size: 16px;
    font-weight: 640;
  }
  .cp-range {
    margin-top: 2px;
  }
</style>
