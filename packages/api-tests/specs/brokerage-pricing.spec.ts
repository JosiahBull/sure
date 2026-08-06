/**
 * The brokerage endpoints that compute a *priced* answer: the value snapshot, the valuation it
 * persists, and the day-by-day valuation history. All three resolve every position through
 * `sure_app::stock_prices::price_at`, which is why none of them was asserted before this suite
 * had a proxy — brokerage.spec.ts drives the import path and stops at the two ledger reads. A
 * priced snapshot is the number this feature exists to produce, and it was the one thing nothing
 * checked.
 *
 * Every money assertion below is an exact minor-unit figure worked out here from the quantity and
 * the stubbed close, and the arithmetic is written into the comment beside it. "Not zero", or
 * "more than the cash balance", would have passed against a snapshot that priced nothing at all,
 * and against the parity bug `sure_app::tasks::exchange_rates`'s module doc records — the FX test
 * below states both the right answer and that bug's answer, so a failure says which mistake was
 * made rather than only that the total moved.
 *
 * `?as_of=` fixes the date each route computes its window from, which is what makes a stub's
 * `path_pattern` reliably match and every figure a pure function of the URL. The backfill has no
 * such parameter — it walks from the account's first activity to `Clock::today()` — so the tests
 * that drive it anchor their fixtures to today instead of to a literal date. That keeps the walk
 * a fixed length however old this file gets, and survives the one clock hazard it has: the JS
 * process and the backend both read a UTC date, so they can disagree only across midnight, and
 * only by a day at either end of a window whose interior is what gets asserted.
 */
import { test, expect, startServer, createSureClient, type StartedServer } from "../fixtures";
import type { SureClient } from "../../client/src/index";
import { createAccount, createTransaction, makeZip } from "../helpers";

/**
 * Yahoo's chart shape, from `[trading day, close]` pairs.
 *
 * A near-copy of stock-prices.spec.ts's, which documents which of these fields the adapter
 * actually reads and which it ignores. Duplicated rather than shared because the alternative is
 * a third file (`helpers.ts`) that both specs import a Yahoo-specific body builder from, and
 * because the two copies answer to the same `ChartResponse`: if one stops deserialising, so does
 * the other, and both fail rather than one silently drifting.
 *
 * The timestamps sit at local midday (`+ 12h - gmtoffset`), because the adapter adds `gmtoffset`
 * back before taking the calendar date — placing them at either boundary would let a fixture
 * file a close under the neighbouring day.
 */
function chart(currency: string, gmtoffset: number, days: Array<[string, number]>): string {
  return JSON.stringify({
    chart: {
      result: [
        {
          meta: { currency, gmtoffset },
          timestamp: days.map(
            ([day]) => Date.parse(`${day}T00:00:00Z`) / 1000 + 12 * 3600 - gmtoffset,
          ),
          indicators: { quote: [{ close: days.map(([, close]) => close) }] },
        },
      ],
    },
  });
}

/** Winter offsets for the two exchanges quoted here. NZX's +12 is the one far enough from UTC
 * to move a close onto another calendar day, so it is the interesting half of the pair. */
const NEW_YORK = -5 * 3600;
const WELLINGTON = 12 * 3600;

/** The backend's own today, as it computes it: `SystemClock::today` is `Utc::now().date_naive()`. */
const TODAY_UTC = Date.parse(`${new Date().toISOString().slice(0, 10)}T00:00:00Z`);

/** A date `offset` days from today, `YYYY-MM-DD`. Negative is in the past. */
function day(offset: number): string {
  return new Date(TODAY_UTC + offset * 86_400_000).toISOString().slice(0, 10);
}

/** One `exchange_rates` row, as a config export serialises it. */
type StoredRate = { base_code: string; quote_code: string; as_of: string; rate: string };

/**
 * Block until the boot poll has stored an NZD→`quote` rate.
 *
 * A config export is the only way to read `exchange_rates` over HTTP: there is no rates endpoint,
 * and the table's only two writers are that scheduled poll and a config restore (the whole reason
 * specs/exchange-rates.spec.ts spawns its own scheduler-on backend). Waiting for the rate *before*
 * the account exists is what stops the snapshot below racing the poll — whatever the snapshot
 * answers, the rate was already on record when it was asked, so a wrong total is arithmetic
 * rather than timing.
 */
async function waitForRate(
  api: SureClient,
  server: StartedServer,
  quote: string,
  timeoutMs = 10_000,
): Promise<StoredRate> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const { data } = await api.GET("/api/config/export", {});
    // Typed `unknown` by the generated client — the body is the DAL's whole `Snapshot`, which has
    // no utoipa schema — so the shape is asserted here rather than by tsc.
    const rows = (data as { exchange_rates?: StoredRate[] } | undefined)?.exchange_rates ?? [];
    const row = rows.find((r) => r.quote_code === quote);
    if (row) return row;
    if (Date.now() > deadline) {
      // The captured output is the diagnosis: a poll that threw logs why, and one that never ran
      // logs nothing — two failures that look identical from out here.
      throw new Error(`no NZD→${quote} rate stored within ${timeoutMs}ms:\n${server.output()}`);
    }
    await new Promise((r) => setTimeout(r, 25));
  }
}

/** Every valuation this account holds, keyed by its date. */
async function valuationsByDay(
  api: SureClient,
  accountId: number,
): Promise<Map<string, { value_minor: number; currency_code: string; source: string }>> {
  const { data, response } = await api.GET("/api/accounts/{id}/valuations", {
    params: { path: { id: accountId } },
  });
  expect(response.status, "list valuations").toBe(200);
  return new Map((data ?? []).map((v) => [v.as_of, v]));
}

/**
 * A priced snapshot, to the cent.
 *
 * The three figures are independent and all three have to be right for the panel to be: the
 * position's market value comes from the *feed's* close, its cost basis from the lot's own
 * recorded price plus its fee, and the account total from the value plus the wallet's cash. A
 * snapshot that fetched nothing totals 41_290 — the cash alone, and a perfectly healthy-looking
 * number; one that priced from the lot's own `unit_price` rather than the quote values the
 * position at 105_000, which is healthier-looking still. Only exact figures separate either from
 * the right answer.
 *
 * `times: 1` and `assertCount` do different jobs here, as in stock-prices.spec.ts: the retiring
 * stub makes an unwanted second call *fail*, and the count is what proves at the wire that the
 * one call was made — a route that answered from somewhere other than the feed would satisfy
 * every value assertion above it.
 */
test("a snapshot prices the position from the feed and totals it with wallet cash", async ({
  testproxy,
  api,
}) => {
  const acc = await createAccount(api, "Sharesies", "brokerage", "NZD");
  const lot = await api.POST("/api/accounts/{id}/brokerage/holdings", {
    params: { path: { id: acc.id } },
    body: {
      ticker: "MEL",
      exchange: "NZX",
      name: "Meridian Energy",
      currency_code: "NZD",
      trade_date: "2026-07-06",
      quantity: 250,
      unit_price: 4.2,
      fee_minor: 2_50,
      kind: "buy",
    },
  });
  expect(lot.response.status, "record a holding lot").toBe(201);
  // Wallet cash is just a transaction on the account (`wallet_balances_at` sums them per
  // currency), so a brokerage account's cash side needs no separate concept.
  await createTransaction(api, {
    account_id: acc.id,
    posted_at: "2026-07-06",
    amount_minor: 412_90,
    description: "Wallet top up",
  });

  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    // The NZX suffix, anchored: an adapter that asked for the bare `MEL` would match nothing,
    // take the replay miss, and 502 rather than quietly pricing an American ticker of that name.
    path_pattern: "^/v8/finance/chart/MEL\\.NZ$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: chart("NZD", WELLINGTON, [
      ["2026-07-09", 5.5],
      ["2026-07-10", 5.55],
    ]),
    times: 1,
  });

  const { data, response } = await api.GET("/api/accounts/{id}/brokerage", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
  });
  expect(response.status).toBe(200);
  expect(data!.as_of).toBe("2026-07-10");
  expect(data!.currency_code).toBe("NZD");

  expect(data!.positions).toHaveLength(1);
  const pos = data!.positions[0];
  expect(pos.ticker).toBe("MEL");
  expect(pos.quantity).toBe(250);
  // Friday's close, not Thursday's: the day comes from `as_of`, not the feed's first row.
  expect(pos.price).toBe("5.55");
  expect(pos.price_as_of).toBe("2026-07-10");
  // 250 × $5.55 = $1,387.50.
  expect(pos.market_value_minor, "250 shares at the stubbed $5.55 close").toBe(1_387_50);
  // 250 × $4.20 + $2.50 brokerage = $1,052.50. Independent of the feed entirely — it comes off
  // the lot — so a snapshot that mixed the two up shows here rather than only in the total.
  expect(pos.cost_basis_minor).toBe(1_052_50);
  // ($1,387.50 − $1,052.50) / $1,052.50 = 31.83%.
  expect(pos.return_pct!).toBeCloseTo(31.83, 2);

  expect(data!.wallets).toEqual([{ currency_code: "NZD", amount_minor: 412_90 }]);
  // $1,387.50 of shares + $412.90 of cash.
  expect(data!.total_value_minor, "position value plus wallet cash").toBe(1_800_40);
  // A single-currency account needs no exchange rate, so an empty rate table must not make this
  // total partial — `Fx::try_factor` answers `Some(1.0)` for the base currency because that is a
  // real rate rather than a fallback. `rates_as_of` is null for the same reason: none were used.
  expect(data!.unconverted).toEqual([]);
  expect(data!.rates_as_of ?? null).toBeNull();

  const called = await testproxy.assertCount(
    { upstream: "yahoo_finance", path_pattern: "^/v8/finance/chart/MEL\\.NZ$" },
    1,
  );
  expect(called.passed, called.message).toBe(true);
});

/**
 * The multi-upstream case: a US-listed position in an NZD account needs a close from Yahoo *and*
 * a rate from Frankfurter, and the answer is wrong if either is missing or misapplied.
 *
 * Wrong in the silent direction is the failure that actually shipped — `sure_app::tasks::exchange_rates`'s
 * module doc records the poller writing a latest-only cache nothing read, "leaving foreign-currency
 * amounts silently at parity" — so the assertion states the parity figure as well as the right
 * one. It also carries on into `revalue`, because the lasting damage of that bug was not the
 * screen: it was 2,325 parity-converted rows in `valuations`, which feed net worth and carry no
 * hint that they understated anything.
 *
 * Hence a backend of its own with the scheduler ON, and the Frankfurter stub registered *before*
 * it starts: `exchange_rates` has no HTTP writer, so the only arrangement in which the table the
 * poll writes and the table the snapshot reads are provably the same table is a real poll, and
 * the poll fires as the process comes up (see specs/exchange-rates.spec.ts). Yahoo needs no such
 * care — the stock-price sweep runs at boot too, but on a database with no accounts in it yet, so
 * it asks for nothing and the only chart request in this test is the snapshot's.
 */
test("a US position in an NZD account is converted at the polled rate, and that figure is what gets stored", async ({
  testproxy,
}) => {
  // 1 NZD = 0.625 USD, i.e. 1 USD = 1.6 NZD. Exactly representable in binary, so the figures
  // below pin the conversion itself rather than a rounding convention a cent of float drift
  // could move. `times` deliberately unlimited: a failed poll is not recorded, so the scheduler
  // retries on its next check, and a retired stub would turn that retry into a replay-miss WARN.
  await testproxy.stub({
    upstream: "frankfurter",
    method: "GET",
    path_pattern: "^/v1/latest$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ amount: 1, base: "NZD", date: "2026-07-09", rates: { USD: 0.625 } }),
  });

  const server = await startServer(
    { RUST_LOG: "error,sure_app::tasks::exchange_rates=info", BACKGROUND_TASKS: "on" },
    { capture: true },
  );
  try {
    const api = createSureClient(server.baseURL);
    expect(await waitForRate(api, server, "USD")).toEqual({
      base_code: "NZD",
      quote_code: "USD",
      as_of: "2026-07-09",
      rate: "0.625",
    });

    const acc = await createAccount(api, "Sharesies", "brokerage", "NZD");
    const lot = await api.POST("/api/accounts/{id}/brokerage/holdings", {
      params: { path: { id: acc.id } },
      body: {
        ticker: "VOO",
        exchange: "NYSE Arca",
        name: "Vanguard S&P 500",
        currency_code: "USD",
        trade_date: "2026-07-06",
        quantity: 40,
        unit_price: 200,
        kind: "buy",
      },
    });
    expect(lot.response.status, "record a holding lot").toBe(201);
    // A brokerage account legitimately holds cash in several currencies at once, so the wallet
    // side has to convert too — and it is a second, independent call into `Fx`, which is why
    // there is USD cash here as well as a USD position.
    const cash = await api.POST("/api/transactions", {
      body: {
        account_id: acc.id,
        posted_at: "2026-07-06",
        amount_minor: 250_00,
        currency_code: "USD",
        description: "Wallet top up",
      },
    });
    expect(cash.response.status, "record USD wallet cash").toBe(201);

    await testproxy.stub({
      upstream: "yahoo_finance",
      method: "GET",
      // No suffix for a US listing, and that is an assertion: `symbol_for` maps only NZX and ASX.
      path_pattern: "^/v8/finance/chart/VOO$",
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: chart("USD", NEW_YORK, [["2026-07-10", 212.5]]),
      times: 1,
    });

    const snap = await api.GET("/api/accounts/{id}/brokerage", {
      params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
    });
    expect(snap.response.status).toBe(200);

    const pos = snap.data!.positions[0];
    // `Decimal` renders shortest-form, so US$212.50 comes back as "212.5" rather than "212.50".
    expect(pos.price).toBe("212.5");
    expect(pos.currency_code).toBe("USD");
    // 40 × US$212.50 = US$8,500.00 — in the position's *own* currency. The row stays unconverted
    // on purpose (only the roll-up is converted), so a regression that converted twice shows up
    // here as well as in the total.
    expect(pos.market_value_minor).toBe(8_500_00);
    expect(snap.data!.wallets).toEqual([{ currency_code: "USD", amount_minor: 250_00 }]);

    // US$8,500.00 + US$250.00 = US$8,750.00, at 1 USD = 1.6 NZD, is NZ$14,000.00.
    expect(
      snap.data!.total_value_minor,
      "NZ$14,000.00 converted at 1 NZD = 0.625 USD; 875_000 would mean the rate was never " +
        "applied and US$8,750.00 was counted as NZ$8,750.00",
    ).toBe(14_000_00);
    expect(snap.data!.currency_code).toBe("NZD");
    // Nothing was withheld from that total, and it is dated by the rate that made it — the
    // poller writes only on success, so a feed that died months ago leaves stale rates looking
    // exactly like this morning's and only this date can tell.
    expect(snap.data!.unconverted).toEqual([]);
    expect(snap.data!.rates_as_of).toBe("2026-07-09");

    // …and the same number, stored. A `source='brokerage'` row is indistinguishable from a
    // complete figure once written, so this is the assertion the parity bug's 2,325 rows needed.
    const revalued = await api.POST("/api/accounts/{id}/brokerage/revalue", {
      params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
    });
    expect(revalued.response.status).toBe(200);
    expect(revalued.data!.total_value_minor).toBe(14_000_00);
    expect(await valuationsByDay(api, acc.id)).toEqual(
      new Map([
        [
          "2026-07-10",
          expect.objectContaining({
            value_minor: 14_000_00,
            currency_code: "NZD",
            source: "brokerage",
          }),
        ],
      ]),
    );

    // Both upstreams, at the wire. The Yahoo count is 1 across *two* priced requests, which is
    // the price cache doing its job: `revalue` re-snapshotted from the row the snapshot wrote.
    const chartCalls = await testproxy.assertCount(
      { upstream: "yahoo_finance", path_pattern: "^/v8/finance/chart/VOO$" },
      1,
    );
    expect(chartCalls.passed, chartCalls.message).toBe(true);
    // And the poll asked for an NZD-based table, which everything above only assumes: the task
    // stamps `base_code: "NZD"` on whatever came back (`sure_dal::settings::base_currency`), so a
    // request for `?base=USD` would store USD-quoted numbers labelled NZD and the stub would
    // answer it just as happily. Readable here only because Frankfurter declares no volatile
    // query parameters — Yahoo's `period1`/`period2` reach the recorder as `CANONICAL`.
    const rateCalls = await testproxy.queryTraffic({ upstream: "frankfurter" });
    expect(rateCalls.length).toBeGreaterThan(0);
    expect(new Set(rateCalls.map((e) => e.request.uri))).toEqual(new Set(["/v1/latest?base=NZD"]));
  } finally {
    server.stop();
  }
});

// An invented instrument, on NZX so the wire symbol carries the `.NZ` suffix the import path has
// to apply exactly as the on-demand lookup does. Deliberately not brokerage.spec.ts's `ZZTEST`:
// that one is left unstubbed on purpose, and reusing the name would make it unclear which spec a
// chart request belonged to.
const LOOKUP = JSON.stringify({
  "fund-zzfill": { symbol: "ZZFILL", name: "Test Instrument", exchange: "NZX", currency: "nzd" },
});

/**
 * The post-import backfill, which nothing could observe before `assertSeen` existed.
 *
 * `POST /brokerage/import` hands the work to `spawn_backfill` and answers immediately, so the
 * response is no evidence at all that a backfill happened — which is why brokerage.spec.ts asserts
 * the import's ledgers and leaves the backfill alone. That was a gap in what was *expressible*
 * rather than in that spec: there was no moment a test could look at, and sleeping until the work
 * has probably finished is how a flake gets written. `assertSeen` blocks on the proxy side until
 * the outbound price call actually arrives, which is the first observable instant of the task.
 *
 * A count would be the wrong primitive here even though it would also block: how many charts a
 * backfill fetches is `backfill_history`'s own property (one per ticker, pinned by the next test)
 * and has nothing to do with whether the import started it. And the write follows the fetch, so
 * `assertSeen` is a starting gun rather than a finish line — hence `expect.poll` for the
 * valuations themselves.
 */
test("the import's fire-and-forget backfill really does write the history", async ({
  testproxy,
  api,
  server,
}) => {
  const acc = await createAccount(api, "Sharesies", "brokerage", "NZD");

  // Two closes a day apart, so the valuations below differ *from each other* — which is what
  // shows the walk read the cache per day rather than stamping one figure across the range.
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/ZZFILL\\.NZ$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: chart("NZD", WELLINGTON, [
      [day(-12), 2.0],
      [day(-10), 2.5],
    ]),
  });

  const zip = makeZip({
    "sharesies-export/lookup.json": LOOKUP,
    "sharesies-export/wallet-transactions.json": JSON.stringify([
      {
        amount: "500.00",
        currency: "nzd",
        description: "Wallet top up",
        reason: "customer deposit",
        key: "wallet-topup-1",
        timestamp: { $quantum: Date.parse(`${day(-14)}T00:00:00Z`) },
      },
    ]),
    "sharesies-export/activity.json": JSON.stringify([
      {
        id: "order-1",
        type: "buy",
        state: "fulfilled",
        fund_id: "fund-zzfill",
        total_transaction_fee: "0.00",
        trades: [
          {
            contract_note_number: "cn1",
            trade_datetime: { $quantum: Date.parse(`${day(-14)}T00:00:00Z`) },
            share_price: "1.50",
            volume: "100",
          },
        ],
      },
    ]),
  });
  // Through the one import endpoint, naming the source and the account it goes to — the backfill
  // is now the single `FollowUp` an import hands back for the transport to spawn, so what this
  // test really pins is that the handoff still happens.
  const q = new URLSearchParams({ source: "sharesies_zip", assign: `sharesies:${acc.id}` });
  const imported = await fetch(`${server.baseURL}/api/import?${q}`, {
    method: "POST",
    headers: { "Content-Type": "application/zip" },
    body: zip,
  });
  expect(imported.status).toBe(200);
  const holdings = (await imported.json()).items[0].extras.find(
    (x: { kind: string }) => x.kind === "holdings",
  );
  expect(holdings.imported).toBe(1);

  const seen = await testproxy.assertSeen(
    { upstream: "yahoo_finance", path_pattern: "^/v8/finance/chart/ZZFILL\\.NZ$" },
    8_000,
  );
  expect(seen.passed, `the import must start a backfill that reaches the feed: ${seen.message}`).toBe(
    true,
  );

  // 100 shares at whichever close was in force that day, plus the $500.00 of wallet cash the same
  // import wrote. Day −11 resolves to day −12's $2.00 (the nearest preceding close): 100 × $2.00 +
  // $500.00 = $700.00. Day −9 resolves to day −10's $2.50: 100 × $2.50 + $500.00 = $750.00. Two
  // different figures is the assertion — one repeated number would mean the whole range was valued
  // at a single price.
  await expect
    .poll(async () => Object.fromEntries(await valuationsByDay(api, acc.id)), { timeout: 8_000 })
    .toMatchObject({
      [day(-11)]: { value_minor: 700_00, currency_code: "NZD", source: "brokerage" },
      [day(-9)]: { value_minor: 750_00 },
    });
});

/**
 * The manual backfill endpoint: one upstream call per ticker for the whole window, then a
 * valuation for every day since the account's first activity.
 *
 * The count is the point. `backfill_history` bulk-fetches each ticker's full series and then
 * walks the days with `provider=None` precisely "so it never fires one upstream request per day"
 * — and the walk here spans a fortnight, so a regression that passed a provider into the loop
 * would make a call per day per ticker against Yahoo's 500ms throttle: half a minute of backfill
 * for one account that has held two things for two weeks. Two tickers rather than one because
 * "one call per ticker" and "one call per backfill" are indistinguishable with a single holding.
 *
 * `days` is asserted against the rows actually written rather than against a literal, because the
 * far end of the walk is the backend's own today: a length would encode this file's age, and this
 * way the response is checked for telling the truth about how much work it did.
 */
test("a backfill fetches each ticker once and values every day since inception", async ({
  testproxy,
  api,
}) => {
  const acc = await createAccount(api, "Sharesies", "brokerage", "NZD");
  for (const [ticker, quantity, price] of [
    ["ZZFILL", 100, 1.5],
    ["ZZBOND", 50, 3.0],
  ] as const) {
    const lot = await api.POST("/api/accounts/{id}/brokerage/holdings", {
      params: { path: { id: acc.id } },
      body: {
        ticker,
        exchange: "NZX",
        currency_code: "NZD",
        trade_date: day(-14),
        quantity,
        unit_price: price,
        kind: "buy",
      },
    });
    expect(lot.response.status, `record a ${ticker} lot`).toBe(201);
  }
  await createTransaction(api, {
    account_id: acc.id,
    posted_at: day(-14),
    amount_minor: 500_00,
    description: "Wallet top up",
  });

  for (const [ticker, close] of [
    ["ZZFILL", 2.0],
    ["ZZBOND", 4.0],
  ] as const) {
    await testproxy.stub({
      upstream: "yahoo_finance",
      method: "GET",
      path_pattern: `^/v8/finance/chart/${ticker}\\.NZ$`,
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: chart("NZD", WELLINGTON, [[day(-12), close]]),
      times: 1,
    });
  }

  const filled = await api.POST("/api/accounts/{id}/brokerage/backfill", {
    params: { path: { id: acc.id } },
  });
  expect(filled.response.status).toBe(200);

  for (const ticker of ["ZZFILL", "ZZBOND"]) {
    const counted = await testproxy.assertCount(
      { upstream: "yahoo_finance", path_pattern: `^/v8/finance/chart/${ticker}\\.NZ$` },
      1,
    );
    expect(counted.passed, `${ticker}: one bulk fetch, not one per day: ${counted.message}`).toBe(
      true,
    );
  }

  const valuations = await valuationsByDay(api, acc.id);
  expect(valuations.size, "the reported day count must be the rows it wrote").toBe(filled.data!.days);
  // 100 × $2.00 + 50 × $4.00 + $500.00 of wallet cash.
  expect(valuations.get(day(-11))).toMatchObject({
    value_minor: 900_00,
    currency_code: "NZD",
    source: "brokerage",
  });
  // The walk starts at the account's first activity, not at the first day a price exists: day −14
  // predates every close in the cache, so both positions go unpriced there and the day carries
  // the cash alone. That is `backfill_history`'s documented behaviour for a ticker with no quote
  // for a day, and the reason the history is honest about where the account began.
  expect(valuations.get(day(-14))).toMatchObject({ value_minor: 500_00 });
});

/**
 * What a client is told when the price feed is down, on both endpoints that can say it.
 *
 * `AppError::Upstream` exists so "the feed is having a bad minute, this may work later" and
 * "there is a bug in sure and it will fail identically forever" stop sharing one status — opposite
 * retry decisions, and a 500 that means neither. `price_at` is the choke point, so every priced
 * brokerage endpoint inherits it; the `code` is the part a client acts on, and the message is
 * scrubbed because 502 is a 5xx and an upstream's `Display` is third-party text (akahu-client's
 * used to carry the whole response payload), hence the marker assertion.
 *
 * The refused `revalue` writing nothing is the other half: a failed valuation that left a row
 * behind would be a total computed from no prices at all, stored as though it were the account's
 * worth.
 */
test("a failing price feed answers 502 coded upstream, and persists nothing", async ({
  testproxy,
  api,
}) => {
  const acc = await createAccount(api, "Sharesies", "brokerage", "NZD");
  const lot = await api.POST("/api/accounts/{id}/brokerage/holdings", {
    params: { path: { id: acc.id } },
    body: {
      ticker: "MEL",
      exchange: "NZX",
      currency_code: "NZD",
      trade_date: "2026-07-06",
      quantity: 100,
      unit_price: 5,
      kind: "buy",
    },
  });
  expect(lot.response.status).toBe(201);

  // Unlimited, because both requests below have to reach it: the first failed before anything was
  // cached, so the second fetches again rather than finding a warm row.
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/MEL\\.NZ$",
    status: 500,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ finance: { error: { description: "UPSTREAM-ONLY-MARKER" } } }),
  });

  const snapshot = await api.GET("/api/accounts/{id}/brokerage", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
  });
  expect(snapshot.response.status, "a feed outage is not an internal bug").toBe(502);
  const snapshotBody = snapshot.error as { error?: { code?: string; message?: string } };
  expect(snapshotBody?.error?.code).toBe("upstream");
  expect(snapshotBody?.error?.message).toContain("request_id=");
  expect(JSON.stringify(snapshotBody)).not.toContain("UPSTREAM-ONLY-MARKER");

  const revalued = await api.POST("/api/accounts/{id}/brokerage/revalue", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
  });
  expect(revalued.response.status).toBe(502);
  expect((revalued.error as { error?: { code?: string } })?.error?.code).toBe("upstream");

  expect((await valuationsByDay(api, acc.id)).size, "a failed revalue must store nothing").toBe(0);

  // Both requests really did go to the feed. Without this, a route that gave up before opening a
  // socket would produce the same two 502s — the status alone cannot tell the two apart.
  const counted = await testproxy.assertCount(
    { upstream: "yahoo_finance", path_pattern: "^/v8/finance/chart/MEL\\.NZ$" },
    2,
  );
  expect(counted.passed, counted.message).toBe(true);
});
