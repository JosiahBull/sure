<script lang="ts">
  import { onMount } from "svelte";
  import { api, formatMoney, formatDate, type Schemas } from "../lib/api";
  import { RANGES, rangeDates, type RangeKey } from "../lib/state.svelte";
  import { router } from "../lib/router.svelte";

  // Deep links via the hash query, read once at mount:
  //   ?tx=<id>        highlight & scroll to a transaction (from the rules audit log)
  //   ?category=<id>  filter to a category and its whole subtree (from the overview pies)
  //   ?range=<key>    apply a preset time range
  const params = new URLSearchParams(router.path.split("?")[1] ?? "");
  const num = (v: string | null) => (v && Number.isFinite(Number(v)) ? Number(v) : null);
  const isRangeKey = (v: string | null): v is RangeKey => !!v && RANGES.some((r) => r.key === v);
  const highlightId = num(params.get("tx"));
  const paramCategory = num(params.get("category"));
  const paramRange = params.get("range");

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

  let accountId = $state<number | "">("");
  let categoryId = $state<number | "">(paramCategory ?? "");
  let search = $state("");
  let includeOneOff = $state(true);

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
  // A range from the link wins; otherwise a deep-linked transaction can be any date, so
  // widen to "all" to be sure it's loaded.
  let range = $state<RangeKey>(
    isRangeKey(paramRange) ? paramRange : highlightId != null ? "all" : "last_90",
  );

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

  const visible = $derived.by(() => {
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
    const { from, to } = rangeDates(range);
    const query: Record<string, unknown> = { from, to, include_one_off: includeOneOff, limit: 2000 };
    if (accountId !== "") query.account_id = accountId;
    // Category is filtered client-side (subtree-aware) in `visible`, not on the server.
    const { data, error: e } = await api.GET("/api/transactions", { params: { query } });
    txns = data ?? [];
    if (e) error = "Failed to load transactions.";
    loading = false;
  }

  onMount(loadRefs);
  $effect(() => {
    // Category is filtered client-side, so it isn't a reload trigger.
    accountId;
    includeOneOff;
    range;
    loadTx();
  });

  // Once the deep-linked row has rendered, scroll it into view (flash comes from CSS).
  let didScroll = false;
  $effect(() => {
    void visible.length; // re-run as the rendered rows change
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
  const setMerchant = (t: Tx, m: number | "") =>
    saveTx(t, { merchant_id: m === "" ? null : m });

  async function del(t: Tx) {
    await api.DELETE("/api/transactions/{id}", { params: { path: { id: t.id } } });
    loadTx();
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
  <div class="row wrap" style="gap:10px;margin-bottom:12px">
    <select class="select" style="width:auto" bind:value={range}>
      {#each RANGES as r}<option value={r.key}>{r.label}</option>{/each}
    </select>
    <select class="select" style="width:auto" bind:value={accountId}>
      <option value="">All accounts</option>
      {#each accounts as a}<option value={a.id}>{a.name}</option>{/each}
    </select>
    <select class="select" style="width:auto" bind:value={categoryId}>
      <option value="">All categories</option>
      {#each categories as c}<option value={c.id}>{c.name}</option>{/each}
    </select>
    <input class="input grow" style="min-width:140px" placeholder="Search…" bind:value={search} />
    <label class="switch">
      <input type="checkbox" bind:checked={includeOneOff} /><span class="track"></span>
      <span>One-off</span>
    </label>
  </div>

  {#if loading && txns.length === 0}
    <div class="row" style="justify-content:center;padding:30px"><span class="spinner"></span></div>
  {:else if visible.length === 0}
    <div class="empty">No transactions.</div>
  {:else}
    <div style="overflow-x:auto">
      <table class="table">
        <thead>
          <tr>
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
          {#each visible as t (t.id)}
            <tr id={`tx-${t.id}`} class:highlight={t.id === highlightId}>
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
        </tbody>
      </table>
    </div>
    <div class="small faint" style="margin-top:10px">{visible.length} transactions</div>
  {/if}
</section>

<style>
  .sort-btn {
    all: unset;
    cursor: pointer;
    white-space: nowrap;
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
