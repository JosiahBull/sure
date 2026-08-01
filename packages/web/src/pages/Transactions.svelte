<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, formatDateLong, colorFor, type Schemas } from "../lib/api";
  import { ICONS } from "../lib/icons";
  import { RANGES, activeRange, filters, type RangeKey } from "../lib/state.svelte";
  import { router } from "../lib/router.svelte";
  import Icon from "../lib/Icon.svelte";
  import {
    people,
    ensureLoaded as ensurePeopleLoaded,
    ownershipLabel,
    ownershipColor,
    ownershipFromKey,
    ownershipOptions,
  } from "../lib/people.svelte";

  // Deep links via the hash query, read once at mount:
  //   ?tx=<id>        highlight & scroll to a transaction (from the rules audit log)
  //   ?category=<id>  filter to a category and its whole subtree (from the overview pies)
  //   ?account=<id>   filter to a single account (from the accounts list)
  //   ?type=<kind>    filter to income or outgoings (from the overview pies/Sankey)
  //   ?range=<key>    apply a preset time range
  //   ?at=<id>        resume the scroll position around a transaction (written as the list scrolls)
  const params = new URLSearchParams(router.path.split("?")[1] ?? "");
  const num = (v: string | null) => (v && Number.isFinite(Number(v)) ? Number(v) : null);
  const isRangeKey = (v: string | null): v is RangeKey => !!v && RANGES.some((r) => r.key === v);
  type TypeFilter = "" | "income" | "expense";
  const isTypeFilter = (v: string | null): v is Exclude<TypeFilter, ""> => v === "income" || v === "expense";
  const highlightId = num(params.get("tx"));
  const paramCategory = num(params.get("category"));
  const paramAccount = num(params.get("account"));
  const paramType = params.get("type");
  const paramRange = params.get("range");
  const paramAnchor = num(params.get("at"));
  // Shareable-link params (mirroring the reference's q[start_date]/q[end_date]/q[categories]/
  // per_page/page): an explicit start+end sets a custom window; page-size and page restore
  // the exact paginated slice; the search text and tab round-trip too.
  const isDate = (v: string | null): v is string => !!v && /^\d{4}-\d{2}-\d{2}$/.test(v);
  const paramStart = params.get("start");
  const paramEnd = params.get("end");
  const paramSearch = params.get("q") ?? "";
  const paramPage = num(params.get("page"));
  const paramPerPage = num(params.get("per_page"));
  const paramTab = params.get("tab");

  type Tx = Schemas["Transaction"];
  type Account = Schemas["Account"];
  type Category = Schemas["Category"];
  type Merchant = Schemas["Merchant"];

  let txns = $state<Tx[]>([]);
  let accounts = $state<Account[]>([]);
  let categories = $state<Category[]>([]);
  let merchants = $state<Merchant[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);

  let accountId = $state<number | "">(paramAccount ?? "");
  let categoryId = $state<number | "">(paramCategory ?? "");
  let typeFilter = $state<TypeFilter>(isTypeFilter(paramType) ? paramType : "");
  // Whose transactions to show — an `ownershipKey` ("person:3" / "joint"), or "" for the
  // whole household. Filtered on the server, since "effective attribution" needs the
  // account join.
  let attributedTo = $state<string>(params.get("owner") ?? "");
  let search = $state(paramSearch);

  // The list only renders in its default newest-first date order now (the reference's grouped
  // view); the sort keys stay fixed at date/desc, which keeps the day-group headers valid.
  type SortKey = "date" | "description" | "category" | "amount";
  const sortKey = $state<SortKey>("date");
  const sortDir = $state<"asc" | "desc">("desc");
  // Time range / one-off are the header's shared filters (App.svelte), not page-local —
  // deep links still get to steer them, though: an explicit `?range=` wins, and a
  // deep-linked transaction/account (which implies "show me everything relevant", not
  // just whatever slice happens to be selected) widens to "all" if no range was given.
  // Runs once per navigation here (this page remounts fresh on every route change), so
  // it behaves like the one-time default the local state used to be, without fighting
  // the user's choice on every subsequent render.
  if (isDate(paramStart) && isDate(paramEnd)) {
    // An explicit shared window wins over any preset range.
    filters.custom = { from: paramStart, to: paramEnd };
  } else if (isRangeKey(paramRange)) {
    filters.range = paramRange;
    filters.custom = null;
  } else if (highlightId != null || paramAccount != null) {
    filters.range = "all";
    filters.custom = null;
  }

  // Matches the reference's "Transactions / Upcoming" tab bar. We don't project recurring/
  // upcoming transactions yet, so that panel is an honest empty state rather than faked data.
  let activeTab = $state<"transactions" | "upcoming">(paramTab === "upcoming" ? "upcoming" : "transactions");

  let showAdd = $state(false);
  let form = $state({
    account_id: 0,
    posted_at: new Date().toISOString().slice(0, 10),
    amount: "",
    description: "",
    category_id: "" as number | "",
    merchant_id: "" as number | "",
    is_one_off: false,
  });

  const accountName = $derived(new Map(accounts.map((a) => [a.id, a.name])));
  const accountOwnership = $derived(new Map(accounts.map((a) => [a.id, a.ownership])));
  /**
   * Who a transaction belongs to: its own override, or — the usual case — its account's
   * owner. Mirrors `sure_core::effective_ownership`; resolved here rather than sent per row
   * because the page already holds every account.
   */
  function ownerOf(t: Tx): { ownership: Schemas["Ownership"]; inherited: boolean } | null {
    const account = accountOwnership.get(t.account_id);
    if (t.ownership) return { ownership: t.ownership, inherited: false };
    return account ? { ownership: account, inherited: true } : null;
  }
  const currencyOf = $derived(new Map(accounts.map((a) => [a.id, a.currency_code])));
  const categoryName = $derived(new Map(categories.map((c) => [c.id, c.name])));
  const merchantName = $derived(new Map(merchants.map((m) => [m.id, m.name])));

  function sortValue(t: Tx, key: SortKey) {
    switch (key) {
      case "date":
        return t.posted_at;
      case "description":
        return (t.description ?? "").toLowerCase();
      case "category":
        return (categoryName.get(t.category_id ?? -1) ?? "").toLowerCase();
      case "amount":
        return t.amount_minor;
    }
  }

  const childrenOf = $derived.by(() => {
    const m = new Map<number, number[]>();
    for (const c of categories) {
      if (c.parent_id != null) {
        const arr = m.get(c.parent_id);
        if (arr) arr.push(c.id);
        else m.set(c.parent_id, [c.id]);
      }
    }
    return m;
  });
  // The selected category plus all its descendants. The category breakdown rolls up to
  // top-level categories, so filtering by a parent must include its children's transactions.
  const categorySubtree = $derived.by(() => {
    if (categoryId === "") return null;
    const out = new Set<number>([categoryId]);
    const stack = [categoryId];
    while (stack.length) {
      for (const ch of childrenOf.get(stack.pop()!) ?? []) {
        if (!out.has(ch)) {
          out.add(ch);
          stack.push(ch);
        }
      }
    }
    return out;
  });

  const sortedFiltered = $derived.by(() => {
    const filtered = txns.filter((t) => {
      if (categorySubtree && !(t.category_id != null && categorySubtree.has(t.category_id))) return false;
      // Income/outgoings is the same sign-of-amount split the stat bar below uses — not a
      // separate server-side concept, just a client-side view over the same rows.
      if (typeFilter === "income" && t.amount_minor < 0) return false;
      if (typeFilter === "expense" && t.amount_minor >= 0) return false;
      if (search && !`${t.description} ${t.merchant ?? ""}`.toLowerCase().includes(search.toLowerCase()))
        return false;
      return true;
    });
    const dir = sortDir === "asc" ? 1 : -1;
    return filtered.sort((a, b) => {
      const va = sortValue(a, sortKey);
      const vb = sortValue(b, sortKey);
      if (va < vb) return -dir;
      if (va > vb) return dir;
      // Stable within a day: ascending id (import order), matching the reference's intra-day
      // ordering so grouped days read oldest-entered first.
      return a.id - b.id;
    });
  });

  // The reference's date-grouped, stat-headed layout only reads correctly while the list is
  // in its natural newest-first order; any other sort falls back to the flat, fully-sortable
  // table. Clicking a column header from the grouped view re-sorts (switching to that flat
  // table); re-selecting Date returns here — so no sort column is ever unreachable.
  const grouped = $derived(sortKey === "date" && sortDir === "desc");

  const catById = $derived(new Map(categories.map((c) => [c.id, c])));
  const txName = (t: Tx) => merchantName.get(t.merchant_id ?? -1) ?? t.merchant ?? t.description ?? "";

  // Header stats + per-day summaries are derived from the current filtered set (the same set
  // the "{n} transactions" footer counts), entirely client-side. Amounts can span currencies;
  // the stat/day totals are shown in a single representative currency (the filtered account's,
  // else the first row's) — a deliberate approximation, not a converted total.
  const statCurrency = $derived(
    accountId !== "" ? (currencyOf.get(accountId) ?? "NZD") : (sortedFiltered[0]?.currency_code ?? "NZD"),
  );
  const stats = $derived.by(() => {
    let income = 0;
    let expenses = 0;
    for (const t of sortedFiltered) {
      if (t.amount_minor >= 0) income += t.amount_minor;
      else expenses += t.amount_minor;
    }
    return { count: sortedFiltered.length, income, expenses };
  });
  // Calendar-day → { count, net } over the whole filtered set, so a day heading always shows
  // that day's full totals even when virtualisation has only mounted part of the day.
  const dayGroups = $derived.by(() => {
    const m = new Map<string, { count: number; net: number }>();
    for (const t of sortedFiltered) {
      const k = t.posted_at.slice(0, 10);
      const g = m.get(k);
      if (g) {
        g.count++;
        g.net += t.amount_minor;
      } else m.set(k, { count: 1, net: t.amount_minor });
    }
    return m;
  });

  // Numbered pagination (matching the reference app) over the already-loaded, filtered/sorted
  // set — rows per page is user-choosable, like the reference's page-size select.
  const PAGE_SIZES = [10, 20, 30, 50, 100];
  let pageSize = $state(paramPerPage && PAGE_SIZES.includes(paramPerPage) ? paramPerPage : 50);
  let page = $state(paramPage && paramPage > 0 ? paramPage : 1); // 1-indexed
  const pageCount = $derived(Math.max(1, Math.ceil(sortedFiltered.length / pageSize)));
  const paged = $derived(sortedFiltered.slice((page - 1) * pageSize, page * pageSize));
  // The reference nests each day as its own two-tier card (a subtle outer wrapper holding a
  // brighter inner card of rows) — bucket the current page's rows into contiguous same-day runs
  // to render that. Only meaningful when `grouped` (paged is already date-desc, so same-day rows
  // are always contiguous); otherwise rows render flat with no day wrapper.
  const renderGroups = $derived.by(() => {
    const out: { dateKey: string; rows: Tx[] }[] = [];
    for (const t of paged) {
      const dk = t.posted_at.slice(0, 10);
      const last = out[out.length - 1];
      if (last && last.dateKey === dk) last.rows.push(t);
      else out.push({ dateKey: dk, rows: [t] });
    }
    return out;
  });
  // Condensed page-number list — first, last, current ±1, with "…" gaps — same shape as the
  // reference's "1 2 3 4 5 … 757".
  const pageNumbers = $derived.by(() => {
    const nums = new Set([1, pageCount, page - 1, page, page + 1]);
    const sorted = [...nums].filter((n) => n >= 1 && n <= pageCount).sort((a, b) => a - b);
    const out: (number | "…")[] = [];
    let prev = 0;
    for (const n of sorted) {
      if (prev && n - prev > 1) out.push("…");
      out.push(n);
      prev = n;
    }
    return out;
  });

  // Reflect the whole filter/paging state into the hash query so the URL is a shareable link
  // (the reference keeps its filters — categories, date window, page, per_page — in the URL for
  // exactly this reason). Kept in sync on every relevant change below.
  function syncUrl() {
    const p = new URLSearchParams();
    if (categoryId !== "") p.set("category", String(categoryId));
    if (accountId !== "") p.set("account", String(accountId));
    if (typeFilter) p.set("type", typeFilter);
    if (attributedTo !== "") p.set("owner", attributedTo);
    if (filters.custom) {
      p.set("start", filters.custom.from);
      p.set("end", filters.custom.to);
    }
    if (search.trim()) p.set("q", search.trim());
    if (activeTab !== "transactions") p.set("tab", activeTab);
    if (pageSize !== 50) p.set("per_page", String(pageSize));
    if (page > 1) p.set("page", String(page));
    const anchorTx = paged[0]?.id;
    if (anchorTx != null) p.set("at", String(anchorTx));
    const base = router.path.split("?")[0];
    const qs = p.toString();
    history.replaceState(null, "", `#${base}${qs ? "?" + qs : ""}`);
  }

  // Resolve the initial page (from a highlight deep-link or a resumed `?at=`) once the data
  // it refers to has loaded — lands on whichever page contains that transaction.
  let didInitPage = false;
  $effect(() => {
    if (didInitPage || sortedFiltered.length === 0) return;
    didInitPage = true;
    const initial = highlightId ?? paramAnchor;
    if (initial != null) {
      const idx = sortedFiltered.findIndex((t) => t.id === initial);
      if (idx >= 0) page = Math.floor(idx / pageSize) + 1;
    }
  });
  // Persist the current page's leading transaction to `?at=` so a refresh resumes on the same
  // page. Guarded on didInitPage so this doesn't clobber a still-pending deep-link resolution.
  $effect(() => {
    // Depend on the full filter/paging surface so the shareable URL tracks every change.
    page;
    void [categoryId, accountId, typeFilter, attributedTo, filters.custom?.from, filters.custom?.to, search, activeTab, pageSize];
    if (didInitPage) syncUrl();
  });

  // Filters/sort/page-size changing what's in the list makes the current page meaningless —
  // jump back to page 1. (A reload with the same filters, e.g. after saving a row, is not a
  // change here, so the current page survives it.)
  let prevFilterSig: string | null = null;
  $effect(() => {
    const sig = `${accountId}|${categoryId}|${typeFilter}|${attributedTo}|${search}|${filters.includeOneOff}|${filters.range}|${filters.custom?.from}|${filters.custom?.to}|${sortKey}|${sortDir}|${pageSize}`;
    if (prevFilterSig != null && sig !== prevFilterSig) {
      // The visible set changed, so a lingering selection could act on rows the user can
      // no longer see — clear it. (A same-filter reload, e.g. after a save, isn't a change.)
      if (selected.size > 0) clearSelection();
      page = 1;
    }
    prevFilterSig = sig;
  });

  // ---- Filter panel: account/category/type tucked behind one "Filter" button + popover,
  // matching the reference's search-bar + Filter-button layout. ----
  let showFilterPanel = $state(false);
  let filterPanelEl = $state<HTMLElement | null>(null);
  let filterBtnEl = $state<HTMLElement | null>(null);
  const hasIcon = (name: string | null | undefined): name is keyof typeof ICONS =>
    !!name && name in ICONS;

  // The removable filter "chips" shown under the search bar — one per active filter, each with
  // its own clear action. Mirrors the reference's start_date/end_date/categories/… badge row.
  type Chip = { key: string; icon?: keyof typeof ICONS; label: string; clear: () => void };
  const activeChips = $derived.by<Chip[]>(() => {
    const chips: Chip[] = [];
    if (filters.custom) {
      const { from, to } = filters.custom;
      chips.push({ key: "start", icon: "calendar", label: `on or after ${from}`, clear: () => (filters.custom = null) });
      chips.push({ key: "end", icon: "calendar", label: `on or before ${to}`, clear: () => (filters.custom = null) });
    }
    if (categoryId !== "")
      chips.push({ key: "category", label: categoryName.get(categoryId) ?? "Category", clear: () => (categoryId = "") });
    if (accountId !== "")
      chips.push({ key: "account", label: accountName.get(accountId) ?? "Account", clear: () => (accountId = "") });
    if (typeFilter)
      chips.push({ key: "type", label: typeFilter === "income" ? "Income" : "Expense", clear: () => (typeFilter = "") });
    if (attributedTo !== "")
      chips.push({
        key: "owner",
        label: ownershipLabel(ownershipFromKey(attributedTo)),
        clear: () => (attributedTo = ""),
      });
    if (search.trim())
      chips.push({ key: "q", icon: "search", label: `"${search.trim()}"`, clear: () => (search = "") });
    return chips;
  });
  $effect(() => {
    if (!showFilterPanel) return;
    function onDocClick(e: MouseEvent) {
      const target = e.target as Node;
      if (filterPanelEl?.contains(target) || filterBtnEl?.contains(target)) return;
      showFilterPanel = false;
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") showFilterPanel = false;
    }
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("click", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  });

  // ---- "…" header menu: shortcuts to the categorisation pages the reference exposes here
  // (rules/categories/merchants) — no CSV import feature exists yet, so that item is omitted
  // rather than added as a dead link. ----
  let showActionMenu = $state(false);
  let actionMenuEl = $state<HTMLElement | null>(null);
  let actionBtnEl = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!showActionMenu) return;
    function onDocClick(e: MouseEvent) {
      const target = e.target as Node;
      if (actionMenuEl?.contains(target) || actionBtnEl?.contains(target)) return;
      showActionMenu = false;
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") showActionMenu = false;
    }
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("click", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  });

  async function loadRefs() {
    const [a, c, m] = await Promise.all([
      api.GET("/api/accounts", {}),
      api.GET("/api/categories", {}),
      api.GET("/api/merchants", {}),
    ]);
    accounts = a.data ?? [];
    categories = c.data ?? [];
    merchants = m.data ?? [];
    if (accounts.length && !form.account_id) form.account_id = accounts[0].id;
  }

  async function loadTx() {
    loading = true;
    error = null;
    const { from, to } = activeRange();
    const query: Record<string, unknown> = { from, to, include_one_off: filters.includeOneOff, limit: 2000 };
    if (accountId !== "") query.account_id = accountId;
    if (attributedTo !== "") {
      const o = ownershipFromKey(attributedTo);
      query.attributed_to = o.kind === "person" ? String(o.person_id) : "joint";
    }
    // Category is filtered client-side (subtree-aware) in `sortedFiltered`, not on the server.
    const { data, error: e } = await api.GET("/api/transactions", { params: { query } });
    txns = data ?? [];
    if (e) error = "Failed to load transactions.";
    loading = false;
  }

  onMount(async () => {
    await Promise.all([loadRefs(), ensurePeopleLoaded()]);
  });
  $effect(() => {
    // Category is filtered client-side, so it isn't a reload trigger.
    accountId;
    attributedTo;
    filters.includeOneOff;
    filters.range;
    filters.custom;
    loadTx();
  });

  // Once the page containing a `?tx=` deep-link has rendered, flash-scroll it into view. A
  // resumed `?at=` needs no scrolling — it already landed on the right page, near the top.
  let didScroll = false;
  $effect(() => {
    void paged.length; // re-run once the target page's rows are rendered
    if (highlightId == null || didScroll) return;
    const el = document.getElementById(`tx-${highlightId}`);
    if (!el) return;
    didScroll = true;
    requestAnimationFrame(() => el.scrollIntoView({ block: "center", behavior: "smooth" }));
  });

  async function addTx() {
    const amount = Math.round(parseFloat(form.amount) * 100);
    if (!form.account_id || isNaN(amount)) {
      error = "Account and a numeric amount are required.";
      return;
    }
    const { error: e } = await api.POST("/api/transactions", {
      body: {
        account_id: form.account_id,
        posted_at: form.posted_at,
        amount_minor: amount,
        description: form.description,
        category_id: form.category_id === "" ? null : form.category_id,
        merchant_id: form.merchant_id === "" ? null : form.merchant_id,
        is_one_off: form.is_one_off,
      },
    });
    if (e) {
      error = "Failed to add transaction.";
      return;
    }
    form.amount = "";
    form.description = "";
    showAdd = false;
    loadTx();
  }

  // Save a transaction, preserving every field except the patched ones — so editing
  // the category inline doesn't wipe the merchant, and vice versa.
  async function saveTx(t: Tx, patch: Partial<Schemas["SaveTransaction"]>) {
    await api.PUT("/api/transactions/{id}", {
      params: { path: { id: t.id } },
      body: {
        account_id: t.account_id,
        posted_at: t.posted_at,
        amount_minor: t.amount_minor,
        currency_code: t.currency_code,
        description: t.description,
        merchant: t.merchant,
        merchant_id: t.merchant_id,
        notes: t.notes,
        category_id: t.category_id,
        is_one_off: t.is_one_off,
        ...patch,
      },
    });
    loadTx();
  }

  const setCategory = (t: Tx, cat: number | "") =>
    saveTx(t, { category_id: cat === "" ? null : cat });

  // ---- Bulk selection & actions ----
  // Selected transaction ids. Reassigned (never mutated in place) on every change so
  // Svelte's reactivity picks it up — a plain Set isn't deeply reactive. Selection is by
  // id, so it survives scrolling/virtualisation and a post-save reload, and it spans the
  // whole filtered list rather than just the rendered window — which is what "select all"
  // has to mean here (only ~60 rows are ever mounted at once).
  let selected = $state<Set<number>>(new Set());
  let bulkBusy = $state(false);
  let confirmingDelete = $state(false);

  const allSelected = $derived(
    sortedFiltered.length > 0 && sortedFiltered.every((t) => selected.has(t.id)),
  );
  const someSelected = $derived(selected.size > 0 && !allSelected);

  function toggleOne(id: number, on: boolean) {
    const next = new Set(selected);
    if (on) next.add(id);
    else next.delete(id);
    selected = next;
  }
  function toggleAll() {
    selected = allSelected ? new Set() : new Set(sortedFiltered.map((t) => t.id));
  }
  function clearSelection() {
    selected = new Set();
    confirmingDelete = false;
  }

  async function runBulk(call: () => Promise<{ error?: unknown }>) {
    if (selected.size === 0) return;
    bulkBusy = true;
    error = null;
    const { error: e } = await call();
    bulkBusy = false;
    if (e) {
      error = "Bulk action failed.";
      return;
    }
    clearSelection();
    await loadTx();
  }

  const bulkPatch = (patch: Partial<Schemas["BulkUpdate"]>) =>
    runBulk(() =>
      api.POST("/api/transactions/bulk-update", { body: { ids: [...selected], ...patch } }),
    );
  const bulkDelete = () =>
    runBulk(() => api.POST("/api/transactions/bulk-delete", { body: { ids: [...selected] } }));

  // The category/merchant bulk pickers apply on change, then snap back to their placeholder
  // (the empty option) so they read as an action, not a persistent selection.
  function onBulkCategory(e: Event & { currentTarget: HTMLSelectElement }) {
    const v = e.currentTarget.value;
    e.currentTarget.value = "";
    if (v === "") return;
    bulkPatch({ category_id: v === "__clear__" ? null : Number(v) });
  }
  function onBulkMerchant(e: Event & { currentTarget: HTMLSelectElement }) {
    const v = e.currentTarget.value;
    e.currentTarget.value = "";
    if (v === "") return;
    bulkPatch({ merchant_id: v === "__clear__" ? null : Number(v) });
  }
  /**
   * Attribute the selection. `inherit` sends an explicit `null`, which the API reads as
   * "drop the override" — distinct from omitting the field, which leaves it untouched.
   * Selecting a single row and using this is also how one transaction gets overridden;
   * the row itself stays a read-only chip rather than growing a third inline picker.
   */
  function onBulkOwner(e: Event & { currentTarget: HTMLSelectElement }) {
    const v = e.currentTarget.value;
    e.currentTarget.value = "";
    if (v === "") return;
    bulkPatch({ ownership: v === "inherit" ? null : ownershipFromKey(v) });
  }
</script>

<div class="tx-page">
<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <h1 style="font-size:20px;font-weight:500">Transactions</h1>
  <div class="row" style="gap:8px">
    <div class="menu-wrap">
      <button
        type="button"
        class="btn btn-sm icon-btn"
        bind:this={actionBtnEl}
        onclick={() => (showActionMenu = !showActionMenu)}
        aria-expanded={showActionMenu}
        aria-label="More actions"
        title="More actions"
      >
        <Icon name="more-horizontal" size={16} />
      </button>
      {#if showActionMenu}
        <div class="action-menu" bind:this={actionMenuEl}>
          <a href="#/settings/rules" class="action-item">Edit rules</a>
          <a href="#/settings/categories" class="action-item">Edit categories</a>
          <a href="#/settings/merchants" class="action-item">Edit merchants</a>
        </div>
      {/if}
    </div>
    <a class="btn btn-sm import-btn" href="#/settings/rules" title="Import">
      <Icon name="download" size={16} />
      Import
    </a>
    <button class="btn btn-primary btn-sm new-tx-btn" onclick={() => (showAdd = !showAdd)}>
      {#if showAdd}Close{:else}<Icon name="plus" size={16} />New transaction{/if}
    </button>
  </div>
</div>

{#if error}<div class="error-banner" style="margin-bottom:12px">{error}</div>{/if}

{#if showAdd}
  <section class="card" style="margin-bottom:14px">
    <div class="grid" style="grid-template-columns:repeat(auto-fit,minmax(150px,1fr))">
      <label class="field">Account
        <select class="select" bind:value={form.account_id}>
          {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
        </select>
      </label>
      <label class="field">Date
        <input class="input" type="date" bind:value={form.posted_at} />
      </label>
      <label class="field">Amount (− for spend)
        <input class="input tabular" placeholder="-12.50" bind:value={form.amount} />
      </label>
      <label class="field">Description
        <input class="input" bind:value={form.description} />
      </label>
      <label class="field">Category
        <select class="select" bind:value={form.category_id}>
          <option value="">— none —</option>
          {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
        </select>
      </label>
      <label class="field">Merchant
        <select class="select" bind:value={form.merchant_id}>
          <option value="">— none —</option>
          {#each merchants as m}<option value={m.id}>{m.name}</option>{/each}
        </select>
      </label>
    </div>
    <div class="row spread" style="margin-top:12px">
      <label class="switch">
        <input type="checkbox" bind:checked={form.is_one_off} /><span class="track"></span>
        <span>One-off</span>
      </label>
      <button class="btn btn-primary" onclick={addTx}>Save transaction</button>
    </div>
  </section>
{/if}

{#if !(loading && txns.length === 0)}
  <section class="card statcard">
    <div class="statbar">
      <div class="stat">
        <span class="label">Total transactions</span>
        <span class="value tabular">{stats.count}</span>
      </div>
      <div class="stat">
        <span class="label">Income</span>
        <span class="value tabular">{formatMoney(stats.income, statCurrency)}</span>
      </div>
      <div class="stat">
        <span class="label">Expenses</span>
        <span class="value tabular">{formatMoney(Math.abs(stats.expenses), statCurrency)}</span>
      </div>
    </div>
  </section>
{/if}

<div class="tabs-nav">
  <button class="tab-btn" class:active={activeTab === "transactions"} onclick={() => (activeTab = "transactions")}>
    Transactions
  </button>
  <button class="tab-btn" class:active={activeTab === "upcoming"} onclick={() => (activeTab = "upcoming")}>
    Upcoming
  </button>
</div>

{#if activeTab === "upcoming"}
  <section class="card">
    <div class="empty">
      <p>Nothing scheduled yet</p>
      <p class="small faint" style="margin-top:4px">
        Upcoming/recurring transaction projections aren't tracked in this build.
      </p>
    </div>
  </section>
{:else}
<section class="card">
  <div class="row" style="gap:8px;margin-bottom:16px">
    <div class="search-box grow">
      <Icon name="search" size={18} />
      <input class="search-input" placeholder="Search transactions ..." bind:value={search} />
    </div>
    <div class="filter-wrap">
      <button
        type="button"
        class="btn"
        bind:this={filterBtnEl}
        onclick={() => (showFilterPanel = !showFilterPanel)}
        aria-expanded={showFilterPanel}
      >
        <Icon name="list-filter" size={16} />
        Filter
      </button>
      {#if showFilterPanel}
        <div class="filter-panel" bind:this={filterPanelEl}>
          <label class="field">Account
            <select class="select" aria-label="Filter by account" bind:value={accountId}>
              <option value="">All accounts</option>
              {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
            </select>
          </label>
          <label class="field">Category
            <select class="select" aria-label="Filter by category" bind:value={categoryId}>
              <option value="">All categories</option>
              {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
            </select>
          </label>
          <label class="field">Type
            <select class="select" aria-label="Filter by type" bind:value={typeFilter}>
              <option value="">All types</option>
              <option value="income">Income</option>
              <option value="expense">Outgoings</option>
            </select>
          </label>
          {#if people.list.length > 0}
            <label class="field">Attributed to
              <select class="select" aria-label="Filter by who it belongs to" bind:value={attributedTo}>
                <option value="">Whole household</option>
                {#each ownershipOptions() as o (o.key)}<option value={o.key}>{o.label}</option>{/each}
              </select>
            </label>
          {/if}
        </div>
      {/if}
    </div>
  </div>

  {#if activeChips.length > 0}
    <ul class="chip-row">
      {#each activeChips as chip (chip.key)}
        <li class="chip">
          {#if chip.icon}<Icon name={chip.icon} size={16} />{/if}
          <span class="chip-label">{chip.label}</span>
          <button type="button" class="chip-x" aria-label="Remove filter" onclick={chip.clear}>
            <Icon name="x" size={16} />
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if loading && txns.length === 0}
    <div class="row" style="justify-content:center;padding:30px"><span class="spinner"></span></div>
  {:else if sortedFiltered.length === 0}
    <div class="empty">No transactions.</div>
  {:else}
    <!-- One row style for every sort order — re-sorting only ever toggles the day-group
         headers on/off (they only make sense over the default newest-first date order,
         since a header assumes consecutive rows share a day), never the row styling itself.
         Fixed-width pieces (avatar, category pill, amount, delete) can add up to more than a
         narrow phone screen, so this scrolls horizontally rather than crushing the
         description to zero width. -->
    <div style="overflow-x:auto">
    <div class="tx-head">
      <div class="th-tx">
        <span class="tx-check">
          <input
            type="checkbox"
            aria-label="Select all transactions"
            title={allSelected ? "Clear selection" : "Select all"}
            checked={allSelected}
            indeterminate={someSelected}
            onchange={toggleAll}
          />
        </span>
        <span class="col-label">transaction</span>
      </div>
      <span class="th-cat col-label">Category label</span>
      <span class="th-amt col-label">Amount</span>
    </div>
    <div class="tx-list" class:grouped>
      {#if grouped}
        {#each renderGroups as g (g.dateKey)}
          {@const day = dayGroups.get(g.dateKey)}
          <div class="day-group">
            <div class="day-head">
              <div class="day-head-left">
                <span class="tx-check">
                  <input
                    type="checkbox"
                    aria-label="Select this day's transactions"
                    checked={g.rows.every((t) => selected.has(t.id))}
                    onchange={(e) => {
                      const on = e.currentTarget.checked;
                      const next = new Set(selected);
                      for (const t of g.rows) on ? next.add(t.id) : next.delete(t.id);
                      selected = next;
                    }}
                  />
                </span>
                <span class="day-date">{formatDateLong(g.rows[0].posted_at)} · {day?.count ?? 0}</span>
              </div>
              <span class="day-total tabular" class:pos={(day?.net ?? 0) >= 0}>
                {formatMoney(day?.net ?? 0, statCurrency)}
              </span>
            </div>
            <div class="day-rows">
              {#each g.rows as t (t.id)}
                {@render row(t)}
              {/each}
            </div>
          </div>
        {/each}
      {:else}
        {#each paged as t (t.id)}
          {@render row(t)}
        {/each}
      {/if}
    </div>
    </div>

    {#snippet row(t: Tx)}
      {@const title = t.description || txName(t) || "—"}
      {@const merchant = merchantName.get(t.merchant_id ?? -1) ?? t.merchant ?? ""}
      {@const cat = t.category_id != null ? catById.get(t.category_id) : null}
      {@const cc = cat ? (cat.color ?? colorFor(cat.parent_id ?? cat.id)) : colorFor(null)}
      <div
        id={`tx-${t.id}`}
        class="tx-row"
        class:highlight={t.id === highlightId}
        class:selected={selected.has(t.id)}
      >
        <div class="tx-name-cell">
          <label class="tx-check">
            <input
              type="checkbox"
              aria-label="Select transaction"
              checked={selected.has(t.id)}
              onchange={(e) => toggleOne(t.id, e.currentTarget.checked)}
            />
          </label>
          <span class="avatar">{title.charAt(0).toUpperCase()}</span>
          <div class="tx-main">
            <div class="tx-name-row">
              <span class="ell tx-name">{title}</span>
              {#if t.is_one_off}<span class="badge">one-off</span>{/if}
              {#if t.linked_transaction_id}<span class="badge">⇄ transfer</span>{/if}
              {#if people.list.length > 0}
                {@const owner = ownerOf(t)}
                {#if owner}
                  {@const c = ownershipColor(owner.ownership)}
                  <!-- Faint when it simply follows the account, solid when this transaction
                       was attributed by hand — so an override is visible at a glance. -->
                  <span
                    class="owner-chip"
                    class:inherited={owner.inherited}
                    style={c ? `--owner:${c}` : undefined}
                    title={owner.inherited
                      ? `Follows the account's owner`
                      : `Attributed to ${ownershipLabel(owner.ownership)} on this transaction`}
                  >
                    {ownershipLabel(owner.ownership)}
                  </span>
                {/if}
              {/if}
            </div>
            <span class="tx-sub ell">
              {#if merchant}{merchant} • {/if}{accountName.get(t.account_id) ?? "—"}{grouped ? "" : ` · ${formatDate(t.posted_at)}`}
            </span>
          </div>
        </div>
        <div class="cat-cell">
          <div class="cat-pill" style="--c:{cc}">
            {#if hasIcon(cat?.icon)}
              <span class="pill-icon"><Icon name={cat.icon} size={16} /></span>
            {:else if cat?.icon}
              <span class="pill-icon">{cat.icon}</span>
            {/if}
            <span class="ell">{cat?.name ?? "Uncategorised"}</span>
            <select
              class="pill-select"
              aria-label="Category"
              value={t.category_id ?? ""}
              onchange={(e) => setCategory(t, e.currentTarget.value === "" ? "" : Number(e.currentTarget.value))}
            >
              <option value="">— none —</option>
              {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
            </select>
          </div>
        </div>
        <span class="amt-cell tabular" class:pos={t.amount_minor >= 0}>
          {formatMoney(t.amount_minor, currencyOf.get(t.account_id) ?? t.currency_code)}
        </span>
      </div>
    {/snippet}

    <div class="pagination row spread wrap">
      <label class="row" style="gap:8px">
        <span class="small faint">Rows per page</span>
        <select class="select btn-sm" style="width:auto" bind:value={pageSize}>
          {#each PAGE_SIZES as n}<option value={n}>{n}</option>{/each}
        </select>
      </label>
      <nav class="pager" aria-label="Pagination">
        <button class="pager-nav" disabled={page <= 1} onclick={() => (page = page - 1)} aria-label="Previous page">‹</button>
        <div class="pager-pill">
          {#each pageNumbers as n}
            {#if n === "…"}
              <span class="pager-ellipsis">…</span>
            {:else}
              <button class="pager-num" class:active={n === page} onclick={() => (page = n)}>{n}</button>
            {/if}
          {/each}
        </div>
        <button class="pager-nav" disabled={page >= pageCount} onclick={() => (page = page + 1)} aria-label="Next page">›</button>
      </nav>
    </div>
  {/if}
</section>
{/if}

{#if selected.size > 0}
  <!-- Floating action bar: fixed to the viewport so it stays reachable no matter how far
       the (virtualised, possibly thousands-of-rows) list is scrolled. -->
  <div class="bulkbar" role="toolbar" aria-label="Bulk actions">
    <span class="count">{selected.size} selected</span>
    <button class="btn btn-sm" onclick={clearSelection} disabled={bulkBusy}>Clear</button>
    <span class="sep" aria-hidden="true"></span>

    <select class="select btn-sm" aria-label="Set category for selected" onchange={onBulkCategory} disabled={bulkBusy}>
      <option value="">Set category…</option>
      <option value="__clear__">— clear —</option>
      {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
    </select>
    {#if people.list.length > 0}
      <select
        class="select btn-sm"
        aria-label="Attribute selected to"
        onchange={onBulkOwner}
        disabled={bulkBusy}
      >
        <option value="">Attribute to…</option>
        {#each ownershipOptions() as o (o.key)}<option value={o.key}>{o.label}</option>{/each}
        <!-- Distinct from "not set": there is no such state. This drops the per-transaction
             override so the rows go back to following their account. -->
        <option value="inherit">Follow the account</option>
      </select>
    {/if}
    <select class="select btn-sm" aria-label="Set merchant for selected" onchange={onBulkMerchant} disabled={bulkBusy}>
      <option value="">Set merchant…</option>
      <option value="__clear__">— clear —</option>
      {#each merchants as m}<option value={m.id}>{m.name}</option>{/each}
    </select>
    <button class="btn btn-sm" onclick={() => bulkPatch({ is_one_off: true })} disabled={bulkBusy}>Mark one-off</button>
    <button class="btn btn-sm" onclick={() => bulkPatch({ is_one_off: false })} disabled={bulkBusy}>Clear one-off</button>

    <span class="sep" aria-hidden="true"></span>
    {#if confirmingDelete}
      <button class="btn btn-sm btn-danger" onclick={bulkDelete} disabled={bulkBusy}>
        Delete {selected.size}?
      </button>
      <button class="btn btn-sm" onclick={() => (confirmingDelete = false)} disabled={bulkBusy}>Cancel</button>
    {:else}
      <button class="btn btn-sm btn-danger" onclick={() => (confirmingDelete = true)} disabled={bulkBusy}>
        Delete
      </button>
    {/if}
    {#if bulkBusy}<span class="spinner" style="margin-left:4px"></span>{/if}
  </div>
{/if}
</div>

<style>
  /* ---- Stat card -------------------------------------------------------------- */
  /* Padding lives on each cell (matching the reference's per-cell p-4), not the card
     itself, so the divider between cells runs flush to the card's rounded edges. */
  .statcard {
    padding: 0;
  }
  .statbar {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
  }
  .statbar .stat {
    gap: 8px;
    padding: 16px;
  }
  .statbar .stat:not(:last-child) {
    border-right: 1px solid color-mix(in srgb, var(--text) 8%, transparent);
  }
  .statbar .label {
    font-size: 14px;
    font-weight: 400;
    text-transform: none;
    letter-spacing: normal;
    color: var(--text-muted);
  }
  .statbar .value {
    font-size: 20px;
    font-weight: 500;
    letter-spacing: normal;
  }
  @media (max-width: 560px) {
    .statbar {
      grid-template-columns: 1fr;
    }
    .statbar .stat:not(:last-child) {
      border-right: none;
      border-bottom: 1px solid color-mix(in srgb, var(--text) 8%, transparent);
    }
  }

  /* ---- Tabs (Transactions / Upcoming) ------------------------------------------- */
  .tabs-nav {
    display: inline-flex;
    gap: 2px;
    width: fit-content;
    padding: 4px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    margin-bottom: 16px;
  }
  .tab-btn {
    all: unset;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 5px 24px;
    border-radius: 6px;
    font-size: 14px;
    font-weight: 550;
    color: var(--text-muted);
    transition: background 0.15s, color 0.15s;
  }
  .tab-btn:hover:not(.active) {
    background: var(--hover);
    color: var(--text);
  }
  .tab-btn.active {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow);
  }

  /* ---- Header "…" action menu --------------------------------------------------- */
  .menu-wrap {
    position: relative;
  }
  .action-menu {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    min-width: 180px;
    padding: 6px;
    border-radius: var(--r);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: var(--shadow);
  }
  .action-item {
    padding: 8px 10px;
    border-radius: var(--r-sm);
    font-size: 13.5px;
    font-weight: 550;
    color: var(--text);
  }
  .action-item:hover {
    background: var(--hover);
  }

  /* ---- Filter popover ---------------------------------------------------------- */
  .filter-wrap {
    position: relative;
    flex: 0 0 auto;
  }
  .filter-panel {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: 220px;
    padding: 14px;
    border-radius: var(--r);
    border: 1px solid var(--border-strong);
    background: var(--bg-elev);
    box-shadow: var(--shadow);
  }

  /* ---- Pagination --------------------------------------------------------------- */
  .pagination {
    margin-top: 14px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .pager {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  /* Plain icon buttons (not bordered boxes) for prev/next, matching the reference. */
  .pager-nav {
    all: unset;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    padding: 6px 8px;
    font-size: 15px;
    color: var(--text-muted);
  }
  .pager-nav:hover:not(:disabled) {
    color: var(--text);
  }
  .pager-nav:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  /* Page numbers live together in one inset pill; the active page is its own raised,
     bordered sub-pill — a segmented control, not individually-boxed buttons. */
  .pager-pill {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 4px;
    border-radius: var(--r);
    background: var(--surface-2);
  }
  .pager-num {
    all: unset;
    cursor: pointer;
    padding: 5px 9px;
    border-radius: var(--r-sm);
    font-size: 13px;
    font-weight: 550;
    color: var(--text-muted);
  }
  .pager-num:hover {
    color: var(--text);
  }
  .pager-num.active {
    background: var(--surface);
    border: 1px solid var(--border-strong);
    box-shadow: var(--shadow);
    color: var(--text);
  }
  .pager-ellipsis {
    padding: 0 9px;
    color: var(--text-faint);
  }

  /* ---- Search box + filter chips ---------------------------------------------- */
  .search-box {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: var(--r-sm);
    color: var(--text-muted);
    background: var(--surface);
  }
  .search-box:focus-within {
    border-color: var(--text-muted);
  }
  .search-input {
    all: unset;
    flex: 1 1 auto;
    min-width: 0;
    font-size: 14px;
    color: var(--text);
  }
  .search-input::placeholder {
    color: var(--text-muted);
  }
  .chip-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    list-style: none;
    margin: 0 0 16px;
    padding: 0;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 6px 5px 10px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 14px;
    color: var(--text);
  }
  .chip :global(svg) {
    color: var(--text-muted);
  }
  .chip-x {
    all: unset;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    color: var(--text-muted);
    border-radius: 999px;
  }
  .chip-x:hover {
    color: var(--text);
  }

  /* ---- Column-label bar (a static inset header, matching the reference) -------- */
  .tx-head {
    display: grid;
    grid-template-columns: repeat(12, minmax(0, 1fr));
    align-items: center;
    padding: 12px 20px;
    margin-bottom: 16px;
    border-radius: var(--r);
    background: var(--surface-2);
    color: var(--text-muted);
    min-width: 480px;
  }
  .th-tx {
    grid-column: span 8;
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .th-cat {
    grid-column: span 2;
  }
  .th-amt {
    grid-column: span 2;
    justify-self: end;
  }
  .col-label {
    font-size: 12px;
    font-weight: 500;
    text-transform: uppercase;
    white-space: nowrap;
  }

  .tx-list {
    display: flex;
    flex-direction: column;
  }
  .tx-list.grouped {
    gap: 24px;
  }
  /* Each day is its own two-tier card — a subtle inset wrapper (day-group) holding a
     brighter, shadowed card of rows (day-rows) — matching the reference exactly rather
     than a single flat list with plain divider text. */
  .day-group {
    background: var(--surface-2);
    border-radius: var(--r);
    padding: 4px;
  }
  .day-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 16px;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-muted);
  }
  .day-head-left {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  .day-date {
    text-transform: uppercase;
  }
  .day-total {
    font-weight: 500;
  }
  .day-rows {
    background: var(--surface);
    border-radius: var(--r-sm);
    box-shadow: var(--shadow);
  }

  .tx-row {
    display: grid;
    grid-template-columns: repeat(12, minmax(0, 1fr));
    align-items: center;
    padding: 16px;
    border-radius: var(--r-sm);
    border: 1px solid transparent;
    min-width: 480px;
  }
  .tx-row:hover {
    background: var(--hover);
  }
  .owner-chip {
    flex: none;
    font-size: 11px;
    font-weight: 600;
    padding: 1px 7px;
    border-radius: 999px;
    border: 1px solid var(--owner, var(--border));
    color: var(--owner, var(--text-muted));
    white-space: nowrap;
  }
  /* Inherited from the account: present, but not competing with the description. */
  .owner-chip.inherited {
    font-weight: 500;
    border-style: dashed;
    opacity: 0.72;
  }
  .tx-row.selected {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .tx-row.highlight {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
    animation: tx-flash 1.8s ease-out;
  }

  .tx-name-cell {
    grid-column: span 8;
    display: flex;
    align-items: center;
    gap: 16px;
    min-width: 0;
  }
  .cat-cell {
    grid-column: span 2;
    min-width: 0;
    display: flex;
  }
  .amt-cell {
    grid-column: span 2;
    justify-self: end;
    text-align: right;
    white-space: nowrap;
    font-weight: 500;
    font-size: 14px;
  }

  .tx-check {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
  }
  /* Light, rounded checkbox matching the reference's `checkbox--light` — a subtle grey box
     at rest, filled on select. */
  .tx-check input {
    appearance: none;
    -webkit-appearance: none;
    width: 18px;
    height: 18px;
    margin: 0;
    border-radius: 6px;
    border: 1.5px solid color-mix(in srgb, var(--text) 13%, transparent);
    background: var(--surface);
    cursor: pointer;
    flex: 0 0 auto;
    transition: background 0.12s, border-color 0.12s;
  }
  .tx-check input:hover {
    border-color: color-mix(in srgb, var(--text) 32%, transparent);
  }
  .tx-check input:checked {
    background-color: var(--accent);
    border-color: var(--accent);
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23fff' stroke-width='3.5' stroke-linecap='round' stroke-linejoin='round'%3E%3Cpath d='M20 6 9 17l-5-5'/%3E%3C/svg%3E");
    background-size: 12px;
    background-position: center;
    background-repeat: no-repeat;
  }
  .tx-check input:indeterminate {
    background-color: var(--accent);
    border-color: var(--accent);
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%23fff' stroke-width='3.5' stroke-linecap='round'%3E%3Cpath d='M5 12h14'/%3E%3C/svg%3E");
    background-size: 12px;
    background-position: center;
    background-repeat: no-repeat;
  }
  .avatar {
    flex: 0 0 auto;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 14px;
    font-weight: 600;
    background: var(--surface-2);
    color: var(--text-muted);
  }
  .tx-main {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .tx-name-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .tx-name {
    font-weight: 500;
    font-size: 14px;
    line-height: 1.3;
    color: var(--text);
  }
  .tx-sub {
    font-size: 12px;
    font-weight: 400;
    line-height: 1.3;
    color: var(--text-muted);
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Category pill: a colour-coded chip (matching Categories/Dashboard) with the real
     <select> laid transparently on top, so the native picker still drives setCategory. */
  .cat-pill {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    padding: 4px 6px;
    border-radius: 999px;
    border: 1px solid color-mix(in oklab, var(--c) 10%, transparent);
    font-size: 14px;
    font-weight: 500;
    color: var(--c);
    background: color-mix(in oklab, var(--c) 10%, transparent);
    cursor: pointer;
  }
  .cat-pill .pill-icon {
    flex: 0 0 auto;
    display: inline-flex;
    line-height: 1;
  }
  .pill-select {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    margin: 0;
    padding: 0;
    border: none;
    opacity: 0;
    cursor: pointer;
    appearance: none;
  }

  /* Floating bulk-action bar, centred near the bottom of the viewport. */
  .bulkbar {
    position: fixed;
    left: 50%;
    bottom: max(18px, env(safe-area-inset-bottom));
    transform: translateX(-50%);
    z-index: 30;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    max-width: calc(100vw - 24px);
    padding: 10px 14px;
    border: 1px solid var(--border-strong);
    border-radius: 12px;
    background: var(--bg-elev);
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.28);
  }
  .bulkbar .count {
    font-weight: 600;
    white-space: nowrap;
  }
  .bulkbar .sep {
    width: 1px;
    align-self: stretch;
    background: var(--border);
    margin: 2px 2px;
  }

  @keyframes tx-flash {
    from {
      background: color-mix(in srgb, var(--accent) 34%, transparent);
    }
  }
</style>
