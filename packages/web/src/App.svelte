<script lang="ts">
  import { router } from "./lib/router.svelte";
  import { filters, RANGES } from "./lib/state.svelte";
  import { people, ensureLoaded as ensurePeopleLoaded, ownershipOptions } from "./lib/people.svelte";
  import Icon from "./lib/Icon.svelte";
  import AccountPanel from "./lib/AccountPanel.svelte";
  import SettingsNav from "./lib/SettingsNav.svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import Forecast from "./pages/Forecast.svelte";
  import Transactions from "./pages/Transactions.svelte";
  import Accounts from "./pages/Accounts.svelte";
  import Household from "./pages/Household.svelte";
  import Rules from "./pages/Rules.svelte";
  import Categories from "./pages/Categories.svelte";
  import Merchants from "./pages/Merchants.svelte";
  import Providers from "./pages/Providers.svelte";
  import Preferences from "./pages/Preferences.svelte";
  import Appearance from "./pages/Appearance.svelte";
  import ScheduledAdjustments from "./pages/ScheduledAdjustments.svelte";

  // The icon rail only surfaces the data-driven views; every management/config page
  // (accounts, rules, categories, merchants, providers, preferences...) lives under Settings,
  // reached via the gear icon — matching the reference app's actual IA.
  const NAV = [
    { path: "/", label: "Dashboard", icon: "pie-chart" as const },
    { path: "/transactions", label: "Transactions", icon: "credit-card" as const },
    { path: "/forecast", label: "Forecast", icon: "trending-up" as const },
    { path: "/settings/accounts", label: "Settings", icon: "settings" as const },
  ];

  const activePath = $derived(router.path.split("?")[0]);
  const inSettings = $derived(activePath.startsWith("/settings/"));
  const railActivePath = $derived(inSettings ? "/settings/accounts" : activePath);

  // Breadcrumb trail: top-level pages are "Home > {page}"; settings pages are
  // "Home > Settings > {subpage}" — labels mirror SettingsNav's groups.
  const SETTINGS_LABELS: Record<string, string> = {
    "/settings/accounts": "Accounts",
    "/settings/household": "Household",
    "/settings/providers": "Bank sync",
    "/settings/preferences": "Preferences",
    "/settings/appearance": "Appearance",
    "/settings/scheduled": "Scheduled adjustments",
    "/settings/categories": "Categories",
    "/settings/rules": "Rules",
    "/settings/merchants": "Merchants",
  };
  const crumbs = $derived.by(() => {
    if (inSettings) return ["Settings", SETTINGS_LABELS[activePath] ?? ""];
    return [NAV.find((n) => n.path === activePath)?.label ?? ""];
  });

  const Page = $derived.by(() => {
    switch (activePath) {
      case "/transactions":
        return Transactions;
      case "/forecast":
        return Forecast;
      case "/settings/accounts":
        return Accounts;
      case "/settings/household":
        return Household;
      case "/settings/rules":
        return Rules;
      case "/settings/categories":
        return Categories;
      case "/settings/merchants":
        return Merchants;
      case "/settings/providers":
        return Providers;
      case "/settings/preferences":
        return Preferences;
      case "/settings/appearance":
        return Appearance;
      case "/settings/scheduled":
        return ScheduledAdjustments;
      default:
        return Dashboard;
    }
  });
  // Shared header filters apply wherever they actually affect the data shown — Overview's
  // charts and Transactions' list. Everything under Settings has no time-range concept, so
  // showing them there would just be confusing, inert chrome.
  const showFilters = $derived(activePath === "/" || activePath === "/transactions");

  let panelCollapsed = $state(false);

  // The household drives the "whose money" filter; loaded once for the whole shell.
  ensurePeopleLoaded();
</script>

<div class="shell" class:panel-collapsed={panelCollapsed}>
  <nav class="rail">
    <a href="#/" class="rail-logo">
      <img src="/favicon.svg" alt="Sure" width="26" height="26" />
    </a>
    <ul class="rail-nav">
      {#each NAV as n}
        <li>
          <a
            href={"#" + n.path}
            class="rail-item"
            class:active={railActivePath === n.path}
            title={n.label}
          >
            <span class="rail-icon"><Icon name={n.icon} /></span>
            <span class="rail-label">{n.label}</span>
          </a>
        </li>
      {/each}
    </ul>
  </nav>

  <aside class="panel-col">
    {#if inSettings}
      <SettingsNav />
    {:else}
      <AccountPanel />
    {/if}
  </aside>

  <div class="main-col">
    <div class="subbar">
      <div class="subbar-inner row spread">
        <div class="row" style="gap:10px">
          <button
            type="button"
            class="btn btn-sm icon-btn"
            onclick={() => (panelCollapsed = !panelCollapsed)}
            title={panelCollapsed ? "Show accounts panel" : "Hide accounts panel"}
            aria-label="Toggle accounts panel"
          >
            <Icon name="panel-left" size={16} />
          </button>
          <nav class="breadcrumb row" style="gap:6px" aria-label="Breadcrumb">
            <a href="#/" class="crumb-link">Home</a>
            {#each crumbs as c, i}
              <Icon name="chevron-right" size={13} />
              {#if i === crumbs.length - 1}
                <span class="crumb-current">{c}</span>
              {:else}
                <span class="crumb-link">{c}</span>
              {/if}
            {/each}
          </nav>
        </div>
        {#if showFilters}
          <div class="row" style="gap:10px">
            {#if filters.custom}
              <button
                class="btn zoom-out"
                onclick={() => (filters.custom = null)}
                title="Clear the zoomed range and return to the selected preset"
              >
                ⤢ Reset zoom
              </button>
            {/if}
            <select
              class="select"
              style="width:auto"
              bind:value={filters.range}
              onchange={() => (filters.custom = null)}
              aria-label="Time range"
            >
              {#each RANGES as r}
                <option value={r.key}>{r.label}</option>
              {/each}
            </select>
            {#if people.list.length > 1}
              <select
                class="select"
                style="width:auto"
                bind:value={filters.attributedTo}
                aria-label="Whose money"
              >
                <option value="">Whole household</option>
                {#each ownershipOptions() as o (o.key)}<option value={o.key}>{o.label}</option>{/each}
              </select>
            {/if}
            <label class="switch" title="Include one-off transactions">
              <input type="checkbox" bind:checked={filters.includeOneOff} />
              <span class="track"></span>
              <span>One-off</span>
            </label>
          </div>
        {/if}
      </div>
    </div>

    <main class="container" style="padding-top:20px">
      {#key activePath}
        <Page />
      {/key}
    </main>
  </div>
</div>

<style>
  .shell {
    display: flex;
    align-items: stretch;
    min-height: 100vh;
    min-height: 100dvh;
  }
  @media (max-width: 720px) {
    .panel-col {
      width: 0 !important;
      border-right-width: 0 !important;
    }
  }

  .rail {
    flex: 0 0 var(--rail-w);
    position: sticky;
    top: 0;
    height: 100dvh;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 18px;
    padding: 16px 0;
    background: var(--surface);
    border-right: 1px solid var(--border);
  }
  .rail-logo {
    display: flex;
  }
  .rail-nav {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 100%;
  }
  .rail-item {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    padding: 8px 4px;
    margin: 0 8px;
    border-radius: var(--r);
    color: var(--text-muted);
  }
  .rail-item:hover {
    background: var(--hover);
    color: var(--text);
  }
  .rail-item.active {
    background: var(--surface-2);
    color: var(--text);
  }
  .rail-item.active::before {
    content: "";
    position: absolute;
    left: -8px;
    top: 50%;
    transform: translateY(-50%);
    width: 3px;
    height: 18px;
    border-radius: 0 3px 3px 0;
    background: var(--accent);
  }
  .rail-icon {
    display: flex;
  }
  .rail-label {
    font-size: 10px;
    font-weight: 550;
  }

  .panel-col {
    flex: 0 0 auto;
    width: var(--panel-w);
    position: sticky;
    top: 0;
    height: 100dvh;
    overflow: hidden;
    background: var(--surface);
    border-right: 1px solid var(--border);
    transition: width 0.3s cubic-bezier(0.4, 0, 0.2, 1),
      border-color 0.3s cubic-bezier(0.4, 0, 0.2, 1);
  }
  .shell.panel-collapsed .panel-col {
    width: 0;
    border-right-color: transparent;
  }
  .panel-col > :global(*) {
    height: 100%;
    overflow-y: auto;
    padding: 16px;
    width: var(--panel-w);
    box-sizing: border-box;
  }

  .main-col {
    flex: 1 1 auto;
    min-width: 0;
  }
  /* Full-bleed bar (background spans the whole main column), but its content is capped and
     centred at the same --maxw as .container below, so the breadcrumb/filters line up with
     the page content instead of hugging the far edges on wide viewports. */
  .subbar {
    position: sticky;
    top: 0;
    z-index: 20;
    background: var(--topbar-bg);
    backdrop-filter: blur(12px);
    border-bottom: 1px solid var(--border);
  }
  .subbar-inner {
    /* Match .container: full-bleed with the same fluid side padding so the breadcrumb/filters
       line up with the page content below. */
    max-width: none;
    margin: 0 auto;
    padding: 10px clamp(16px, 2.8vw, 40px);
  }
  .icon-btn {
    padding: 6px 8px;
  }
  .zoom-out {
    padding: 6px 11px;
    font-size: 13px;
    white-space: nowrap;
  }
  .breadcrumb {
    font-size: 13.5px;
    color: var(--text-faint);
  }
  .crumb-link {
    color: var(--text-muted);
  }
  a.crumb-link:hover {
    color: var(--text);
  }
  .crumb-current {
    color: var(--text);
    font-weight: 600;
  }
</style>
