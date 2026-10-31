<script lang="ts">
  // Settings › Debug Extra for Experts: what a launch cost and what the path to the server looks
  // like, for diagnosing launch latency from the phone itself. Not in SettingsNav; reached by
  // tapping the version line at the foot of the settings menu five times, or at #/settings/debug.
  import { onMount } from "svelte";
  import {
    NOT_EXPOSED,
    deviceDetails,
    deviceSection,
    fetchEcho,
    launchRequests,
    launchSection,
    parallel,
    probeSection,
    requestsSection,
    sequential,
    serverSection,
    toJson,
    toText,
    type Echo,
    type Row,
    type Sample,
  } from "../lib/connectionDebug";

  const SEQUENTIAL = 10;
  const PARALLEL = 12;

  // Read once, as the page opens: everything after this is the page's own traffic.
  const launchEntries = launchRequests(performance.now());
  const launch = launchSection();
  const requests = requestsSection(launchEntries);

  let seq = $state.raw<Sample[] | null>(null);
  let burst = $state.raw<{ samples: Sample[]; wall: number } | null>(null);
  let echo = $state.raw<Echo | string | null>(null);
  let probeError = $state<string | null>(null);
  let running = $state(false);
  let details = $state.raw<Row[]>([]);
  let insets = $state("–");
  let insetEl = $state<HTMLElement | null>(null);
  let copied = $state<"text" | "json" | null>(null);

  const probe = $derived.by(() => {
    const section = probeSection(seq, burst);
    if (probeError) section.rows.push({ k: "error", v: probeError });
    return section;
  });
  const sections = $derived([
    launch,
    requests,
    probe,
    serverSection(echo),
    deviceSection(insets, details),
  ]);

  /**
   * The sequential probe first, so its first request finds the connection as it was when the
   * page opened (or when "Run again" was pressed after the app had been in the background).
   */
  async function run() {
    if (running) return;
    running = true;
    probeError = null;
    seq = null;
    burst = null;
    echo = null;
    try {
      seq = await sequential(SEQUENTIAL);
      echo = await fetchEcho();
      burst = await parallel(PARALLEL);
    } catch (e) {
      probeError = e instanceof Error ? e.message : String(e);
    } finally {
      running = false;
    }
  }

  function measureInsets() {
    if (!insetEl) return;
    const css = getComputedStyle(insetEl);
    const px = (side: string) => parseFloat(css.getPropertyValue(`padding-${side}`)) || 0;
    insets = `t ${px("top")} · r ${px("right")} · b ${px("bottom")} · l ${px("left")}`;
  }

  onMount(() => {
    measureInsets();
    run();
    deviceDetails().then((rows) => (details = rows));
  });

  async function copy(kind: "text" | "json") {
    const text =
      kind === "text"
        ? toText(sections)
        : toJson(sections, {
            launch: launchEntries,
            probe: [...(seq ?? []), ...(burst?.samples ?? [])],
            echo,
          });
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // No async clipboard outside a secure context (the app over plain http on a LAN address).
      const area = document.createElement("textarea");
      area.value = text;
      area.style.cssText = "position:fixed;top:0;left:0;opacity:0;font-size:16px";
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    copied = kind;
    setTimeout(() => (copied = null), 2000);
  }
</script>

<h1 style="font-size:20px;margin-bottom:14px">Debug Extra for Experts</h1>

<div class="row wrap actions">
  <button type="button" class="btn btn-sm" onclick={run} disabled={running}>
    {running ? "Running…" : "Run probe again"}
  </button>
  <button type="button" class="btn btn-sm" onclick={() => copy("text")}>
    {copied === "text" ? "Copied" : "Copy text"}
  </button>
  <button type="button" class="btn btn-sm" onclick={() => copy("json")}>
    {copied === "json" ? "Copied" : "Copy JSON"}
  </button>
</div>

<div class="sections">
  {#each sections as section (section.title)}
    <section class="card dbg" data-section={section.title}>
      <h2>{section.title}</h2>
      <div class="kv">
        {#each section.rows as row}
          {#if "k" in row}
            <span class="k">{row.k}</span>
            <span class="v" class:na={row.v === NOT_EXPOSED}>{row.v}</span>
          {:else if "note" in row}
            <p class="note">{row.note}</p>
          {:else}
            <div class="tbl" style:grid-template-columns="repeat({row.table[0].length}, auto)">
              {#each row.table as cells, r}
                {#each cells as cell, c}
                  <span class:head={r === 0} class:num={c > 0}>{cell}</span>
                {/each}
              {/each}
            </div>
          {/if}
        {/each}
      </div>
    </section>
  {/each}
</div>

<div class="inset-probe" bind:this={insetEl} aria-hidden="true"></div>

<style>
  .actions {
    gap: 8px;
    margin-bottom: 12px;
  }
  .sections {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .dbg {
    padding: 10px 12px;
    border-radius: var(--r);
  }
  .dbg h2 {
    font-size: 13px;
    margin-bottom: 5px;
  }
  .kv {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    column-gap: 10px;
    font-family: var(--mono);
    font-size: 11.5px;
    line-height: 1.38;
  }
  .k {
    color: var(--text-muted);
    white-space: nowrap;
  }
  .v {
    color: var(--text);
    overflow-wrap: anywhere;
  }
  .v.na,
  .note {
    color: var(--text-muted);
  }
  .note {
    grid-column: 1 / -1;
    margin: 3px 0 0;
  }
  .tbl {
    grid-column: 1 / -1;
    display: grid;
    justify-content: start;
    column-gap: 9px;
    margin: 3px 0;
  }
  .tbl .head {
    color: var(--text-muted);
  }
  .tbl .num {
    text-align: right;
  }
  /* Reads the safe-area insets back as computed padding; never seen. */
  .inset-probe {
    position: fixed;
    top: 0;
    left: 0;
    width: 0;
    height: 0;
    visibility: hidden;
    pointer-events: none;
    padding: env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom)
      env(safe-area-inset-left);
  }
</style>
