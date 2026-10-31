<script lang="ts">
  import { navigate, router } from "./router.svelte";

  const GROUPS: { label: string; items: { path: string; label: string }[] }[] = [
    {
      label: "General",
      items: [
        { path: "/settings/accounts", label: "Accounts" },
        { path: "/settings/import", label: "Import" },
        { path: "/settings/household", label: "Household" },
        { path: "/settings/providers", label: "Bank sync" },
      { path: "/settings/preferences", label: "Preferences" },
        { path: "/settings/appearance", label: "Appearance" },
        { path: "/settings/scheduled", label: "Scheduled adjustments" },
      ],
    },
    {
      label: "Transactions",
      items: [
        { path: "/settings/categories", label: "Categories" },
        { path: "/settings/rules", label: "Rules" },
        { path: "/settings/merchants", label: "Merchants" },
      ],
    },
  ];

  const activePath = $derived(router.path.split("?")[0]);

  // Five taps on the version line open the connection page, which is not in the menu. An
  // installed app has no address bar to type #/settings/debug into.
  const TAPS = 5;
  const WITHIN_MS = 3000;
  let taps: number[] = [];
  function tapVersion() {
    const now = performance.now();
    taps = [...taps.filter((t) => now - t < WITHIN_MS), now];
    if (taps.length < TAPS) return;
    taps = [];
    navigate("/settings/debug");
  }
</script>

<nav class="settings-nav">
  <a href="#/" class="back-link">‹ Back</a>
  {#each GROUPS as g}
    <div class="group">
      <div class="group-label">{g.label}</div>
      <ul>
        {#each g.items as item}
          <li>
            <a href={"#" + item.path} class="nav-item" class:active={activePath === item.path}>
              {item.label}
            </a>
          </li>
        {/each}
      </ul>
    </div>
  {/each}
  <button type="button" class="version" onclick={tapVersion}>Sure {__APP_VERSION__}</button>
</nav>

<style>
  .settings-nav {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .back-link {
    display: inline-flex;
    align-items: center;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-muted);
    margin-bottom: 4px;
  }
  .back-link:hover {
    color: var(--text);
  }
  .group-label {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-faint);
    margin-bottom: 6px;
    padding: 0 10px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .nav-item {
    display: block;
    padding: 7px 10px;
    border-radius: var(--r-sm);
    font-size: 13.5px;
    font-weight: 550;
    color: var(--text-muted);
  }
  /* This nav is the phone's only route to nine of the eleven settings pages, so its rows are
     the last place in the app that should be hard to hit. */
  @media (pointer: coarse) {
    .nav-item {
      padding: 12px 10px;
      font-size: 15px;
    }
    .back-link {
      min-height: 44px;
      font-size: 15px;
    }
  }
  .version {
    margin-top: auto;
    align-self: flex-start;
    padding: 6px 10px;
    border: 0;
    background: none;
    font-size: 12px;
    color: var(--text-faint);
    cursor: default;
    -webkit-user-select: none;
    user-select: none;
    -webkit-touch-callout: none;
  }
  .nav-item:hover {
    background: var(--hover);
    color: var(--text);
  }
  .nav-item.active {
    background: var(--surface-2);
    color: var(--text);
  }
</style>
