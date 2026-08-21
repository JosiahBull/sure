<script lang="ts">
  // Every expected payday against the deposit that satisfied it — the review surface for the
  // matcher. Matched rows expand to the payslip reconstructed from what actually landed
  // (gross − PAYE − ACC − KiwiSaver − student loan = the deposit, to the cent); expected rows
  // past their date are the missed pays; linking happens inline against the handful of
  // deposits near the due date, and anything odder deep-links to Transactions.
  import { onMount } from "svelte";
  import { api, formatMoney, type Schemas } from "../../lib/api";

  type IncomePayment = Schemas["IncomePayment"];
  type IncomeStream = Schemas["IncomeStream"];

  let payments = $state<IncomePayment[]>([]);
  let streams = $state<IncomeStream[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let rematching = $state(false);
  let showAll = $state(false);
  /** The payment whose payslip or candidate list is expanded. */
  let open = $state<number | null>(null);
  let candidates = $state<Schemas["Transaction"][]>([]);
  let candidatesFor = $state<number | null>(null);

  const today = new Date().toISOString().slice(0, 10);
  const SHOWN = 20;

  async function load() {
    loading = true;
    const [p, s] = await Promise.all([
      api.GET("/api/income-payments", { params: { query: {} } }),
      api.GET("/api/income-streams", {}),
    ]);
    payments = p.data ?? [];
    streams = s.data ?? [];
    error = p.error || s.error ? "Failed to load payments." : null;
    loading = false;
  }
  onMount(load);

  const streamById = $derived(new Map(streams.map((s) => [s.id, s])));

  /** Future expected rows are the schedule, not news — the review list starts at today. */
  const reviewable = $derived(
    payments.filter((p) => p.status !== "expected" || p.due_on <= today)
  );
  const shown = $derived(showAll ? reviewable : reviewable.slice(0, SHOWN));
  const anyMatchable = $derived(streams.some((s) => s.match_account_id != null));

  async function rematch() {
    rematching = true;
    error = null;
    const { error: e } = await api.POST("/api/income-payments/rematch", {});
    if (e) error = "The matcher run failed.";
    rematching = false;
    await load();
  }

  async function act(fn: () => Promise<{ error?: unknown }>) {
    error = null;
    const { error: e } = await fn();
    if (e) {
      error =
        (e as { error?: { message?: string } }).error?.message ?? "That didn't work.";
      return;
    }
    await load();
  }
  const setStatus = (id: number, status: Schemas["IncomePaymentStatus"]) =>
    act(() =>
      api.PATCH("/api/income-payments/{id}", { params: { path: { id } }, body: { status } })
    );
  const unlink = (id: number) =>
    act(() => api.DELETE("/api/income-payments/{id}/link", { params: { path: { id } } }));
  const link = (id: number, transactionId: number) =>
    act(async () => {
      const res = await api.POST("/api/income-payments/{id}/link", {
        params: { path: { id } },
        body: { transaction_id: transactionId },
      });
      if (!res.error) {
        candidatesFor = null;
        open = null;
      }
      return res;
    });

  /** The deposits near an expected payday, for linking by hand. */
  async function findCandidates(p: IncomePayment) {
    const stream = streamById.get(p.income_stream_id);
    candidatesFor = p.id;
    candidates = [];
    const due = new Date(p.due_on);
    const from = new Date(due);
    from.setDate(from.getDate() - 5);
    const to = new Date(due);
    to.setDate(to.getDate() + 3);
    const { data } = await api.GET("/api/transactions", {
      params: {
        query: {
          account_id: stream?.match_account_id ?? undefined,
          from: from.toISOString().slice(0, 10),
          to: to.toISOString().slice(0, 10),
        },
      },
    });
    candidates = (data ?? []).filter((t) => t.amount_minor > 0).slice(0, 5);
  }

  function statusLabel(p: IncomePayment): string {
    if (p.status === "expected") return p.due_on < today ? "missed" : "expected";
    return p.status;
  }
  function currencyOf(p: IncomePayment): string {
    return streamById.get(p.income_stream_id)?.currency_code ?? "NZD";
  }
  /** How far the deposit landed from the prediction, worth a look past a few dollars. */
  function drift(p: IncomePayment): number | null {
    if (p.observed_net_minor == null || p.expected_net_minor == null) return null;
    return p.observed_net_minor - p.expected_net_minor;
  }
</script>

{#if streams.length === 0 && payments.length === 0}
  <!-- No income at all: nothing to review and nothing to explain — stay out of the way. The
       panel deliberately DOES show for a household with streams but no match config, because
       hiding it hid the very instruction ("set an account and memo") that person needed. -->
{:else}
  <section class="card" style="margin-top:16px">
    <div class="card-title">
      <div>
        <h2 style="margin-bottom:0">Pay matching</h2>
        <div class="muted small">
          Each expected payday, checked off against the deposit that satisfied it.
        </div>
      </div>
      <button class="btn btn-sm" onclick={rematch} disabled={rematching}>
        {rematching ? "Matching…" : "Match now"}
      </button>
    </div>

    {#if error}<div class="error-banner small" style="margin-bottom:10px">{error}</div>{/if}

    {#if loading && payments.length === 0}
      <div class="row" style="justify-content:center;padding:24px">
        <span class="spinner"></span>
      </div>
    {:else if reviewable.length === 0}
      <div class="empty">
        {#if anyMatchable}
          Nothing yet — press Match now, or wait a few minutes for the background pass.
        {:else}
          No income is set up for matching. Edit a stream above and fill in
          <strong>Match deposits automatically</strong> — the account its pay lands in and a word
          its deposit memo always carries — then press Match now.
        {/if}
      </div>
    {:else}
      <div class="pay-list">
        {#each shown as p (p.id)}
          {@const stream = streamById.get(p.income_stream_id)}
          {@const d = drift(p)}
          <div class="pay-row" class:missed={statusLabel(p) === "missed"}>
            <div class="row spread wrap" style="gap:8px">
              <span class="row" style="gap:8px;min-width:0">
                <span class="chip status-{p.status}">{statusLabel(p)}</span>
                <span class="tabular small">{p.due_on}</span>
                <span class="ell" style="font-weight:560">{stream?.label ?? "Income"}</span>
              </span>
              <span class="row" style="gap:10px">
                {#if p.observed_net_minor != null}
                  <span class="tabular small">
                    {formatMoney(p.observed_net_minor, currencyOf(p))}
                  </span>
                  {#if d != null && Math.abs(d) > 5_00}
                    <!-- The reconciliation signal: a persistent gap here is a pay rise or a
                         wrong KiwiSaver rate, and the fix is the stream's config. -->
                    <span class="small warn-text tabular">
                      {d > 0 ? "+" : ""}{formatMoney(d, currencyOf(p))} vs expected
                    </span>
                  {/if}
                {:else if p.expected_net_minor != null}
                  <span class="tabular small faint">
                    ~{formatMoney(p.expected_net_minor, currencyOf(p))}
                  </span>
                {/if}
                {#if p.status === "matched"}
                  <button class="btn btn-sm" onclick={() => setStatus(p.id, "confirmed")}>
                    Confirm
                  </button>
                {/if}
                {#if p.transaction_id != null}
                  <button
                    class="btn btn-sm"
                    onclick={() => (open = open === p.id ? null : p.id)}
                  >
                    {open === p.id ? "Hide payslip" : "Payslip"}
                  </button>
                  <button class="btn btn-sm btn-danger" onclick={() => unlink(p.id)}>
                    Unlink
                  </button>
                {:else if p.status === "expected"}
                  <button
                    class="btn btn-sm"
                    onclick={() =>
                      candidatesFor === p.id ? (candidatesFor = null) : findCandidates(p)}
                  >
                    {candidatesFor === p.id ? "Close" : "Link…"}
                  </button>
                  <button class="btn btn-sm" onclick={() => setStatus(p.id, "dismissed")}>
                    Dismiss
                  </button>
                {:else if p.status === "dismissed"}
                  <button class="btn btn-sm" onclick={() => setStatus(p.id, "expected")}>
                    Re-open
                  </button>
                {/if}
              </span>
            </div>

            {#if open === p.id && p.gross_minor != null}
              <div class="payslip small tabular">
                <span>Gross {formatMoney(p.gross_minor, currencyOf(p))}</span>
                <span>PAYE −{formatMoney(p.income_tax_minor ?? 0, currencyOf(p))}</span>
                <span>ACC −{formatMoney(p.acc_levy_minor ?? 0, currencyOf(p))}</span>
                <span>KiwiSaver −{formatMoney(p.kiwisaver_minor ?? 0, currencyOf(p))}</span>
                <span>Student loan −{formatMoney(p.student_loan_minor ?? 0, currencyOf(p))}</span>
                <span style="font-weight:600">
                  Landed {formatMoney(p.observed_net_minor ?? 0, currencyOf(p))}
                </span>
              </div>
            {/if}

            {#if candidatesFor === p.id}
              <div class="cands">
                {#if candidates.length === 0}
                  <div class="small faint">
                    No deposits near {p.due_on} in that account.
                    <a href="#/transactions">Look in Transactions</a> instead.
                  </div>
                {:else}
                  {#each candidates as t (t.id)}
                    <button type="button" class="cand-row" onclick={() => link(p.id, t.id)}>
                      <span class="tabular small">{t.posted_at.slice(0, 10)}</span>
                      <span class="ell small">{t.description}</span>
                      <span class="tabular small" style="font-weight:600">
                        {formatMoney(t.amount_minor, t.currency_code)}
                      </span>
                    </button>
                  {/each}
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
      {#if reviewable.length > SHOWN}
        <button class="btn btn-sm" style="margin-top:10px" onclick={() => (showAll = !showAll)}>
          {showAll ? "Show recent only" : `Show all ${reviewable.length}`}
        </button>
      {/if}
    {/if}
  </section>
{/if}

<style>
  .pay-list {
    display: flex;
    flex-direction: column;
  }
  .pay-row {
    padding: 9px 0;
    border-top: 1px solid var(--border);
  }
  .pay-row:first-child {
    border-top: none;
  }
  .pay-row.missed {
    background: color-mix(in srgb, var(--warn) 5%, transparent);
  }
  .status-matched {
    color: var(--positive);
  }
  .status-confirmed {
    color: var(--positive);
  }
  .status-expected {
    color: var(--text-muted);
  }
  .status-dismissed {
    color: var(--text-faint);
  }
  .payslip {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    margin-top: 8px;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .cands {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 8px;
  }
  .cand-row {
    all: unset;
    display: grid;
    grid-template-columns: 90px 1fr auto;
    gap: 10px;
    align-items: baseline;
    box-sizing: border-box;
    width: 100%;
    padding: 5px 8px;
    border-radius: var(--r-sm);
    cursor: pointer;
  }
  .cand-row:hover {
    background: var(--hover);
  }
  .cand-row:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .warn-text {
    color: var(--warn);
  }
</style>
