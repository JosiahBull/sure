<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, colorFor, type Schemas } from "../lib/api";
  import { RANGES, activeRange, filters, type RangeKey } from "../lib/state.svelte";
  import { router } from "../lib/router.svelte";

  // Deep links via the hash query, read once at mount:
  //   ?tx=<id>        highlight & scroll to a transaction (from the rules audit log)
  //   ?category=<id>  filter to a category and its whole subtree (from the overview pies)
  //   ?account=<id>   filter to a single account (from the accounts list)
  //   ?range=<key>    apply a preset time range
  //   ?at=<id>        resume the scroll position around a transaction (written as the list scrolls)
  const params = new URLSearchParams(router.path.split("?")[1] ?? "");
  const num = (v: string | null) => (v && Number.isFinite(Number(v)) ? Number(v) : null);
  const isRangeKey = (v: string | null): v is RangeKey => !!v && RANGES.some((r) => r.key === v);
  const highlightId = num(params.get("tx"));
  const paramCategory = num(params.get("category"));
  const paramAccount = num(params.get("account"));
  const paramRange = params.get("range");
  const paramAnchor = num(params.get("at"));

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
  let search = $state("");

  type SortKey = "date" | "description" | "account" | "category" | "merchant" | "amount";
  let sortKey = $state<SortKey>("date");
  let sortDir = $state<"asc" | "desc">("desc");
  function toggleSort(key: SortKey) {
    if (sortKey === key) sortDir = sortDir === "asc" ? "desc" : "asc";
    else {
      sortKey = key;
      sortDir = key === "date" || key === "amount" ? "desc" : "asc";
    }
  }
  const sortArrow = (key: SortKey) => (sortKey === key ? (sortDir === "asc" ? " ▲" : " ▼") : "");
  // Time range / one-off are the header's shared filters (App.svelte), not page-local —
  // deep links still get to steer them, though: an explicit `?range=` wins, and a
  // deep-linked transaction/account (which implies "show me everything relevant", not
  // just whatever slice happens to be selected) widens to "all" if no range was given.
  // Runs once per navigation here (this page remounts fresh on every route change), so
  // it behaves like the one-time default the local state used to be, without fighting
  // the user's choice on every subsequent render.
  if (isRangeKey(paramRange)) {
    filters.range = paramRange;
    filters.custom = null;
  } else if (highlightId != null || paramAccount != null) {
    filters.range = "all";
    filters.custom = null;
  }

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
  const currencyOf = $derived(new Map(accounts.map((a) => [a.id, a.currency_code])));
  const categoryName = $derived(new Map(categories.map((c) => [c.id, c.name])));
  const merchantName = $derived(new Map(merchants.map((m) => [m.id, m.name])));

  function sortValue(t: Tx, key: SortKey) {
    switch (key) {
      case "date":
        return t.posted_at;
      case "description":
        return (t.description ?? "").toLowerCase();
      case "account":
        return (accountName.get(t.account_id) ?? "").toLowerCase();
      case "category":
        return (categoryName.get(t.category_id ?? -1) ?? "").toLowerCase();
      case "merchant":
        return (merchantName.get(t.merchant_id ?? -1) ?? t.merchant ?? "").toLowerCase();
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
      return 0;
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

  // Virtualised, "infinite scroll" rendering: the table can easily hold thousands of rows
  // (each with two <select> menus), which is what actually makes the page feel slow — so
  // only a window of rows around `anchorId` is ever mounted, and it's recomputed straight
  // from the live scroll position on every scroll event (rather than incrementally sliding
  // step by step — that got stuck permanently the moment a step happened to be a geometric
  // no-op, e.g. right at the top of the list, since nothing then left/re-entered to trigger
  // the next one). The window is anchored to a transaction *id*, not a row index, so it keeps
  // pointing at the same transactions when new ones are added/removed elsewhere in the
  // (sorted) list — an index would silently drift. The anchor is persisted to `?at=`
  // (debounced) so a refresh resumes near the same transactions instead of at a fixed pixel
  // offset.
  const RENDER_COUNT = 60;
  const ROW_HEIGHT = 56; // estimate: converts scrolled px into an approximate row index
  const OVERSCAN = 15; // rendered on each side of the estimated position as a scroll buffer

  let anchorId = $state<number | null>(null);
  const resolvedAnchorIndex = $derived(
    anchorId == null ? 0 : Math.max(0, sortedFiltered.findIndex((t) => t.id === anchorId)),
  );
  const windowStart = $derived(Math.max(0, resolvedAnchorIndex - OVERSCAN));
  const windowEnd = $derived(Math.min(sortedFiltered.length, windowStart + RENDER_COUNT));
  const windowed = $derived(sortedFiltered.slice(windowStart, windowEnd));

  let persistTimer: ReturnType<typeof setTimeout> | undefined;
  function writeAnchorToUrl(id: number | null) {
    const p = new URLSearchParams(router.path.split("?")[1] ?? "");
    if (id == null) p.delete("at");
    else p.set("at", String(id));
    const base = router.path.split("?")[0];
    const qs = p.toString();
    history.replaceState(null, "", `#${base}${qs ? "?" + qs : ""}`);
  }
  function schedulePersist() {
    clearTimeout(persistTimer);
    persistTimer = setTimeout(() => writeAnchorToUrl(anchorId), 250);
  }

  // How far the table has scrolled past the viewport's top edge, converted to a row index —
  // recomputed fresh each time, so it can't get stuck the way an incremental step could.
  let tableEl = $state<HTMLElement | null>(null);
  let scrollRaf: ReturnType<typeof requestAnimationFrame> | undefined;
  function onScroll() {
    if (scrollRaf != null) return;
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = undefined;
      if (!tableEl || sortedFiltered.length === 0) return;
      const scrolledPast = Math.max(0, -tableEl.getBoundingClientRect().top);
      const estIndex = Math.min(sortedFiltered.length - 1, Math.floor(scrolledPast / ROW_HEIGHT));
      const id = sortedFiltered[estIndex]?.id;
      if (id != null && id !== anchorId) {
        anchorId = id;
        schedulePersist();
      }
    });
  }
  $effect(() => {
    window.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      window.removeEventListener("scroll", onScroll);
      if (scrollRaf != null) cancelAnimationFrame(scrollRaf);
      clearTimeout(persistTimer);
    };
  });

  // Resolve the initial anchor (from a highlight deep-link or a resumed `?at=`) once the
  // data it refers to has loaded, then let scrolling take over.
  let didInitAnchor = false;
  $effect(() => {
    if (didInitAnchor || sortedFiltered.length === 0) return;
    didInitAnchor = true;
    const initial = highlightId ?? paramAnchor;
    if (initial != null && sortedFiltered.some((t) => t.id === initial)) anchorId = initial;
  });

  // Filters/sort changing what's in the list makes the current anchor meaningless — jump
  // back to the top. (A reload with the same filters, e.g. after saving a row, is not a
  // change here, so the scroll position survives it.)
  let prevFilterSig: string | null = null;
  $effect(() => {
    const sig = `${accountId}|${categoryId}|${search}|${filters.includeOneOff}|${filters.range}|${filters.custom?.from}|${filters.custom?.to}|${sortKey}|${sortDir}`;
    if (prevFilterSig != null && sig !== prevFilterSig) {
      // The visible set changed, so a lingering selection could act on rows the user can
      // no longer see — clear it. (A same-filter reload, e.g. after a save, isn't a change.)
      if (selected.size > 0) clearSelection();
      if (anchorId != null) {
        anchorId = null;
        writeAnchorToUrl(null);
        window.scrollTo({ top: 0 });
      }
    }
    prevFilterSig = sig;
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
    // Category is filtered client-side (subtree-aware) in `sortedFiltered`, not on the server.
    const { data, error: e } = await api.GET("/api/transactions", { params: { query } });
    txns = data ?? [];
    if (e) error = "Failed to load transactions.";
    loading = false;
  }

  onMount(loadRefs);
  $effect(() => {
    // Category is filtered client-side, so it isn't a reload trigger.
    accountId;
    filters.includeOneOff;
    filters.range;
    filters.custom;
    loadTx();
  });

  // Once the anchor row has rendered (as the first row of the window, see above), scroll it
  // into view — smoothly with a flash for an explicit ?tx= deep-link, instantly/silently when
  // just resuming a scroll position from ?at=.
  let didScroll = false;
  $effect(() => {
    void windowed.length; // re-run as the rendered rows change
    const target = highlightId ?? paramAnchor;
    if (target == null || didScroll) return;
    const el = document.getElementById(`tx-${target}`);
    if (!el) return;
    didScroll = true;
    const highlighting = highlightId != null;
    requestAnimationFrame(() =>
      el.scrollIntoView({ block: highlighting ? "center" : "start", behavior: highlighting ? "smooth" : "auto" }),
    );
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
  const setMerchant = (t: Tx, m: number | "") =>
    saveTx(t, { merchant_id: m === "" ? null : m });

  async function del(t: Tx) {
    await api.DELETE("/api/transactions/{id}", { params: { path: { id: t.id } } });
    loadTx();
  }

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
</script>

<div class="row spread wrap" style="margin-bottom:14px;gap:10px">
  <h1 style="font-size:20px">Transactions</h1>
  <button class="btn btn-primary btn-sm" onclick={() => (showAdd = !showAdd)}>
    {showAdd ? "Close" : "+ Add"}
  </button>
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

<section class="card">
  {#if grouped && sortedFiltered.length > 0}
    <div class="statbar">
      <div class="stat">
        <span class="label">Total transactions</span>
        <span class="value tabular">{stats.count}</span>
      </div>
      <div class="stat">
        <span class="label">Income</span>
        <span class="value tabular pos">{formatMoney(stats.income, statCurrency)}</span>
      </div>
      <div class="stat">
        <span class="label">Expenses</span>
        <span class="value tabular neg">{formatMoney(Math.abs(stats.expenses), statCurrency)}</span>
      </div>
    </div>
  {/if}

  <div class="row wrap" style="gap:10px;margin-bottom:12px">
    <select class="select" style="width:auto" aria-label="Filter by account" bind:value={accountId}>
      <option value="">All accounts</option>
      {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
    </select>
    <select class="select" style="width:auto" bind:value={categoryId}>
      <option value="">All categories</option>
      {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
    </select>
    <input class="input grow" style="min-width:140px" placeholder="Search…" bind:value={search} />
  </div>

  {#if loading && txns.length === 0}
    <div class="row" style="justify-content:center;padding:30px"><span class="spinner"></span></div>
  {:else if sortedFiltered.length === 0}
    <div class="empty">No transactions.</div>
  {:else if grouped}
    <!-- Grouped-by-day view (default newest-first sort). The column labels are still sort
         buttons — clicking one re-sorts and drops to the flat table below. Each row's fixed-
         width pieces (avatar, category pill, amount, delete) can add up to more than a narrow
         phone screen, so this scrolls horizontally rather than crushing the description to
         zero width — same fallback the flat table below already uses. -->
    <div style="overflow-x:auto">
    <div class="tx-head">
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
      <button class="col-btn grow" onclick={() => toggleSort("description")}>Transaction{sortArrow("description")}</button>
      <button class="col-btn" onclick={() => toggleSort("category")}>Category{sortArrow("category")}</button>
      <button class="col-btn amount" onclick={() => toggleSort("amount")}>Amount{sortArrow("amount")}</button>
      <span class="tx-del" aria-hidden="true"></span>
    </div>
    <div class="tx-list" bind:this={tableEl}>
      <div aria-hidden="true" style={`height:${windowStart * ROW_HEIGHT}px`}></div>
      {#each windowed as t, i (t.id)}
        {@const dk = t.posted_at.slice(0, 10)}
        {@const name = txName(t)}
        {@const cat = t.category_id != null ? catById.get(t.category_id) : null}
        {@const cc = cat ? (cat.color ?? colorFor(cat.parent_id ?? cat.id)) : colorFor(null)}
        {#if i === 0 || windowed[i - 1].posted_at.slice(0, 10) !== dk}
          {@const day = dayGroups.get(dk)}
          <div class="day-head">
            <span class="day-date">{formatDate(t.posted_at)}</span>
            <span class="faint small">· {day?.count ?? 0}</span>
            <span
              class="tabular grow"
              style="text-align:right"
              class:pos={(day?.net ?? 0) >= 0}
              class:neg={(day?.net ?? 0) < 0}
            >
              {formatMoney(day?.net ?? 0, statCurrency)}
            </span>
          </div>
        {/if}
        <div
          id={`tx-${t.id}`}
          class="tx-row"
          class:highlight={t.id === highlightId}
          class:selected={selected.has(t.id)}
        >
          <label class="tx-check">
            <input
              type="checkbox"
              aria-label="Select transaction"
              checked={selected.has(t.id)}
              onchange={(e) => toggleOne(t.id, e.currentTarget.checked)}
            />
          </label>
          <span class="avatar" style="background:{colorFor(t.merchant_id ?? t.merchant ?? t.description)}">
            {(name || "?").charAt(0).toUpperCase()}
          </span>
          <div class="tx-main">
            <div class="tx-name-row">
              <span class="ell tx-name">{name || t.description || "—"}</span>
              {#if t.is_one_off}<span class="badge">one-off</span>{/if}
              {#if t.linked_transaction_id}<span class="badge">⇄ transfer</span>{/if}
            </div>
            <span class="small faint ell">{accountName.get(t.account_id) ?? "—"}</span>
          </div>
          <div class="cat-pill" style="--c:{cc}">
            {#if cat?.icon}<span class="pill-icon">{cat.icon}</span>{/if}
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
          <span
            class="tx-amount tabular"
            class:pos={t.amount_minor >= 0}
            class:neg={t.amount_minor < 0}
          >
            {formatMoney(t.amount_minor, currencyOf.get(t.account_id) ?? t.currency_code)}
          </span>
          <button class="btn btn-sm btn-danger tx-del" title="Delete" onclick={() => del(t)}>✕</button>
        </div>
      {/each}
      <div aria-hidden="true" style={`height:${(sortedFiltered.length - windowEnd) * ROW_HEIGHT}px`}></div>
    </div>
    </div>
  {:else}
    <div style="overflow-x:auto">
      <table class="table" bind:this={tableEl}>
        <thead>
          <tr>
            <th class="chk-col">
              <input
                type="checkbox"
                aria-label="Select all transactions"
                title={allSelected ? "Clear selection" : "Select all"}
                checked={allSelected}
                indeterminate={someSelected}
                onchange={toggleAll}
              />
            </th>
            <th><button class="sort-btn" onclick={() => toggleSort("date")}>Date{sortArrow("date")}</button></th>
            <th><button class="sort-btn" onclick={() => toggleSort("description")}>Description{sortArrow("description")}</button></th>
            <th><button class="sort-btn" onclick={() => toggleSort("account")}>Account{sortArrow("account")}</button></th>
            <th><button class="sort-btn" onclick={() => toggleSort("category")}>Category{sortArrow("category")}</button></th>
            <th><button class="sort-btn" onclick={() => toggleSort("merchant")}>Merchant{sortArrow("merchant")}</button></th>
            <th style="text-align:right"><button class="sort-btn" onclick={() => toggleSort("amount")}>Amount{sortArrow("amount")}</button></th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr aria-hidden="true">
            <td colspan="8" style={`padding:0;border:none;height:${windowStart * ROW_HEIGHT}px`}></td>
          </tr>
          {#each windowed as t (t.id)}
            <tr id={`tx-${t.id}`} class:highlight={t.id === highlightId} class:selected={selected.has(t.id)}>
              <td class="chk-col">
                <input
                  type="checkbox"
                  aria-label="Select transaction"
                  checked={selected.has(t.id)}
                  onchange={(e) => toggleOne(t.id, e.currentTarget.checked)}
                />
              </td>
              <td class="faint small" style="white-space:nowrap">{formatDate(t.posted_at)}</td>
              <td>
                {t.description || "—"}
                {#if t.is_one_off}<span class="badge" style="margin-left:6px">one-off</span>{/if}
                {#if t.linked_transaction_id}<span class="badge" style="margin-left:6px">⇄ transfer</span>{/if}
              </td>
              <td class="muted small">{accountName.get(t.account_id) ?? "—"}</td>
              <td>
                <select
                  class="select btn-sm"
                  style="width:auto;padding:4px 8px"
                  value={t.category_id ?? ""}
                  onchange={(e) => setCategory(t, (e.currentTarget.value === "" ? "" : Number(e.currentTarget.value)))}
                >
                  <option value="">—</option>
                  {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
                </select>
              </td>
              <td>
                <select
                  class="select btn-sm"
                  style="width:auto;padding:4px 8px"
                  value={t.merchant_id ?? ""}
                  onchange={(e) => setMerchant(t, (e.currentTarget.value === "" ? "" : Number(e.currentTarget.value)))}
                >
                  <option value="">—</option>
                  {#each merchants as m}<option value={m.id}>{m.name}</option>{/each}
                </select>
              </td>
              <td
                class="tabular"
                class:pos={t.amount_minor >= 0}
                class:neg={t.amount_minor < 0}
                style="text-align:right;white-space:nowrap"
              >
                {formatMoney(t.amount_minor, currencyOf.get(t.account_id) ?? t.currency_code)}
              </td>
              <td style="text-align:right">
                <button class="btn btn-sm btn-danger" title="Delete" onclick={() => del(t)}>✕</button>
              </td>
            </tr>
          {/each}
          <tr aria-hidden="true">
            <td colspan="8" style={`padding:0;border:none;height:${(sortedFiltered.length - windowEnd) * ROW_HEIGHT}px`}></td>
          </tr>
        </tbody>
      </table>
    </div>
    <div class="small faint" style="margin-top:10px">{sortedFiltered.length} transactions</div>
  {/if}
</section>

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

<style>
  /* ---- Grouped ("by day") view ---------------------------------------------- */
  .statbar {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
    padding-bottom: 16px;
    margin-bottom: 16px;
    border-bottom: 1px solid var(--border);
  }
  .statbar .value {
    font-size: 22px;
  }
  @media (max-width: 560px) {
    .statbar {
      grid-template-columns: 1fr;
      gap: 8px;
    }
  }

  /* Column-label bar; the labels are sort buttons that drop to the flat table. */
  .tx-head {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    border-radius: var(--r);
    background: var(--surface-2);
    color: var(--text-faint);
    min-width: 480px;
  }
  .col-btn {
    all: unset;
    cursor: pointer;
    font-size: 11.5px;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .col-btn:hover {
    color: var(--text);
  }
  .col-btn.amount {
    text-align: right;
    width: 110px;
    flex: 0 0 auto;
  }

  .tx-list {
    display: flex;
    flex-direction: column;
  }
  .day-head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 16px 12px 6px;
    font-size: 12px;
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .day-head .day-date {
    color: var(--text);
  }

  .tx-row {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    border-radius: var(--r);
    border: 1px solid transparent;
    min-width: 480px;
  }
  .tx-row:hover {
    background: var(--hover);
  }
  .tx-row.selected {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .tx-row.highlight {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
    animation: tx-flash 1.8s ease-out;
  }

  .tx-check {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
  }
  .tx-check input {
    cursor: pointer;
    vertical-align: middle;
  }
  .avatar {
    flex: 0 0 auto;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 14px;
    font-weight: 650;
    color: #fff;
  }
  .tx-main {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .tx-name-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .tx-name {
    font-weight: 550;
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
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 190px;
    padding: 5px 12px;
    border-radius: 999px;
    font-size: 13px;
    font-weight: 550;
    color: var(--c);
    background: color-mix(in srgb, var(--c) 16%, transparent);
    cursor: pointer;
  }
  .cat-pill .pill-icon {
    flex: 0 0 auto;
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
  .tx-amount {
    flex: 0 0 auto;
    width: 110px;
    text-align: right;
    white-space: nowrap;
    font-weight: 550;
  }
  .tx-del {
    flex: 0 0 auto;
  }

  .sort-btn {
    all: unset;
    cursor: pointer;
    white-space: nowrap;
  }

  /* Checkbox column: keep it tight and centred so the table's own columns stay put. */
  .chk-col {
    width: 1%;
    white-space: nowrap;
    text-align: center;
  }
  .chk-col input {
    cursor: pointer;
    vertical-align: middle;
  }

  tr.selected td {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
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

  /* Deep-linked transaction (e.g. from the rules audit log): a brief flash that settles
     into a subtle persistent tint so the row stays identifiable. */
  tr.highlight td {
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    animation: tx-flash 1.8s ease-out;
  }
  @keyframes tx-flash {
    from {
      background: color-mix(in srgb, var(--accent) 34%, transparent);
    }
  }
</style>
