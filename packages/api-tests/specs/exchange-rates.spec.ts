/**
 * Foreign exchange, from the feed to the converted figure.
 *
 * `sure_app::tasks::exchange_rates` records the bug this file exists for in its own module
 * doc: the poller "used to write a separate latest-only cache that nothing read, leaving
 * foreign-currency amounts silently at parity". Every foreign-currency figure in the app was
 * counted as if a US dollar were a New Zealand one, for years, and the suite was green the
 * whole time — because nothing in it could reach the poller. `exchange_rates` has exactly two
 * writers: a config-snapshot restore, and that one scheduled task. This suite runs with
 * `BACKGROUND_TASKS: "off"`, and there is no HTTP route that refreshes a rate, so the task's
 * half was unreachable. `specs/reports.spec.ts` covers conversion from *imported* rates, which
 * is the writer that was never broken.
 *
 * So every test here spawns its own backend with the scheduler ON and Frankfurter stubbed:
 * that is the only arrangement in which the table the poll writes and the table a report reads
 * are provably the same table. The `server`/`api` fixtures cannot be used — they boot with tasks
 * off — and the stub has to be registered *before* the process starts: the scheduler's first
 * check fires as the process comes up and every task is due on a fresh database, so a stub
 * registered after `startServer` returns is already racing the call it is meant to answer.
 */
import { test, expect, startServer, createSureClient, type StartedServer } from "../fixtures";
import type { ProxyClient } from "../proxy-client";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

/**
 * The reference date every stub below is published under.
 *
 * Deliberately not today: ECB rates are published once a day and Frankfurter serves the last
 * publication, so a poll on a Sunday legitimately answers with Friday's date, and the stored
 * `as_of` has to be the feed's rather than the fetch's.
 */
const FEED_DATE = "2026-03-09";

/** Quiet, except for the one line the exchange-rate task logs when a run finishes. */
const POLL_LOGS = { RUST_LOG: "error,sure_app::tasks::exchange_rates=info" };

/** One row of the `exchange_rates` table, as a config export serialises it. */
type StoredRate = { base_code: string; quote_code: string; as_of: string; rate: string };

/**
 * Stub Frankfurter's one endpoint with a body quoting `rates` against the base currency.
 *
 * The matcher never sees a query string, so `^/v1/latest$` is the whole identification of this
 * call: this stub answers `?base=USD` with an NZD table just as happily. The first test below
 * closes that off the recorder instead. `times` is left unlimited on purpose: a run that fails is
 * not recorded, so the scheduler retries it on its next check, and a stub that had retired would
 * turn that retry into a replay-miss WARN attributed to whichever test was unlucky.
 */
async function stubFrankfurter(testproxy: ProxyClient, rates: Record<string, number>): Promise<void> {
  await testproxy.stub({
    upstream: "frankfurter",
    method: "GET",
    path_pattern: "^/v1/latest$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ amount: 1, base: "NZD", date: FEED_DATE, rates }),
  });
}

/**
 * How many rates the completed run stored, out of the task's own log line — or `undefined`
 * while it has not finished.
 *
 * The only signal a *finished* run leaves anywhere a test can read: `scheduled_task_runs` is
 * not exposed over HTTP, and the stored rates themselves cannot tell "still working" from
 * "died part-way through", which is exactly the distinction one of the tests below is about.
 */
function storedByCompletedPoll(output: string): number | undefined {
  // The fmt subscriber colours field names, so `stored=2` is not a substring of the raw output
  // — the ESC has to go with the sequence or it sits between `stored` and `=`.
  const plain = output.replace(/\x1B\[[0-9;]*m/gu, "");
  const line = plain.split("\n").find((l) => l.includes("refreshed exchange rates"));
  const match = line?.match(/\bstored=(\d+)/);
  return match ? Number(match[1]) : undefined;
}

/** Block until the boot poll reports a completed run, and answer with what it stored. */
async function waitForCompletedPoll(server: StartedServer, timeoutMs = 10_000): Promise<number> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const stored = storedByCompletedPoll(server.output());
    if (stored !== undefined) return stored;
    if (Date.now() > deadline) {
      // The captured output is the diagnosis: a run that threw logs why, and a task that never
      // started logs nothing at all — two failures that look identical from out here.
      throw new Error(
        `the exchange-rate poll did not report a completed run within ${timeoutMs}ms:\n${server.output()}`,
      );
    }
    await new Promise((r) => setTimeout(r, 25));
  }
}

/**
 * Every `exchange_rates` row the database holds, sorted by quote currency.
 *
 * A config export is the only way to read the table over HTTP (there is no rates endpoint —
 * see the file comment). Sorted rather than compared in place: the export is a bare
 * `SELECT * FROM exchange_rates`, so its order is SQLite's rowid order, which is not a
 * property any test should be pinning.
 */
async function storedRates(api: SureClient): Promise<StoredRate[]> {
  const { data, response } = await api.GET("/api/config/export", {});
  expect(response.status, "config export").toBe(200);
  // Typed `unknown` by the generated client — the body is the DAL's whole `Snapshot`, which has
  // no utoipa schema — so the shape is asserted here rather than by tsc.
  const rows = (data as { exchange_rates?: StoredRate[] } | undefined)?.exchange_rates ?? [];
  return [...rows].sort((a, b) => a.quote_code.localeCompare(b.quote_code));
}

/** Block until the poll has stored a rate for `quote`, and answer with every stored rate. */
async function waitForRate(api: SureClient, quote: string, timeoutMs = 10_000): Promise<StoredRate[]> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const rates = await storedRates(api);
    if (rates.some((r) => r.quote_code === quote)) return rates;
    if (Date.now() > deadline) {
      throw new Error(`no ${quote} rate was stored within ${timeoutMs}ms; stored: ${JSON.stringify(rates)}`);
    }
    await new Promise((r) => setTimeout(r, 25));
  }
}

const balances = (api: SureClient) => api.GET("/api/reports/balances", { params: { query: {} } });

test("the boot poll stores a rate for every currency the ledger knows and skips the rest", async ({
  testproxy,
}) => {
  // CHF is a real Frankfurter currency with no `currencies` row (migration 0001 seeds
  // NZD/USD/AUD/GBP/EUR and nothing else), so `exchange_rates.quote_code`'s foreign key could
  // not accept it — which is why the task skips it rather than offering it and failing. It also
  // sorts between AUD and USD in the `BTreeMap` the adapter parses into, so the run has to get
  // *past* it to reach USD: if an unquotable code failed the run instead of being skipped, the
  // missing USD row is what says so.
  await stubFrankfurter(testproxy, { AUD: 0.92, CHF: 0.48, USD: 0.625 });

  const server = await startServer({ ...POLL_LOGS, BACKGROUND_TASKS: "on" }, { capture: true });
  try {
    // Two of the three, from a run that reported itself complete: both halves of the property
    // in one number.
    expect(await waitForCompletedPoll(server)).toBe(2);

    expect(await storedRates(createSureClient(server.baseURL))).toEqual([
      { base_code: "NZD", quote_code: "AUD", as_of: FEED_DATE, rate: "0.92" },
      { base_code: "NZD", quote_code: "USD", as_of: FEED_DATE, rate: "0.625" },
    ]);

    // What the poll *asked* for, which everything above only assumes. `base_code: "NZD"` on those
    // rows is the task's own idea of the base — `sure_dal::settings::base_currency`, stamped onto
    // whatever table came back — so an adapter that sent `?base=USD` would store USD-quoted
    // numbers labelled NZD, the stub would answer it just the same, and every assertion above
    // would still pass. Readable here and not in stock-prices.spec.ts because Frankfurter declares
    // no volatile query parameters (`Upstream::volatile_query_params`), so nothing canonicalises
    // this one on its way into the ring.
    const asked = await testproxy.queryTraffic({ upstream: "frankfurter" });
    expect(asked.map((e) => e.request.uri)).toEqual(["/v1/latest?base=NZD"]);
  } finally {
    server.stop();
  }
});

test("a report converts a foreign balance at the polled rate, never at parity", async ({ testproxy }) => {
  // 1 NZD = 0.625 USD. That rate is exactly representable in binary, so the figure asserted
  // below pins the conversion itself (minor units, divided by the rate, rounded once at the
  // end) rather than a rounding convention that a cent of float drift could move.
  await stubFrankfurter(testproxy, { USD: 0.625 });

  const server = await startServer({ ...POLL_LOGS, BACKGROUND_TASKS: "on" }, { capture: true });
  try {
    const api = createSureClient(server.baseURL);
    // Before the accounts exist, so the report below cannot be racing the poll: whatever it
    // answers, the rate was already on record when it was asked.
    await waitForRate(api, "USD");

    await createAccount(api, "Everyday", "bank", "NZD", {
      opening_balance_minor: 50_000,
      opening_balance_date: "2026-01-01",
    });
    const us = await createAccount(api, "US Savings", "savings", "USD", {
      opening_balance_minor: 125_000,
      opening_balance_date: "2026-01-01",
    });

    const report = await balances(api);
    expect(report.response.status).toBe(200);
    // US$1,250.00 at 1 NZD = 0.625 USD is NZ$2,000.00, plus the NZ$500.00 account: NZ$2,500.00.
    // The parity bug's answer was 175_000 — the two balances added together as though a dollar
    // were a dollar. An assertion that only checked "not zero" would have passed throughout.
    expect(report.data!.total_minor).toBe(250_000);
    expect(report.data!.currency).toBe("NZD");
    // Nothing was withheld from that total, and it is dated by the rate that made it.
    expect(report.data!.unconverted).toEqual([]);
    expect(report.data!.rates_as_of).toBe(FEED_DATE);

    // The account row itself stays in its own currency — only the roll-up is converted, so a
    // regression that converted twice would show up here as well as in the total.
    const usd = report.data!.accounts.find((a) => a.account_id === us.id)!;
    expect(usd.currency_code).toBe("USD");
    expect(usd.value_minor).toBe(125_000);
  } finally {
    server.stop();
  }
});

test("a currency the feed did not quote is named as unconverted, not folded in at parity", async ({
  testproxy,
}) => {
  // The feed answers and the poll succeeds — it just has nothing to say about AUD. That is a
  // designed-for state, not a failure: `Fx::unconverted` names the currency and `FxNotice.svelte`
  // renders it, because an unconverted foreign balance is a *wrong* number, not a missing one,
  // and the only honest total is one that leaves it out and says which currency it left out.
  await stubFrankfurter(testproxy, { USD: 0.625 });

  const server = await startServer({ ...POLL_LOGS, BACKGROUND_TASKS: "on" }, { capture: true });
  try {
    const api = createSureClient(server.baseURL);
    await waitForRate(api, "USD");

    await createAccount(api, "Everyday", "bank", "NZD", {
      opening_balance_minor: 50_000,
      opening_balance_date: "2026-01-01",
    });
    const aud = await createAccount(api, "Aussie Savings", "savings", "AUD", {
      opening_balance_minor: 30_000,
      opening_balance_date: "2026-01-01",
    });

    const report = await balances(api);
    expect(report.data!.unconverted).toEqual(["AUD"]);
    // The NZD account alone. 80_000 would be the parity bug; the AUD balance is excluded, which
    // is what the notice above is telling the user about.
    expect(report.data!.total_minor).toBe(50_000);
    // …and the account is still listed at its true value, so nothing has gone missing from the
    // screen — only from the roll-up.
    const row = report.data!.accounts.find((a) => a.account_id === aud.id)!;
    expect(row.currency_code).toBe("AUD");
    expect(row.value_minor).toBe(30_000);
    // The rate that *did* land still dates the report: "some of this is excluded" and "the
    // rates are from the 9th" are two separate things the notice says.
    expect(report.data!.rates_as_of).toBe(FEED_DATE);
  } finally {
    server.stop();
  }
});

test("the stored as_of is the feed's reference date, not the moment it was fetched", async ({ testproxy }) => {
  // What the date is for: the poller only writes on success, so a feed that died months ago
  // leaves last quarter's rates in place looking exactly like this morning's, and `rates_as_of`
  // is the only thing that can tell the two apart — it is what `FxNotice.svelte`'s staleness
  // banner is computed from. Stamping the fetch time here would make a dead feed permanently
  // fresh, and every figure derived from it permanently trustworthy-looking.
  await stubFrankfurter(testproxy, { USD: 0.625 });

  const server = await startServer({ ...POLL_LOGS, BACKGROUND_TASKS: "on" }, { capture: true });
  try {
    const api = createSureClient(server.baseURL);
    const stored = await waitForRate(api, "USD");
    expect(stored).toEqual([{ base_code: "NZD", quote_code: "USD", as_of: FEED_DATE, rate: "0.625" }]);

    const report = await balances(api);
    expect(report.data!.rates_as_of).toBe(FEED_DATE);
    // And the two dates really are different, which is what makes the assertion above mean
    // something. Compared against the *server's* today (a balance sheet with no `to` is "as at
    // today") rather than this process's clock, so there is one clock in the comparison.
    expect(report.data!.as_of).not.toBe(FEED_DATE);
  } finally {
    server.stop();
  }
});
