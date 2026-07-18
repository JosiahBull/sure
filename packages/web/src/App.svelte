<script lang="ts">
  import { router } from "./lib/router.svelte";
  import { filters, RANGES } from "./lib/state.svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import Transactions from "./pages/Transactions.svelte";
  import Accounts from "./pages/Accounts.svelte";
  import Rules from "./pages/Rules.svelte";
  import Settings from "./pages/Settings.svelte";

  const NAV = [
    { path: "/", label: "Overview" },
    { path: "/transactions", label: "Transactions" },
    { path: "/accounts", label: "Accounts" },
    { path: "/rules", label: "Rules" },
    { path: "/settings", label: "Settings" },
  ];

  const activePath = $derived(router.path.split("?")[0]);
  const Page = $derived.by(() => {
    switch (activePath) {
      case "/transactions":
        return Transactions;
      case "/accounts":
        return Accounts;
      case "/rules":
        return Rules;
      case "/settings":
        return Settings;
      default:
        return Dashboard;
    }
  });
  // Shared header filters apply wherever they actually affect the data shown — Overview's
  // charts and Transactions' list. Accounts/Rules/Settings have no time-range concept, so
  // showing them there would just be confusing, inert chrome.
  const showFilters = $derived(activePath === "/" || activePath === "/transactions");
</script>

<div class="app">
  <header class="topbar">
    <div class="container row spread bar">
      <a href="#/" class="brand">
        <img src="/favicon.svg" alt="" width="26" height="26" />
        <strong>Sure</strong>
      </a>
      {#if showFilters}
        <div class="row" style="gap:10px">
          {#if filters.custom}
            <button class="btn zoom-out" onclick={() => (filters.custom = null)}
                    title="Clear the zoomed range and return to the selected preset">
              ⤢ Reset zoom
            </button>
          {/if}
          <select class="select" style="width:auto" bind:value={filters.range}
                  onchange={() => (filters.custom = null)} aria-label="Time range">
            {#each RANGES as r}
              <option value={r.key}>{r.label}</option>
            {/each}
          </select>
          <label class="switch" title="Include one-off transactions">
            <input type="checkbox" bind:checked={filters.includeOneOff} />
            <span class="track"></span>
            <span>One-off</span>
          </label>
        </div>
      {/if}
    </div>
    <nav class="container nav">
      {#each NAV as n}
        <a href={"#" + n.path} class="navlink" class:active={activePath === n.path}>{n.label}</a>
      {/each}
    </nav>
  </header>

  <main class="container" style="padding-top:20px">
    {#key activePath}
      <Page />
    {/key}
  </main>
</div>

<style>
  .topbar {
    position: sticky;
    top: 0;
    z-index: 20;
    background: var(--topbar-bg);
    backdrop-filter: blur(12px);
    border-bottom: 1px solid var(--border);
    padding-top: env(safe-area-inset-top);
  }
  .bar {
    padding-top: 14px;
    padding-bottom: 12px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .brand strong {
    font-size: 17px;
  }
  .nav {
    display: flex;
    gap: 4px;
    overflow-x: auto;
    padding-bottom: 8px;
    scrollbar-width: none;
  }
  .nav::-webkit-scrollbar {
    display: none;
  }
  .navlink {
    padding: 7px 13px;
    border-radius: 999px;
    font-size: 14px;
    color: var(--text-muted);
    white-space: nowrap;
    font-weight: 550;
  }
  .navlink.active {
    background: var(--surface-2);
    color: var(--text);
  }
  .navlink:hover {
    color: var(--text);
  }
  .zoom-out {
    padding: 6px 11px;
    font-size: 13px;
    white-space: nowrap;
  }
</style>
