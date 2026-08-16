<script lang="ts">
  /**
   * The notice for a bank connection the upstream has retired.
   *
   * A disconnected feed is silent in every way that matters. The account it fed keeps its last
   * recorded balance, so it goes on appearing in net worth, in the balance sheet and in every
   * chart at a number that has simply stopped moving — there is no gap, no zero, nothing that
   * looks wrong. The only place the app knows is the sync history, which nobody reads. So this
   * says it wherever a total built on that balance is shown.
   *
   * The same shape as `FxNotice.svelte`, and for the same reason: on a healthy household it
   * renders nothing at all, and when it does render the figures beside it are still the best
   * answer available — they just are not a current one.
   *
   * Retrying is not the fix and the wording must not imply it is. The bank connection behind the
   * account was removed, expired, or re-authorised, and a re-authorisation issues a new account
   * id — so the link Sure holds cannot be repaired, only replaced.
   */
  import type { Schemas } from "./api";

  let {
    providers,
    href = null,
  }: {
    providers: Schemas["Provider"][];
    /**
     * Where to send the reader to fix it. Omitted on the Bank sync page, which is already
     * there — a link to the page you are on reads as a broken one.
     */
    href?: string | null;
  } = $props();

  const stale = $derived(providers.filter((p) => p.last_sync?.status === "disconnected"));
  /**
   * Names are listed only for a reader who cannot see the connection list — on the Bank sync
   * page each one is a red row a few pixels below, and repeating them is noise.
   */
  const named = $derived(href ? stale.map((p) => p.name).join(", ") : null);
</script>

{#if stale.length > 0}
  <div class="stale-feed">
    <strong
      >{stale.length} connection{stale.length === 1 ? " is" : "s are"} no longer connected
      upstream{named ? `: ${named}` : ""}.</strong
    >
    {stale.length === 1 ? "Its account still shows its" : "Their accounts still show their"} last
    known balance, and nothing new will arrive until you link
    {stale.length === 1 ? "it" : "them"} again.
    {#if href}<a {href}>Bank sync →</a>{/if}
  </div>
{/if}

<style>
  .stale-feed {
    /* Owned here rather than by each consumer: a wrapper carrying the margin would leave a
       12px gap on every healthy household, since this renders nothing at all then. */
    margin-bottom: 16px;
    padding: 12px 14px;
    border: 1px solid color-mix(in srgb, var(--negative) 32%, var(--border));
    border-radius: var(--r);
    background: color-mix(in srgb, var(--negative) 6%, transparent);
    line-height: 1.45;
  }
  .stale-feed a {
    white-space: nowrap;
  }
</style>
