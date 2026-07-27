<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { api, type Schemas } from "./api";
  import type { PanelTab } from "./balanceGroups";
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

  // The type menu (the reference's account-type picker), split into the same asset/debt buckets
  // the sidebar groups by. Picking one opens the form pre-set to that kind.
  type Kind = Schemas["AccountKind"];
  const ASSET_TYPES: { kind: Kind; label: string }[] = [
    { kind: "bank", label: "Cash / Bank" },
    { kind: "savings", label: "Savings" },
    { kind: "shares_us", label: "Investment" },
    { kind: "brokerage", label: "Brokerage" },
    { kind: "real_estate", label: "Property" },
    { kind: "vehicle", label: "Vehicle" },
    { kind: "asset", label: "Other asset" },
  ];
  const DEBT_TYPES: { kind: Kind; label: string }[] = [
    { kind: "credit_card", label: "Credit card" },
    { kind: "mortgage", label: "Mortgage" },
    { kind: "loan", label: "Loan" },
    { kind: "student_loan", label: "Student loan" },
    { kind: "liability", label: "Other liability" },
  ];

  let currencies = $state<Schemas["Currency"][]>([]);
  let accounts = $state<Schemas["Account"][]>([]);
  let chosen = $state<Kind | null>(null);

  onMount(async () => {
    const [c, a] = await Promise.all([api.GET("/api/currencies", {}), api.GET("/api/accounts", {})]);
    currencies = c.data ?? [];
    accounts = a.data ?? [];
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") onclose();
  }

  // The sidebar (an animated/filtered ancestor) is a containing block for `position: fixed`,
  // which would trap the overlay inside it — reparent to <body> so it covers the viewport.
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }
  // Which buckets to show first, based on the tab the user was on when opening the modal.
  const sections = untrack(() =>
    initialTab === "debts"
      ? [
          { heading: "Debts", types: DEBT_TYPES },
          { heading: "Assets", types: ASSET_TYPES },
        ]
      : [
          { heading: "Assets", types: ASSET_TYPES },
          { heading: "Debts", types: DEBT_TYPES },
        ],
  );
</script>

<svelte:window onkeydown={onKey} />

<div
  class="overlay"
  use:portal
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) onclose();
  }}
>
  <div class="modal" role="dialog" aria-modal="true" aria-label="New account">
    <header class="modal-head">
      <div class="row" style="gap:8px;min-width:0">
        {#if chosen}
          <button class="link-btn" onclick={() => (chosen = null)} aria-label="Back to types">
            <Icon name="chevron-right" size={16} />
          </button>
        {/if}
        <h2>New account</h2>
      </div>
      <button class="icon-btn" onclick={onclose} aria-label="Close">
        <Icon name="x" size={18} />
      </button>
    </header>

    {#if !chosen}
      <p class="sub">Choose the type of account to add.</p>
      {#each sections as s (s.heading)}
        <div class="type-group">
          <p class="type-label">{s.heading}</p>
          <div class="type-grid">
            {#each s.types as t (t.kind)}
              <button type="button" class="type-btn" onclick={() => (chosen = t.kind)}>
                <span>{t.label}</span>
                <Icon name="chevron-right" size={16} />
              </button>
            {/each}
          </div>
        </div>
      {/each}
    {:else}
      <div class="form-slot">
        <AccountForm
          initialKind={chosen}
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
  .modal {
    width: 100%;
    max-width: 520px;
    padding: 20px;
    border-radius: var(--r-lg);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.35);
  }
  .modal-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
  }
  .modal-head h2 {
    font-size: 18px;
    font-weight: 600;
  }
  .sub {
    margin: 2px 0 16px;
    font-size: 14px;
    color: var(--text-muted);
  }
  .link-btn {
    all: unset;
    display: inline-flex;
    cursor: pointer;
    color: var(--text-muted);
    transform: rotate(180deg);
  }
  .link-btn:hover {
    color: var(--text);
  }
  .type-group + .type-group {
    margin-top: 16px;
  }
  .type-label {
    margin: 0 0 8px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .type-grid {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 8px;
  }
  .type-btn {
    all: unset;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 12px 14px;
    border-radius: var(--r);
    border: 1px solid var(--border);
    background: var(--surface);
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s;
  }
  .type-btn:hover {
    background: var(--hover);
    border-color: var(--border-strong);
  }
  .type-btn :global(svg) {
    color: var(--text-faint);
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
