<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { api, type Schemas } from "./api";
  import type { PanelTab } from "./balanceGroups";
  import { kindStyle } from "./accountMeta";
  import { navigate } from "./router.svelte";
  import AccountForm from "./AccountForm.svelte";
  import Icon from "./Icon.svelte";

  let {
    initialTab = "all",
    onclose,
    oncreated,
  }: {
    initialTab?: PanelTab;
    onclose: () => void;
    oncreated: () => void;
  } = $props();

  type Kind = Schemas["AccountKind"];
  type Row = { kind: Kind; label: string };

  // The type menu, mirroring the reference app's picker: a short list of the kinds people
  // actually reach for, with the finer-grained ones behind "More types". Labels are the
  // picker's own plain-English vocabulary (the reference's accountable names) rather than
  // the raw kind labels — "Investment" is a brokerage account, "Property" real estate.
  const COMMON_ASSETS: Row[] = [
    { kind: "bank", label: "Bank account" },
    { kind: "savings", label: "Savings" },
    { kind: "brokerage", label: "Investment" },
    { kind: "crypto", label: "Crypto" },
    { kind: "real_estate", label: "Property" },
    { kind: "vehicle", label: "Vehicle" },
    { kind: "asset", label: "Other asset" },
  ];
  const MORE_ASSETS: Row[] = [
    { kind: "cash", label: "Physical cash" },
    { kind: "shares_nz", label: "Shares (NZ)" },
    { kind: "shares_us", label: "Shares (US)" },
    { kind: "shares_private", label: "Shares (private)" },
  ];
  const COMMON_DEBTS: Row[] = [
    { kind: "credit_card", label: "Credit card" },
    { kind: "mortgage", label: "Mortgage" },
    { kind: "loan", label: "Loan" },
    { kind: "liability", label: "Other liability" },
  ];
  const MORE_DEBTS: Row[] = [
    { kind: "revolving_credit", label: "Revolving credit" },
    { kind: "student_loan", label: "Student loan" },
  ];

  let currencies = $state<Schemas["Currency"][]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let chosen = $state<Row | null>(null);
  let showMore = $state(false);
  let listEl = $state<HTMLElement | null>(null);
  let modalEl = $state<HTMLElement | null>(null);

  onMount(async () => {
    const [c, a] = await Promise.all([api.GET("/api/currencies", {}), api.GET("/api/accounts", {})]);
    currencies = c.data ?? [];
    accounts = a.data ?? [];
  });

  // Only the tab's own side is offered (the reference passes the same asset/liability
  // classification through to its picker); the "all" tab shows both, headed.
  const sections = untrack(() => {
    const assets = { heading: "Assets", common: COMMON_ASSETS, more: MORE_ASSETS };
    const debts = { heading: "Debts", common: COMMON_DEBTS, more: MORE_DEBTS };
    if (initialTab === "assets") return [assets];
    if (initialTab === "debts") return [debts];
    return [assets, debts];
  });
  const hasMore = sections.some((s) => s.more.length > 0);

  /** Keyboard navigation over the menu rows, like the reference's list-keyboard-navigation. */
  function moveFocus(delta: number) {
    const items = Array.from(listEl?.querySelectorAll<HTMLElement>("[data-nav]") ?? []);
    if (items.length === 0) return;
    const current = items.indexOf(document.activeElement as HTMLElement);
    // No row focused yet: ↓ starts at the top, ↑ at the bottom.
    const next = current === -1 ? (delta > 0 ? 0 : items.length - 1) : (current + delta + items.length) % items.length;
    items[next].focus();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      onclose();
      return;
    }
    if (chosen) return; // typing in the form — leave the arrow keys alone
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      moveFocus(e.key === "ArrowDown" ? 1 : -1);
    }
  }

  function connectProvider() {
    onclose();
    navigate("/settings/providers");
  }

  // The sidebar (an animated/filtered ancestor) is a containing block for `position: fixed`,
  // which would trap the overlay inside it — reparent to <body> so it covers the viewport.
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  // Focus the dialog itself (not a row) so the arrow keys work immediately without painting a
  // focus ring on a row the user never picked.
  $effect(() => {
    if (!chosen) modalEl?.focus();
  });
</script>

{#snippet filledIcon(kind: Kind, size: number)}
  {@const style = kindStyle(kind)}
  <span class="filled" style="--c:{style.color};--s:{size}px">
    <Icon name={style.icon} size={Math.round(size * 0.56)} />
  </span>
{/snippet}

<svelte:window onkeydown={onKey} />

<div
  class="overlay"
  use:portal
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) onclose();
  }}
>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label={chosen ? `New ${chosen.label.toLowerCase()}` : "What would you like to add?"}
    tabindex="-1"
    bind:this={modalEl}
  >
    <header class="modal-head">
      <div class="head-title">
        {#if chosen}
          <button class="icon-btn" onclick={() => (chosen = null)} aria-label="Back to account types">
            <Icon name="arrow-left" size={18} />
          </button>
          {@render filledIcon(chosen.kind, 28)}
          <h2>New {chosen.label.toLowerCase()}</h2>
        {:else}
          <h2>What would you like to add?</h2>
        {/if}
      </div>
      <button class="icon-btn" onclick={onclose} aria-label="Close">
        <Icon name="x" size={18} />
      </button>
    </header>

    {#if !chosen}
      <div class="menu" bind:this={listEl}>
        {#each sections as s (s.heading)}
          {#if sections.length > 1}<p class="type-label">{s.heading}</p>{/if}
          {#each showMore ? [...s.common, ...s.more] : s.common as t (t.kind)}
            <button type="button" class="type-btn" data-nav onclick={() => (chosen = t)}>
              {@render filledIcon(t.kind, 32)}
              <span class="type-name">{t.label}</span>
              <Icon name="chevron-right" size={16} />
            </button>
          {/each}
        {/each}

        {#if hasMore}
          <button
            type="button"
            class="type-btn subtle"
            data-nav
            aria-expanded={showMore}
            onclick={() => (showMore = !showMore)}
          >
            <span class="filled muted" style="--s:32px">
              <Icon name={showMore ? "chevron-up" : "more-horizontal"} size={18} />
            </span>
            <span class="type-name">{showMore ? "Fewer types" : "More types"}</span>
          </button>
        {/if}

        <div class="menu-divider"></div>
        <button type="button" class="type-btn" data-nav onclick={connectProvider}>
          <span class="filled" style="--c:#F79009;--s:32px"><Icon name="link-2" size={18} /></span>
          <span class="type-name">Connect a bank or broker</span>
          <Icon name="chevron-right" size={16} />
        </button>
      </div>

      <footer class="hints">
        <span class="hint"><span>Select</span><kbd><Icon name="corner-down-left" size={12} /></kbd></span>
        <span class="hint">
          <span>Navigate</span>
          <kbd><Icon name="arrow-up" size={12} /></kbd>
          <kbd><Icon name="arrow-down" size={12} /></kbd>
        </span>
        <span class="hint push">
          <button type="button" class="hint-btn" onclick={onclose}>Close</button>
          <kbd class="wide">ESC</kbd>
        </span>
      </footer>
    {:else}
      <div class="form-slot">
        <AccountForm
          initialKind={chosen.kind}
          {currencies}
          {accounts}
          onsave={oncreated}
          oncancel={() => (chosen = null)}
        />
      </div>
    {/if}
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: max(40px, 6vh) 16px 40px;
    overflow-y: auto;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(2px);
  }
  /* Header and keyboard legend stay put; the long "More types" list scrolls inside the body. */
  .modal {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-width: 520px;
    max-height: 100%;
    padding: 16px 8px 8px;
    border-radius: var(--r-lg);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.35);
  }
  .modal:focus {
    outline: none;
  }
  .modal-head {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 0 8px 14px;
    border-bottom: 1px solid var(--border);
  }
  .head-title {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .modal-head h2 {
    font-size: 16px;
    font-weight: 550;
  }
  .icon-btn {
    all: unset;
    flex: none;
    display: inline-flex;
    padding: 4px;
    border-radius: var(--r-sm);
    cursor: pointer;
    color: var(--text-faint);
  }
  .icon-btn:hover {
    background: var(--hover);
    color: var(--text);
  }

  /* One type per row: a tinted glyph, the name, and a chevron — the reference's picker. */
  .menu {
    display: flex;
    flex-direction: column;
    min-height: 0;
    padding: 8px 0;
    overflow-y: auto;
  }
  .type-label {
    margin: 6px 0 2px;
    padding: 0 10px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .type-btn {
    all: unset;
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 14px;
    width: 100%;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    font-size: 14px;
    cursor: pointer;
    color: var(--text);
  }
  .type-btn:hover,
  .type-btn:focus-visible {
    background: var(--hover);
  }
  .type-btn:focus-visible {
    outline: 1px solid var(--border-strong);
  }
  .type-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .type-btn > :global(svg) {
    flex: none;
    color: var(--text-faint);
  }
  .subtle .type-name {
    color: var(--text-muted);
  }
  .menu-divider {
    margin: 6px 10px;
    border-top: 1px solid var(--border);
  }

  /* Filled icon: the kind's colour at low opacity behind its glyph (the reference's
     DS::FilledIcon, which color-mixes the accountable colour into the surface). */
  .filled {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: var(--s);
    height: var(--s);
    border-radius: calc(var(--s) / 3.5);
    background: color-mix(in oklab, var(--c) 14%, transparent);
    border: 1px solid color-mix(in oklab, var(--c) 22%, transparent);
    color: var(--c);
  }
  .filled.muted {
    background: var(--surface-2);
    border-color: var(--border);
    color: var(--text-faint);
  }

  /* Keyboard legend, as in the reference — desktop only, where these keys exist. */
  .hints {
    display: none;
    flex: none;
    align-items: center;
    gap: 20px;
    padding: 12px 16px 8px;
    border-top: 1px solid var(--border);
    font-size: 13px;
    color: var(--text-muted);
  }
  .hint {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .hint.push {
    margin-left: auto;
  }
  .hint-btn {
    all: unset;
    cursor: pointer;
  }
  .hint-btn:hover {
    color: var(--text);
  }
  kbd {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border-radius: 6px;
    background: var(--surface-2);
    box-shadow: inset 0 -1px 0 0 rgba(0, 0, 0, 0.1);
    color: var(--text-muted);
  }
  kbd.wide {
    width: 32px;
    font-size: 11px;
  }
  @media (min-width: 640px) {
    .hints {
      display: flex;
    }
  }

  .form-slot {
    min-height: 0;
    padding: 4px 8px 8px;
    overflow-y: auto;
  }
  /* The embedded AccountForm is its own card; strip that chrome (and its duplicate title) so it
     reads as the modal's body rather than a card-within-a-card. */
  .form-slot :global(section.card) {
    margin: 0;
    padding: 0;
    border: none;
    background: transparent;
    box-shadow: none;
  }
  .form-slot :global(section.card > h2) {
    display: none;
  }
</style>
