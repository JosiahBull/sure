<script lang="ts">
  import { router } from "./router.svelte";

  const GROUPS: { label: string; items: { path: string; label: string }[] }[] = [
    {
      label: "General",
      items: [
        { path: "/settings/accounts", label: "Accounts" },
        { path: "/settings/import", label: "Import" },
        { path: "/settings/household", label: "Household" },
        { path: "/settings/providers", label: "Bank sync" },
        { path: "/settings/tax", label: "Tax rates" },
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
  .nav-item:hover {
    background: var(--hover);
    color: var(--text);
  }
  .nav-item.active {
    background: var(--surface-2);
    color: var(--text);
  }
</style>
