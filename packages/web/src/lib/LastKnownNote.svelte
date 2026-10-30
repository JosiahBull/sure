<script lang="ts">
  /**
   * The line a view shows when its refresh failed while it was drawing a kept answer (see
   * `lastKnown.ts`).
   *
   * The figures stay, still dimmed, and this says how old they are — a number from this morning
   * is worth having offline, as long as it does not pass for this minute's. While a refresh is
   * merely in flight it renders nothing: the dimming is the whole signal then, and a line saying
   * "refreshing" would narrate it.
   */
  let { at }: { at: number } = $props();

  /** "9:41 am" for today, "28 Oct, 9:41 am" for anything older. */
  const when = $derived.by(() => {
    const then = new Date(at);
    const today = new Date().toDateString() === then.toDateString();
    return then.toLocaleString("en-NZ", {
      ...(today ? {} : { day: "numeric", month: "short" }),
      hour: "numeric",
      minute: "2-digit",
    });
  });
</script>

<p class="last-known small">Couldn't refresh — these figures are from {when}.</p>

<style>
  .last-known {
    margin: 0 0 12px;
    color: var(--text-muted);
  }
</style>
