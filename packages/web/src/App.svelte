<script lang="ts">
  import { untrack } from "svelte";

  import { router } from "./lib/router.svelte";
  import { filters, RANGES, syncPeriodWithUrl } from "./lib/state.svelte";
  import { ensureLoaded as ensurePeopleLoaded } from "./lib/people.svelte";
  import { ensureSettingsLoaded } from "./lib/settings.svelte";
  import Icon from "./lib/Icon.svelte";
  import AccountPanel from "./lib/AccountPanel.svelte";
  import SettingsNav from "./lib/SettingsNav.svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import Transactions from "./pages/Transactions.svelte";
  import Accounts from "./pages/Accounts.svelte";
  import Import from "./pages/Import.svelte";
  import Household from "./pages/Household.svelte";
  import Rules from "./pages/Rules.svelte";
  import Categories from "./pages/Categories.svelte";
  import Merchants from "./pages/Merchants.svelte";
  import Providers from "./pages/Providers.svelte";
  import Preferences from "./pages/Preferences.svelte";
  import TaxRates from "./pages/TaxRates.svelte";
  import Appearance from "./pages/Appearance.svelte";
  import ScheduledAdjustments from "./pages/ScheduledAdjustments.svelte";

  // The icon rail only surfaces the data-driven views; every management/config page
  // (accounts, rules, categories, merchants, providers, preferences...) lives under Settings,
  // reached via the gear icon — matching the reference app's actual IA.
  const NAV = [
    { path: "/", label: "Dashboard", icon: "pie-chart" as const },
    { path: "/transactions", label: "Transactions", icon: "credit-card" as const },
    { path: "/settings/accounts", label: "Settings", icon: "settings" as const },
  ];

  const activePath = $derived(router.path.split("?")[0]);
  const inSettings = $derived(activePath.startsWith("/settings/"));
  const railActivePath = $derived(inSettings ? "/settings/accounts" : activePath);

  // Breadcrumb trail: top-level pages are "Home > {page}"; settings pages are
  // "Home > Settings > {subpage}" — labels mirror SettingsNav's groups.
  const SETTINGS_LABELS: Record<string, string> = {
    "/settings/accounts": "Accounts",
    "/settings/import": "Import",
    "/settings/household": "Household",
    "/settings/providers": "Bank sync",
    "/settings/tax": "Tax rates",
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
      case "/settings/accounts":
        return Accounts;
      case "/settings/import":
        return Import;
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
      case "/settings/tax":
        return TaxRates;
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

  /**
   * Whether the side panel is showing.
   *
   * One flag, two presentations, because the panel is the same content either way. Wide enough
   * for all three columns (see `--bp-dock` in app.css) and it is a docked sidebar that collapses
   * to zero width; narrower and it is an overlay drawer over the content, because a docked 300px
   * panel on a 768px tablet leaves a 382px content column — narrower than the phone it was
   * meant to be wider than, and the measured worst case of the whole layout.
   *
   * It starts open, which is what a docked sidebar should do. The drawer therefore has to be
   * closed on the way *in* rather than opened on demand — the media-query effect below does
   * that on first paint and on every crossing of the breakpoint, so a phone never lands on a
   * page covered by its own menu, and a desktop never loses its sidebar to a narrow moment
   * during a window resize.
   */
  let panelOpen = $state(true);
  /** True while the viewport is wide enough to dock the panel beside the content. */
  let docked = $state(true);

  $effect(() => {
    // Mirrors `--bp-dock` in app.css, which cannot be read out of a custom property — a media
    // query takes a literal. tests/mobile.spec.ts checks this copy against that declaration.
    const mq = window.matchMedia("(min-width: 1000px)");
    const sync = () => {
      docked = mq.matches;
      // Docked: always visible. Drawer: never *becomes* visible on its own — opening it is a
      // deliberate tap, and a resize is not one.
      panelOpen = mq.matches;
    };
    sync();
    mq.addEventListener("change", sync);
    return () => mq.removeEventListener("change", sync);
  });

  /** The drawer is modal, so it closes the way every modal does. */
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && !docked && panelOpen) panelOpen = false;
  }

  // Navigating is what a drawer is for, so it closes itself once you have. Only in drawer mode:
  // a docked sidebar that vanished every time you clicked one of its links would be useless.
  $effect(() => {
    router.path;
    if (!untrack(() => docked)) panelOpen = false;
  });

  // The body must not scroll behind an open drawer — on iOS a touch-drag that starts over the
  // overlay otherwise scrolls the page underneath it.
  $effect(() => {
    const lock = !docked && panelOpen;
    document.body.classList.toggle("scroll-locked", lock);
    return () => document.body.classList.remove("scroll-locked");
  });

  /**
   * How tall the sticky sub-bar currently is, published as `--subbar-h` for `html`'s
   * `scroll-padding-top` (see app.css).
   *
   * Anything the browser scrolls into view — a `?tx=` deep link, find-in-page, tabbing to an
   * off-screen field — otherwise parks it directly under this bar, because a sticky element
   * does not take itself out of the scrollport the way `scroll-padding` accounts for. It is
   * measured rather than assumed because the bar is one row or two depending on the page and on
   * whether the filters wrapped, and worst on the narrowest screens, which are exactly the ones
   * with the least room to spare.
   */
  let subbarEl = $state<HTMLElement | null>(null);
  let subbarH = $state(0);
  $effect(() => {
    const el = subbarEl;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => (subbarH = entry.contentRect.height));
    ro.observe(el);
    subbarH = el.getBoundingClientRect().height;
    return () => ro.disconnect();
  });
  $effect(() => {
    document.documentElement.style.setProperty("--subbar-h", `${Math.round(subbarH)}px`);
  });

  const panelLabel = $derived(inSettings ? "settings menu" : "accounts panel");

  // The household drives the "whose money" filter; loaded once for the whole shell.
  ensurePeopleLoaded();
  // And the base currency decides whether a figure reads "$12.00" or "USD 12.00", which every
  // page needs before it draws one.
  ensureSettingsLoaded();
  // The selected period lives in the address bar, so a view can be linked to and survives a
  // reload. Set up here because the shell outlives every page that reads it.
  syncPeriodWithUrl();
</script>

<svelte:window onkeydown={onKeydown} />

<div class="shell" class:panel-collapsed={!panelOpen} class:drawer-mode={!docked}>
  <!-- One element, two layouts: a vertical icon rail down the left on a wide screen, a fixed
       bottom tab bar on a phone. Same links, same active state — the phone form just stops the
       80px rail eating a fifth of a 402px screen, and puts the targets where a thumb is. -->
  <nav class="rail" aria-label="Primary">
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
            aria-current={railActivePath === n.path ? "page" : undefined}
          >
            <span class="rail-icon"><Icon name={n.icon} /></span>
            <span class="rail-label">{n.label}</span>
          </a>
        </li>
      {/each}
    </ul>
  </nav>

  <!-- Only in drawer mode, and only while open: a backdrop that is always in the DOM would
       swallow clicks on a docked sidebar's own links. -->
  {#if !docked && panelOpen}
    <button
      type="button"
      class="panel-scrim"
      aria-label="Close {panelLabel}"
      onclick={() => (panelOpen = false)}
    ></button>
  {/if}

  <!-- `inert` and nothing else: it already takes the subtree out of both the tab order and the
       accessibility tree, and it has to be here because a closed drawer is still laid out (it
       slides out rather than collapsing) — left alone, its links stay focusable and a keyboard
       walks into a menu that is off the side of the screen. `aria-hidden` alongside it would be
       redundant and, on a container holding focusable children, wrong. -->
  <aside
    class="panel-col"
    aria-label={inSettings ? "Settings" : "Accounts"}
    inert={!docked && !panelOpen}
  >
    {#if inSettings}
      <SettingsNav />
    {:else}
      <AccountPanel />
    {/if}
  </aside>

  <div class="main-col">
    <div class="subbar" bind:this={subbarEl}>
      <div class="subbar-inner">
        <div class="subbar-lead">
          <button
            type="button"
            class="btn btn-sm icon-btn"
            onclick={() => (panelOpen = !panelOpen)}
            title={panelOpen ? `Hide ${panelLabel}` : `Show ${panelLabel}`}
            aria-label={panelOpen ? `Hide ${panelLabel}` : `Show ${panelLabel}`}
            aria-expanded={panelOpen}
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
          <!-- Two groups, not one row of controls. On a wide bar they sit side by side and read
               as one group anyway; on a phone the range select needs the whole width to show
               its value ("Last 12 months" truncated to "Last 12 m" is a filter you cannot
               read) while the toggle is narrow enough to ride along with the breadcrumb. -->
          <div class="subbar-selects">
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
          </div>
          <label class="switch subbar-switch" title="Include one-off transactions">
            <input type="checkbox" bind:checked={filters.includeOneOff} />
            <span class="track"></span>
            <span>One-off</span>
          </label>
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
    /* The one line that makes the page layout honest. Every card below sizes itself against
       *this column*, not against the window — and the two are not the same number: at a 768px
       viewport the docked panel leaves 382px here, narrower than the phone the phone rules were
       written for. Viewport media queries could not see that, which is why the tablet was the
       worst-laid-out width in the app (416px of horizontal overflow, measured) despite sitting
       between two that worked. See `@container main` throughout the pages. */
    container: main / inline-size;
  }

  /* ---- Drawer mode: the panel overlays the content instead of displacing it ----------------
     Below --bp-dock there is not enough width for rail + panel + a usable content column, so
     the panel stops being a column and becomes a modal drawer. It is the same markup and the
     same toggle button; only the presentation changes. */
  .shell.drawer-mode .panel-col {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    /* `100dvh`, not `bottom: 0` — see the tab bar's note below on why a fixed element's edges
       are the wrong thing to trust on a phone. Here it would run the drawer's last few links
       past the bottom of the screen, which on the settings menu is Categories/Rules/Merchants. */
    height: 100dvh;
    z-index: 60;
    width: min(320px, 86dvw);
    max-width: 86dvw;
    border-right: 1px solid var(--border);
    box-shadow: 0 0 40px rgba(0, 0, 0, 0.45);
    transform: translateX(0);
    transition: transform 0.28s cubic-bezier(0.4, 0, 0.2, 1);
    /* The drawer is the one place a phone gets the full panel, so it keeps its own safe-area
       padding — an iPhone's home indicator sits over the bottom of it. */
    padding-bottom: env(safe-area-inset-bottom);
  }
  .shell.drawer-mode.panel-collapsed .panel-col {
    /* Slid out, not zero-width: a 0px drawer would reflow its contents to nothing on every
       open, and the transition would be a squash rather than a slide. `visibility` is what
       takes it out of the tab order once the slide has finished. */
    width: min(320px, 86dvw);
    transform: translateX(-101%);
    visibility: hidden;
    transition: transform 0.28s cubic-bezier(0.4, 0, 0.2, 1), visibility 0s linear 0.28s;
    box-shadow: none;
  }
  .shell.drawer-mode .panel-col > :global(*) {
    width: 100%;
    padding: 16px 16px calc(16px + env(safe-area-inset-bottom));
  }
  .panel-scrim {
    position: fixed;
    top: 0;
    left: 0;
    /* Sized in viewport units for the same reason the drawer and the tab bar are: `inset: 0`
       would stretch it to the layout viewport, so the strip beside the drawer — the one a thumb
       reaches for to dismiss it — would partly hang off the screen. */
    width: 100dvw;
    height: 100dvh;
    z-index: 55;
    border: 0;
    padding: 0;
    background: rgba(0, 0, 0, 0.45);
    cursor: pointer;
    animation: scrim-in 0.2s ease-out;
  }
  @keyframes scrim-in {
    from {
      opacity: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .shell.drawer-mode .panel-col,
    .shell.drawer-mode.panel-collapsed .panel-col {
      transition: none;
    }
    .panel-scrim {
      animation: none;
    }
  }

  /* ---- Phone: the rail becomes a bottom tab bar -------------------------------------------
     80px of permanent left rail is a fifth of a 402px screen, and the bottom of the screen is
     where a thumb already is. Same links, same active state, laid on their side. */
  @media (max-width: 720px) {
    .shell {
      flex-direction: column;
    }
    .rail {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      /* `bottom: 0` and `right: 0` are not enough, and the reason is the whole bug this
         replaced: **a fixed element resolves its offsets against the layout viewport, which on
         a phone is bigger than the part you can see** whenever the browser's toolbars are up.
         Measured at 320x568 in Chromium's mobile emulation, whose layout viewport is 365x648:
         the tab bar was laid out at y=582..648 and 365px wide, so it sat below the fold *and*
         ran 45px past the right edge — the app's entire primary navigation, off screen, on
         every page.

         Viewport *units* do track what you can actually see — vh/dvh/svh/lvh all report 568 and
         vw/dvw/svw all report 320 in that same case — so size and place against those instead
         of against the edges. `dvh` rather than `svh` because it follows the toolbars as they
         collapse, which keeps the bar flush to the bottom rather than floating above a gap once
         they are gone. A specified `top`/`width` wins over `bottom`/`right`, so the two lines
         above remain the fallback for anything too old to know these units. */
      top: calc(100dvh - var(--tabbar-h) - env(safe-area-inset-bottom));
      width: 100dvw;
      z-index: 50;
      flex: none;
      /* Fixed, not `auto`: --tabbar-h is what .main-col's bottom padding and the bulk-action
         bar are both measured against, and a bar that sized itself to its content would be
         free to disagree with them. */
      height: calc(var(--tabbar-h) + env(safe-area-inset-bottom));
      flex-direction: row;
      align-items: stretch;
      gap: 0;
      padding: 6px 4px calc(6px + env(safe-area-inset-bottom));
      border-right: none;
      border-top: 1px solid var(--border);
      background: var(--topbar-bg);
      backdrop-filter: blur(12px);
    }
    /* The logo is a decoration up the rail and a redundant fourth tab along the bottom — the
       Dashboard tab next to it already goes home. */
    .rail-logo {
      display: none;
    }
    .rail-nav {
      flex-direction: row;
      justify-content: space-around;
    }
    .rail-nav li {
      flex: 1 1 0;
      display: flex;
    }
    .rail-item {
      flex: 1;
      gap: 3px;
      /* 48px of target including the label, which is what a tab bar needs and what the 8px
         vertical padding of the rail form did not give. */
      padding: 7px 2px;
      margin: 0 2px;
    }
    .rail-item.active {
      background: transparent;
      color: var(--text);
    }
    /* The active marker moves with the bar: a strip across the top of the tab rather than
       down its left edge. */
    .rail-item.active::before {
      left: 50%;
      top: -7px;
      transform: translateX(-50%);
      width: 28px;
      height: 3px;
      border-radius: 0 0 3px 3px;
    }
    .rail-label {
      font-size: 11px;
    }
    /* Clear the fixed bar, so the last card is not permanently under it. */
    .main-col {
      padding-bottom: calc(var(--tabbar-h) + env(safe-area-inset-bottom));
    }
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
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--gap);
  }
  .subbar-lead {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .subbar-selects {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: none;
    /* The switch sits beside this group on a wide bar; the auto margin is what keeps the two of
       them together at the right rather than spread apart by the container's space-between. */
    margin-left: auto;
  }
  .subbar-switch {
    flex: none;
  }
  /* The breadcrumb and the three range/household/one-off controls do not fit side by side at
     phone width, and none of them will shrink on their own (a `<select>` is sized by its
     longest option). They used to widen the *document* to ~712px, so the whole page scrolled
     sideways — and because the rail and this bar are sticky, scrolling the page then slid
     content underneath them, which is how a click on a column header landed on the rail.
     Wrapping them onto their own line keeps every control at full size, reachable by thumb,
     and the document exactly one viewport wide. */
  /* Two rows, arranged so nothing has to be abbreviated and the sticky bar costs no more of the
     screen than it must:

         [▤]  Home › Dashboard              ( ) One-off
         [ Last 12 months  ▾ ] [ Whole household ▾ ]

     The breadcrumb leaves ~230px spare on its row and the toggle wants ~100px, so the toggle
     rides along there and the two selects get the whole of the second row — ~180px each, which
     is what it takes to read them. Squeezing all three onto one row was tried first and turned
     "Last 12 months" into "Last 12 m". */
  @container main (max-width: 700px) {
    .subbar-inner {
      flex-wrap: wrap;
      align-items: center;
      row-gap: 8px;
      column-gap: 10px;
    }
    .subbar-lead {
      flex: 1 1 auto;
    }
    .subbar-selects {
      order: 3;
      width: 100%;
      flex: 1 1 auto;
      margin-left: 0;
      gap: 8px;
    }
    /* `auto` basis, not `0`: sized from their own content and only then sharing what is left
       over, rather than splitting the row in half whatever is in them. The halves were equal
       and the labels are not — "Month to date (MTD)" needs ~160px and "Whole household" ~120px,
       so an even split truncated the longer one while the shorter sat in spare space. Both fit
       at their natural widths here; if a long enough household name ever means they do not,
       they shrink together instead of one of them alone. */
    .subbar-selects :global(.select) {
      flex: 1 1 auto;
      min-width: 0;
      width: auto !important;
    }
  }
  @media (max-width: 720px) {
    .subbar-inner {
      padding-top: 8px;
      padding-bottom: 8px;
    }
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
  /* "Home" is a real link, and it is the only one in the breadcrumb. A 20px-tall word between
     two other 20px-tall words is not a target; the row is already 44px, so spend it. */
  @media (pointer: coarse) {
    a.crumb-link {
      display: inline-flex;
      align-items: center;
      min-height: 44px;
      /* Negative margin so the taller box does not push the bar's height up with it. */
      margin: -12px 0;
    }
  }
  .crumb-current {
    color: var(--text);
    font-weight: 600;
  }
</style>
