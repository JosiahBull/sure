import { test, expect, startServer, createSureClient } from "../fixtures";
import { createAccount } from "../helpers";

// `GET /api/accounts/{id}/stock-price` backfills the price cache from Yahoo on a miss, so
// until this suite had a proxy the only coverage was the four 404s below — the paths that
// give up before a socket is opened. Every server here is now pointed at a `sure-testproxy`
// in replay mode with no snapshots (see fixtures.ts), which turns that round: an outbound
// call nobody stubbed is answered `503 {}` and cannot leave the machine, and a call that *is*
// stubbed lets the whole route be asserted on — the close that comes back, the cache that
// stops the second call, and the two ways a quote can be unusable.
//
// Two levers make it deterministic. `?as_of=YYYY-MM-DD` fixes the date the route computes its
// window from, so the request the adapter sends is a pure function of the URL rather than of
// today; and a stub registered `times: 1` retires as it fires, so a second outbound call gets the
// replay miss and the route answers 502.
//
// `times: 1` and `assertCount` do different jobs and both are used below. The retiring stub makes
// an unwanted second call *fail*; the count is what proves it never happened, at the wire, rather
// than inferring it from a status code that a change to the adapter's error handling could make
// lie (it already answers a 404 with "no data" — one more such arm for 5xx and the inference is
// silently worthless).

/**
 * Yahoo's chart shape, built from `[trading day, close]` pairs. A `null` close is what the
 * real feed sends for a non-trading day inside the requested range, and is dropped by
 * `parse_quotes`.
 *
 * `gmtoffset` is the exchange's offset from UTC, and the adapter *adds* it to each timestamp
 * before taking the calendar date — so the timestamps here are placed at local midday
 * (`+ 12h - gmtoffset`), far enough from either boundary that no fixture silently lands a
 * close on the neighbouring day. Only `meta.currency`, `meta.gmtoffset`, `timestamp` and
 * `indicators.quote[0].close` are read — `regularMarketPrice`, which is what a reader expects
 * to matter, is not. Dropping either `meta` field or `indicators` fails to deserialise and
 * arrives as a 502; `timestamp` is the one that may legitimately be absent, and means "no
 * data for this range".
 */
function chart(currency: string, gmtoffset: number, days: Array<[string, number | null]>): string {
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

// The three exchanges quoted below, in their winter offsets. Real values rather than a
// convenient zero: the offset is what decides which calendar day a close is filed under, and
// NZX's +12 is the one far enough from UTC to move it.
const NEW_YORK = -5 * 3600;
const WELLINGTON = 12 * 3600;
const LONDON = 0;

test("a US listing's close and currency come back from the feed", async ({ testproxy, api }) => {
  const acc = await createAccount(api, "Vanguard S&P 500", "shares_us", "USD", {
    metadata: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  });
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    // Anchored on the exact symbol, which is the assertion that `NYSE Arca` adds no suffix:
    // an adapter asking for `VOO.NZ` matches nothing, takes the replay miss, and 502s here.
    path_pattern: "^/v8/finance/chart/VOO$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: chart("USD", NEW_YORK, [
      ["2026-07-09", 210.11],
      ["2026-07-10", 212.34],
    ]),
  });

  const { data, response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
  });
  expect(response.status).toBe(200);
  // Friday's close, not Thursday's: the day comes from `as_of`, not from the feed's first row.
  expect(data?.close).toBe("212.34");
  expect(data?.as_of).toBe("2026-07-10");
  // The currency is the *feed's*, not the account's — the two can disagree, and every amount
  // rendered from this row is denominated by what comes back here.
  expect(data?.currency_code).toBe("USD");
  expect(data?.ticker).toBe("VOO");
  // The cache is keyed on the account's own `exchange` string, not on the Yahoo symbol.
  expect(data?.exchange).toBe("NYSE Arca");
});

/**
 * The property the cache exists for, and the one nothing checked: a second lookup of a date
 * already fetched does not go back to the upstream.
 *
 * Two 200s and one recorded request. The second 200 says `price_at` found a stored row, which is
 * what proves the backfill wrote through rather than handing back an ephemeral value; the count
 * says the row was found *before* the provider was touched. Only the count can say that — a route
 * that called the upstream, got the retired stub's 503 and then fell back to the cache would
 * answer 200 twice and look identical from the API. Yahoo's per-instance throttle (500ms) makes
 * the cost of getting this wrong more than theoretical: it is paid by every panel on the page.
 */
test("a second lookup of the same day is served from the cache, not the feed", async ({
  testproxy,
  api,
}) => {
  const acc = await createAccount(api, "Vanguard S&P 500", "shares_us", "USD", {
    metadata: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  });
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/VOO$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: chart("USD", NEW_YORK, [["2026-07-10", 212.34]]),
    times: 1,
  });

  const ask = () =>
    api.GET("/api/accounts/{id}/stock-price", {
      params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
    });

  const first = await ask();
  expect(first.response.status).toBe(200);
  const second = await ask();
  expect(second.response.status, "the second lookup must not need the upstream").toBe(200);
  // Byte-identical, `fetched_at` included: the stored row came back, not a re-fetch of it.
  expect(second.data).toEqual(first.data);

  const counted = await testproxy.assertCount(
    { upstream: "yahoo_finance", path_pattern: "^/v8/finance/chart/VOO$" },
    1,
  );
  expect(counted.passed, `two lookups must be one outbound call: ${counted.message}`).toBe(true);
});

/**
 * What a user sees on a Sunday. Markets are shut, so there is no close for the date they are
 * looking at, and the answer has to be Friday's — resolved by `as_of <=` in
 * `sure_dal::stock_prices::get_at` over a window that reached back past the weekend.
 *
 * Also the NZX half of the symbol mapping: the stub only answers `MEL.NZ`, so an adapter that
 * dropped the `.NZ` suffix takes the replay miss and fails this test rather than quietly
 * pricing an American ticker of the same name.
 */
test("a Sunday as_of resolves to the preceding Friday's close", async ({ testproxy, api }) => {
  const acc = await createAccount(api, "Meridian Energy", "shares_nz", "NZD", {
    metadata: { profile: "shares", broker: "Sharesies", ticker: "MEL", exchange: "NZX" },
  });
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/MEL\\.NZ$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    // The weekend arrives as two null closes, exactly as the real feed sends it.
    body: chart("NZD", WELLINGTON, [
      ["2026-07-09", 5.42],
      ["2026-07-10", 5.55],
      ["2026-07-11", null],
      ["2026-07-12", null],
    ]),
  });

  const { data, response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-12" } },
  });
  expect(response.status).toBe(200);
  expect(data?.close).toBe("5.55");
  // The row is dated Friday, not the Sunday that was asked for — a caller showing this date
  // beside the price is showing when the market last traded, which is the honest answer.
  expect(data?.as_of).toBe("2026-07-10");
});

/**
 * `BACKFILL_LOOKBACK_DAYS` (10), as it reaches the wire.
 *
 * The window has to span a weekend *and* a holiday cluster or "nearest preceding trading
 * day" finds nothing to fall back on — a Monday-morning lookup after Christmas would answer
 * 404 with the price sitting three days back in the feed. The number is only meaningful at
 * the edge that sends it, and the adapter pads it by another day on each side because Yahoo
 * buckets by the exchange's local trading day.
 *
 * The call itself — that it happened once, at the NZX symbol — comes off the recorder. The two
 * epochs cannot: `sure_testproxy::start` installs `CanonicaliseQuery`, and `partly-proxy-lib`
 * hands the recorder the request *after* every middleware's `redact_request_for_snapshot`
 * (`build_recorded`, its `listener.rs`), so the recorded URI reads
 * `?interval=1d&period1=CANONICAL&period2=CANONICAL`. The values survive only in the backend's
 * own log, via reqwest rendering the URL it was given into the error. If reqwest ever stops
 * naming the URL there, this fails with the captured line in the message, which is the right way
 * for it to go.
 *
 * `packages/providers/tests/{yahoo_finance,akahu}.rs` hit the same wall and build middleware-free
 * clusters to get around it. This suite cannot: it drives a spawned `sure-testproxy`, whose
 * cluster is whatever `start` assembles. Retiring the log-scrape is one line there — install
 * `CanonicaliseQuery` only when `snapshot_dir.is_some()`, since it exists so a *recorded* snapshot
 * outlives the clock that took it, and this suite attaches no snapshot storage at all.
 *
 * And it is the one test here that deliberately stubs *nothing*, so the 502 below is also
 * this suite's no-internet guarantee asserted rather than assumed: the call went to a proxy
 * that dials no upstream in replay mode, was answered `503 {}`, and failed. So one of the
 * `replay miss` WARNs a green run prints is this test's and means nothing is missing —
 * `brokerage.spec.ts` and `shutdown.spec.ts` account for the others.
 */
test("the backfill window spans the weekend and holiday cluster before as_of", async ({
  testproxy,
}) => {
  // Its own backend, for the two epochs alone: the `server` fixture discards the logs, and only a
  // handler that fails logs at all (`err(level = WARN)` on the route).
  const server = await startServer({ RUST_LOG: "sure_api=warn" }, { capture: true });
  try {
    const api = createSureClient(server.baseURL);
    const acc = await createAccount(api, "Meridian Energy", "shares_nz", "NZD", {
      metadata: { profile: "shares", broker: "Sharesies", ticker: "MEL", exchange: "NZX" },
    });
    const res = await api.GET("/api/accounts/{id}/stock-price", {
      params: { path: { id: acc.id }, query: { as_of: "2026-07-12" } },
    });
    // 502 `upstream`, because the proxy's replay miss is a 503 to the adapter and every
    // provider failure now says so rather than claiming an internal bug — see the last test in
    // this file. Worth asserting here too: this is the shape a *missing fixture* takes, so
    // anyone who lands one and sees a 500 knows to look somewhere other than the stub list.
    expect(res.response.status, "an unstubbed upstream must fail, not reach the internet").toBe(
      502,
    );

    // One call, at the suffixed symbol. Both halves are load-bearing: a 502 is what a wrong
    // symbol produces too, so without the path there is nothing here distinguishing "asked for
    // MEL.NZ and nobody answered" from "asked for the wrong thing"; and a backfill that retried
    // internally would burn Yahoo's throttle for every panel on the page while still failing once.
    const called = await testproxy.assertCount(
      { upstream: "yahoo_finance", path_pattern: "^/v8/finance/chart/MEL\\.NZ$" },
      1,
    );
    expect(called.passed, called.message).toBe(true);

    await expect.poll(() => server.output(), { timeout: 5_000 }).toContain("period1");
    const asked = /period1=(\d+)&period2=(\d+)&interval=1d/.exec(server.output());
    expect(asked, `no chart URL in the backend's log:\n${server.output()}`).toBeTruthy();
    const [from, to] = [Number(asked![1]) * 1000, Number(asked![2]) * 1000];

    // as_of 2026-07-12 minus the 10-day lookback is the 2nd, minus the adapter's one-day pad
    // is the 1st; the far end is as_of plus the same pad.
    expect(new Date(from).toISOString()).toBe("2026-07-01T00:00:00.000Z");
    expect(new Date(to).toISOString()).toBe("2026-07-13T00:00:00.000Z");
    // The span on its own, independent of where it sits: both weekends and the 4th of July
    // fall inside it, which is the whole reason the constant is 10 days and not 3.
    expect((to - from) / 86_400_000).toBeGreaterThanOrEqual(10);
  } finally {
    server.stop();
  }
});

/**
 * A quote in a currency `currencies` has no row for is dropped, not fatal.
 *
 * `sure_app::stock_prices::is_unusable_quote` exists because one such quote used to propagate
 * out of the daily sweep: every ticker after it went unpriced, and — a failed run never being
 * recorded — the poll re-ran on every check tick and died on the same quote each time. The
 * route-level half of that contract is what this pins: a ticker Yahoo prices in pence answers
 * "no price" rather than an error, and the *next* ticker on the same server still prices.
 *
 * Pence (`GBX`) is the real case and deliberately not a `currencies` row: that table carries
 * `decimal_places`, so inventing one would render every GBX price as pounds.
 */
test("a quote in an unknown currency is dropped and the next ticker still prices", async ({
  testproxy,
  api,
}) => {
  // Held as `shares_us` because the route admits only that and `shares_nz`; `LSE` is not a
  // suffixed exchange, so the symbol on the wire is the bare ticker.
  const pence = await createAccount(api, "Vodafone", "shares_us", "GBP", {
    metadata: { profile: "shares", broker: "Sharesies", ticker: "VOD", exchange: "LSE" },
  });
  const dollars = await createAccount(api, "Vanguard S&P 500", "shares_us", "USD", {
    metadata: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  });
  // One currency per chart response, because `meta.currency` covers every day in it — which
  // is why "some days unusable" is not a shape this upstream can produce, and the whole
  // ticker is either storable or not.
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/VOD$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: chart("GBX", LONDON, [["2026-07-10", 72.3]]),
  });
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/VOO$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: chart("USD", NEW_YORK, [["2026-07-10", 212.34]]),
  });

  const unusable = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: pence.id }, query: { as_of: "2026-07-10" } },
  });
  expect(unusable.response.status, "an unstorable quote is a missing price, not a fault").toBe(404);
  // Distinguishes this from the four 404s below, which never reached a provider at all.
  expect((unusable.error as { error?: { message?: string } })?.error?.message).toContain(
    "stock price",
  );

  const usable = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: dollars.id }, query: { as_of: "2026-07-10" } },
  });
  expect(usable.response.status, "the unusable ticker must not poison the next one").toBe(200);
  expect(usable.data?.close).toBe("212.34");
});

/**
 * What the route does when Yahoo fails: a 502 coded `upstream`, and none of the upstream's text
 * in it.
 *
 * This used to be `500 internal`, because `price_at` took a bare `?` on the provider's
 * `anyhow::Error` and `AppError::Internal` is `#[from] anyhow::Error`. Two things were wrong
 * with that. A client could not tell "the price feed is having a bad minute, this may work
 * later" from "there is a bug in sure and it will fail the same way forever" — opposite
 * retry decisions behind one status. And every Yahoo blip landed in the logs as a 500, which
 * is the signal `AppError::is_overloaded` already went to the trouble of protecting for the
 * saturated-database case, so that a 500 keeps meaning "a bug to look at".
 *
 * 502 rather than 503 because 503 is taken: it is this server's own "busy, come back" contract,
 * with a `Retry-After` measured in a second. An upstream outage is not that, and a client that
 * backs off identically for both gets one wrong.
 *
 * The body matters as much as the status, and is unchanged: 502 is a 5xx, so the message is
 * still scrubbed. That is deliberate — an upstream's error text is a third-party `Display`, and
 * `akahu-client`'s used to carry the whole response payload with it — so the machine-readable
 * `code` is what a client gets and the detail is for the log. Hence the marker assertion below.
 */
test("an upstream failure answers 502 with nothing of the upstream's own in the body", async ({
  testproxy,
  api,
}) => {
  const acc = await createAccount(api, "Vanguard S&P 500", "shares_us", "USD", {
    metadata: { profile: "shares", broker: "Sharesies", ticker: "VOO", exchange: "NYSE Arca" },
  });
  await testproxy.stub({
    upstream: "yahoo_finance",
    method: "GET",
    path_pattern: "^/v8/finance/chart/VOO$",
    status: 500,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ finance: { error: { description: "UPSTREAM-ONLY-MARKER" } } }),
  });

  const { response, error } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id }, query: { as_of: "2026-07-10" } },
  });
  expect(response.status).toBe(502);
  const body = error as { error?: { code?: string; message?: string } };
  expect(body?.error?.code).toBe("upstream");
  expect(body?.error?.message).toContain("request_id=");
  expect(JSON.stringify(body)).not.toContain("UPSTREAM-ONLY-MARKER");
});

// ---- the paths that give up before a provider is ever reached ----------------------------

test("404s for an unknown account", async ({ api }) => {
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: 999_999 } },
  });
  expect(response.status).toBe(404);
});

test("404s for a non-shares account kind", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(404);
});

test("404s for a shares account with no ticker set", async ({ api }) => {
  // A listed holding can't be *created* without a ticker any more, so the only way to hold
  // one is the provider-link path, which validates in `ValidationMode::Linked` — exactly the
  // state a discovered account is in before its first sync fills anything in.
  const linked = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_no_ticker",
      name: "Akahu — Meridian",
      new_account: {
        name: "Meridian",
        kind: "shares_nz",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(linked.response.status).toBe(201);

  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: linked.data!.account_id } },
  });
  expect(response.status).toBe(404);
});

test("404s for a shares_private account (no market ticker)", async ({ api }) => {
  const acc = await createAccount(api, "Startco Options", "shares_private", "USD", {
    metadata: { profile: "shares", ticker: "N/A" },
  });
  const { response } = await api.GET("/api/accounts/{id}/stock-price", {
    params: { path: { id: acc.id } },
  });
  expect(response.status).toBe(404);
});
