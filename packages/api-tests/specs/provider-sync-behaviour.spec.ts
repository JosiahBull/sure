/**
 * What a provider sync does when the upstream misbehaves, and what a second sync does while the
 * first one is still on the wire.
 *
 * Every guard in this file is about a sync that is *in flight*, which is why none of them has
 * ever been exercised end to end: until this suite had a programmable proxy there was no way to
 * hold an outbound request open, and against an upstream that answers immediately "was the second
 * request inside the first one's window?" is a coin toss. `ProxyClient.pause` and a stub's
 * `delay_ms` are what make the file possible — a paused upstream receives the request and then
 * holds it, so the adapter is genuinely mid-request (bounded only by `sure_providers::http`'s 6s
 * `REQUEST_TIMEOUT`) rather than refused, and the window stops being a coin toss.
 *
 * Akahu is the upstream throughout, with the two invented tokens `packages/providers/tests/akahu.rs`
 * already uses. Every id, name and amount below is invented too (CLAUDE.md rule 3, which bites
 * hardest on this provider: a real bank feed's account numbers and payee names cannot be
 * scrubbed back out of anything afterwards).
 */
import { test, expect, startServer, createSureClient } from "../fixtures";
import type { ProxyClient } from "../proxy-client";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

/**
 * Credentials, injected. The fixture strips `AKAHU_APP_TOKEN`/`AKAHU_USER_TOKEN` from every
 * server it spawns — `specs/akahu.spec.ts` asserts the "not configured" 422 — so a test that
 * wants a sync to actually run has to put them back. Invented, and short enough to be obviously
 * not a token: what reaches the proxy is `X-Akahu-Id` and `Authorization: Bearer …`.
 */
const AKAHU_TOKENS = { AKAHU_APP_TOKEN: "app_token_test", AKAHU_USER_TOKEN: "user_token_test" };

/** The two upstream accounts these tests link. `AccountId::new` only checks the `acc_` prefix. */
const SPENDING = "acc_spend01";
const SAVING = "acc_save01";

/**
 * One settled transaction per id, in Akahu's paginated envelope.
 *
 * The wire shape is the one `packages/providers/tests/akahu.rs` spells out. `cursor.next: null`
 * ends the sweep, so one page is one request — a second would find no stub left and take the
 * proxy's replay-miss 503, which is how a pagination regression would fail these tests rather
 * than quietly passing them.
 */
function transactionsPage(account: string, ids: string[]): string {
  return JSON.stringify({
    success: true,
    items: ids.map((id) => ({
      _id: id,
      _account: account,
      _connection: "conn_bank01",
      created_at: "2026-01-07T02:00:00.000Z",
      date: "2026-01-05T09:30:00.000Z",
      description: "COUNTDOWN GREENLANE",
      amount: -136.55,
      type: "EFTPOS",
    })),
    cursor: { next: null },
  });
}

/** The single-account refetch `sync_provider` makes after importing, for the live balance. */
function accountItem(account: string): string {
  return JSON.stringify({
    success: true,
    item: {
      _id: account,
      _authorisation: "auth_login01",
      connection: { _id: "conn_bank01", name: "ASB", connection_type: "official" },
      name: "Everyday Spending",
      status: "ACTIVE",
      refreshed: {},
      // NZD, matching the local account: a mismatch is refused rather than recorded, and this
      // file is not about that path.
      balance: { current: 2480.15, currency: "NZD" },
      type: "CHECKING",
      attributes: ["TRANSACTIONS"],
    },
  });
}

/**
 * Answer both calls one sync makes, for one linked account.
 *
 * Both patterns are `$`-anchored so `^/v1/accounts/acc_spend01$` cannot also swallow the
 * transactions path, which makes registration order irrelevant here — unlike the paginated case,
 * where two stubs on one path are the only way to give two answers (a matcher never sees the
 * `cursor` query parameter). `times: 1` each, so a sync that was supposed to be refused and ran
 * anyway takes a replay-miss 503 and fails loudly instead of quietly succeeding.
 */
async function stubOneSync(
  testproxy: ProxyClient,
  account: string,
  transactions: string[],
  delayMs?: number,
): Promise<void> {
  const json = { "content-type": "application/json" };
  await testproxy.stub({
    upstream: "akahu",
    method: "GET",
    path_pattern: `^/v1/accounts/${account}/transactions$`,
    status: 200,
    response_headers: json,
    body: transactionsPage(account, transactions),
    delay_ms: delayMs,
    times: 1,
  });
  await testproxy.stub({
    upstream: "akahu",
    method: "GET",
    path_pattern: `^/v1/accounts/${account}$`,
    status: 200,
    response_headers: json,
    body: accountItem(account),
    times: 1,
  });
}

/**
 * A local account with an Akahu provider pointed at `external`.
 *
 * `POST /api/providers` rather than `POST /api/providers/link`, deliberately: linking fires an
 * immediate best-effort sync, which would consume the stubs these tests are about to time
 * precisely.
 */
async function akahuProvider(api: SureClient, name: string, external: string) {
  const account = await createAccount(api, name, "bank");
  const { data, response } = await api.POST("/api/providers", {
    body: {
      name: `Akahu — ${name}`,
      kind: "akahu",
      account_id: account.id,
      enabled: true,
      config: { external_account_id: external },
    },
  });
  expect(response.status, "create provider").toBe(201);
  return data!;
}

const sync = (api: SureClient, id: number) =>
  api.POST("/api/providers/{id}/sync", { params: { path: { id } }, body: {} });

const syncs = (api: SureClient, id: number) =>
  api.GET("/api/providers/{id}/syncs", { params: { path: { id } } });

/**
 * The one INFO line `ShutdownReport::record` emits, with tracing's colour codes stripped.
 *
 * Returned whole rather than parsed into fields: the last test asserts on three of them, and the
 * line itself is the better failure message. `specs/shutdown.spec.ts` has the field-by-field
 * version, and owns the vocabulary.
 */
function shutdownReport(output: string): string {
  // The ESC goes with the sequence: left in, it would sit between a field name and its `=` and
  // be exactly as unmatchable as the whole escape.
  const plain = output.replace(/\x1B\[[0-9;]*m/gu, "");
  const line = plain.split("\n").find((l) => l.includes("shutdown complete"));
  expect(line, `no shutdown report in output:\n${plain}`).toBeDefined();
  return line!;
}

test("a second sync of one provider while the first is on the wire is refused, not run twice", async ({
  testproxy,
}) => {
  // Why refusing beats running: two syncs of one provider mean double the outbound requests to
  // an upstream whose rate limits are per household, two write transactions contending for
  // SQLite's single writer, and a second run whose only product is `skipped` counts. Nothing has
  // ever tested the guard.
  await stubOneSync(testproxy, SPENDING, ["trans_9001"]);

  const server = await startServer(AKAHU_TOKENS);
  try {
    const api = createSureClient(server.baseURL);
    const provider = await akahuProvider(api, "Everyday", SPENDING);

    // Held, not refused — and that is the synchronisation point as well as the scenario. While
    // the upstream is paused nothing that reaches it can answer, so the *first* of the two calls
    // to settle is necessarily the one that never got there: the refusal. No sleep, and no
    // assumption about which request reached the guard first.
    await testproxy.pause("akahu");
    const attempts = [sync(api, provider.id), sync(api, provider.id)];
    const refused = await Promise.race(attempts);
    expect(
      refused.response.status,
      `expected a 409 while the first sync was in flight, got ${JSON.stringify(refused.error)}`,
    ).toBe(409);
    // From the guard, and saying so. A 409 could also be an optimistic-concurrency conflict from
    // somewhere else on the request path, and the two would want opposite fixes.
    expect(JSON.stringify(refused.error)).toContain("already running");

    await testproxy.resume("akahu");
    const settled = (await Promise.all(attempts)).map((a) => a.response.status).sort((a, b) => a - b);
    expect(settled).toEqual([200, 409]);

    // One run on the record, not two — a 409 is not a failed sync, it is a sync that did not
    // happen.
    const recorded = await syncs(api, provider.id);
    expect(recorded.data!.length).toBe(1);
    expect(recorded.data![0]).toMatchObject({ status: "ok", imported: 1 });

    // And one sweep on the wire, which is the harm the guard was added for rather than a proxy for
    // it: Akahu's rate limits are per household, so the cost of a lost guard is paid upstream
    // before it is ever visible in the sync history. Counted here rather than inferred from the
    // row above, because the two can disagree — a second run that reached the upstream and then
    // failed to record is exactly the shape that would make the count 2 and the history still 1.
    const swept = await testproxy.assertCount(
      { upstream: "akahu", path_pattern: `^/v1/accounts/${SPENDING}/transactions$` },
      1,
    );
    expect(swept.passed, swept.message).toBe(true);
  } finally {
    server.stop();
  }
});

test("two different providers sync at the same time — the guard is per provider, not one global lock", async ({
  testproxy,
}) => {
  // A regression to a single lock would be all but invisible: the 6-hourly poll walks providers
  // sequentially, so the only thing that would notice is a household with two feeds and a human
  // pressing "Sync now" during the sweep — which is the collision the guard was added for in the
  // first place.
  //
  // `delay_ms` rather than `pause` here. The window in which a global lock is observable is
  // exactly the time the first sync spends waiting on its upstream, and a delay makes that
  // window a known length; pausing would need a `resume` timed against the moment the second
  // request reached the guard, which nothing outside the process can observe. Guessing at it is
  // how a suite acquires a test that passes nine times in ten.
  const HELD_MS = 1_500;
  await stubOneSync(testproxy, SPENDING, ["trans_9001"], HELD_MS);
  await stubOneSync(testproxy, SAVING, ["trans_9101"], HELD_MS);

  const server = await startServer(AKAHU_TOKENS);
  try {
    const api = createSureClient(server.baseURL);
    const spending = await akahuProvider(api, "Everyday", SPENDING);
    const saving = await akahuProvider(api, "Savings", SAVING);

    const [first, second] = await Promise.all([sync(api, spending.id), sync(api, saving.id)]);

    expect(first.response.status, JSON.stringify(first.error)).toBe(200);
    expect(second.response.status, JSON.stringify(second.error)).toBe(200);
    // Both actually ran: a refused sync would have imported nothing, and each provider's page
    // carries exactly one transaction of its own.
    expect(first.data).toMatchObject({ status: "ok", imported: 1 });
    expect(second.data).toMatchObject({ status: "ok", imported: 1 });
  } finally {
    server.stop();
  }
});

test("a sync that fails hands its slot back, so the next one is accepted rather than refused", async ({
  testproxy,
}) => {
  // `SyncSlot` returns the slot from `Drop`, on every exit path — which is the whole reason the
  // type exists. A plain `remove` at the end of `sync_provider` would leak the id on the early
  // `return Err` a failed fetch takes, and this provider would then answer 409 to every sync for
  // the rest of the process's life: one upstream outage, and "Sync now" is dead until a restart,
  // reporting that a sync is already running when none is.
  await testproxy.stub({
    upstream: "akahu",
    method: "GET",
    path_pattern: `^/v1/accounts/${SPENDING}/transactions$`,
    status: 500,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ success: false, message: "the upstream is having a bad day" }),
    times: 1,
  });

  const server = await startServer(AKAHU_TOKENS);
  try {
    const api = createSureClient(server.baseURL);
    const provider = await akahuProvider(api, "Everyday", SPENDING);

    const failed = await sync(api, provider.id);
    expect(failed.response.status, "an upstream 500 is a failed sync").toBe(422);

    // The same provider, immediately, with a healthy upstream. The 500 stub has retired, so
    // these two answer instead. A 409 here is the leak.
    await stubOneSync(testproxy, SPENDING, ["trans_9001"]);
    const retried = await sync(api, provider.id);
    expect(retried.response.status, "the failed sync did not release its single-flight slot").toBe(200);
    expect(retried.data).toMatchObject({ status: "ok", imported: 1 });

    // Both attempts are on the record, newest first — the failure durably, which is what puts it
    // in the UI rather than only in a log line nobody reads.
    const recorded = await syncs(api, provider.id);
    expect(recorded.data!.map((s) => s.status)).toEqual(["ok", "error"]);
  } finally {
    server.stop();
  }
});

/**
 * A second sync straight after a successful one is answered from the record, not from Akahu.
 *
 * The gap the single-flight guard above cannot close. It refuses a *concurrent* second sync;
 * it has nothing to say about a sequential one, because by then the first has finished and
 * given its slot back. Twenty clicks of "Sync now" were therefore twenty complete paginated
 * sweeps of somebody else's API — against limits that are per *household*, so the cost lands
 * on the one person whose data it is — and not one of them could find anything the first did
 * not: Akahu refreshes an official bank connection a few times a day.
 *
 * `fresh: false` is what makes this safe to show. Returning the previous run unmarked would
 * have the UI report "Imported 1" a second time for an import that never happened.
 */
test("a second sync inside the cooldown replays the last run instead of sweeping again", async ({
  testproxy,
}) => {
  // One sweep's worth of stubs, and `times: 1` on each: a second sweep would find nothing left
  // to answer it and take the proxy's replay-miss 503, so a lost cooldown fails here loudly
  // rather than by an off-by-one in a count.
  await stubOneSync(testproxy, SPENDING, ["trans_9001"]);

  const server = await startServer({ ...AKAHU_TOKENS, PROVIDER_SYNC_COOLDOWN_SECS: "60" });
  try {
    const api = createSureClient(server.baseURL);
    const provider = await akahuProvider(api, "Everyday", SPENDING);

    const first = await sync(api, provider.id);
    expect(first.response.status, JSON.stringify(first.error)).toBe(200);
    expect(first.data).toMatchObject({ status: "ok", imported: 1, fresh: true });

    // Sequential, not concurrent — awaited above, so the slot is already back.
    const replayed = await sync(api, provider.id);
    expect(replayed.response.status, "a cooled-down sync is not an error").toBe(200);
    expect(replayed.data).toMatchObject({ status: "ok", imported: 1, fresh: false });
    // The *same row*, not a new one that happens to carry the same counts.
    expect(replayed.data!.id).toBe(first.data!.id);

    // One run on the record: a replay is not a sync, so it must not appear in the history as
    // one. A user reading this list would otherwise see runs that never happened.
    const recorded = await syncs(api, provider.id);
    expect(recorded.data!.length).toBe(1);

    // And the harm the guard exists to prevent, counted where it is actually paid.
    const swept = await testproxy.assertCount(
      { upstream: "akahu", path_pattern: `^/v1/accounts/${SPENDING}/transactions$` },
      1,
    );
    expect(swept.passed, swept.message).toBe(true);
  } finally {
    server.stop();
  }
});

/**
 * …and once it expires, the next sync really does go out.
 *
 * The half that a cooldown test is worthless without: a guard that never lets go is
 * indistinguishable from one that works, right up until someone's bank feed has been frozen
 * since the process started. This is also the reason the window is configuration rather than a
 * constant — at the production 60s this test would be a minute of wall clock, and a suite pays
 * that on every run forever.
 */
test("once the cooldown expires the next sync reaches the upstream again", async ({ testproxy }) => {
  const COOLDOWN_SECS = 1;
  // Two sweeps' worth, each stub firing once. The second sync can only succeed by reaching the
  // upstream, and can only reach it if the window really elapsed.
  await stubOneSync(testproxy, SPENDING, ["trans_9001"]);
  await stubOneSync(testproxy, SPENDING, ["trans_9002"]);

  const server = await startServer({
    ...AKAHU_TOKENS,
    PROVIDER_SYNC_COOLDOWN_SECS: String(COOLDOWN_SECS),
  });
  try {
    const api = createSureClient(server.baseURL);
    const provider = await akahuProvider(api, "Everyday", SPENDING);

    const first = await sync(api, provider.id);
    expect(first.data).toMatchObject({ status: "ok", imported: 1, fresh: true });

    // Comfortably past the window, and bounded by it: the cooldown is measured against
    // `providers.last_synced_at` on the server's own clock, so there is nothing here to race.
    await new Promise((r) => setTimeout(r, (COOLDOWN_SECS + 1) * 1000));

    const second = await sync(api, provider.id);
    expect(second.response.status, JSON.stringify(second.error)).toBe(200);
    expect(second.data, "the expired cooldown still refused a real sync").toMatchObject({
      fresh: true,
      status: "ok",
      imported: 1,
    });
    expect(second.data!.id).not.toBe(first.data!.id);

    const recorded = await syncs(api, provider.id);
    expect(recorded.data!.length).toBe(2);

    const swept = await testproxy.assertCount(
      { upstream: "akahu", path_pattern: `^/v1/accounts/${SPENDING}/transactions$` },
      2,
    );
    expect(swept.passed, swept.message).toBe(true);
  } finally {
    server.stop();
  }
});

/**
 * A *failed* sync is not rate limited: the retry after an outage works immediately.
 *
 * The cooldown is keyed on `providers.last_synced_at`, which `sync_provider` only advances on
 * the success path — so this falls out of where one line sits rather than from a second check,
 * and that is exactly why it is worth pinning. Move `update_last_synced` onto the error path
 * and every transient upstream failure would lock the button for a minute, at the one moment a
 * user is most likely to press it again.
 */
test("a failed sync is not rate limited, so the retry after an outage runs immediately", async ({
  testproxy,
}) => {
  await testproxy.stub({
    upstream: "akahu",
    method: "GET",
    path_pattern: `^/v1/accounts/${SPENDING}/transactions$`,
    status: 500,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ success: false, message: "the upstream is having a bad day" }),
    times: 1,
  });

  const server = await startServer({ ...AKAHU_TOKENS, PROVIDER_SYNC_COOLDOWN_SECS: "3600" });
  try {
    const api = createSureClient(server.baseURL);
    const provider = await akahuProvider(api, "Everyday", SPENDING);

    const failed = await sync(api, provider.id);
    expect(failed.response.status, "an upstream 500 is a failed sync").toBe(422);

    // An hour-long cooldown, and this still has to go out — because the failure above never
    // moved the watermark.
    await stubOneSync(testproxy, SPENDING, ["trans_9001"]);
    const retried = await sync(api, provider.id);
    expect(retried.response.status, "a failed sync must not start the cooldown").toBe(200);
    expect(retried.data).toMatchObject({ status: "ok", imported: 1, fresh: true });
  } finally {
    server.stop();
  }
});

test("SIGTERM with a sync stalled on its upstream still drains cleanly", async ({ testproxy }) => {
  // The case `sure_providers::http`'s `REQUEST_TIMEOUT` comment is about: 6s, chosen to stay
  // "well under the 10s drain grace, so the scheduler always finishes the task it is on".
  // `reqwest` sets no timeout of any kind by default, so without that ceiling an upstream which
  // accepts a connection and then goes quiet holds the request — and the drain waiting on it —
  // until the grace expires and the work is abandoned mid-write. Every existing shutdown test
  // drains requests that are only waiting on *us*; this one drains a request whose completion is
  // in somebody else's hands.
  //
  // A manual sync rather than the scheduler's provider poll, which is what a production stall
  // would most likely be: the poll only has a provider to visit if one existed before the
  // process started, and the scheduler's next check is 60s away — longer than any test's
  // timeout. The manual route reaches the same adapter through the same bounded client, inside a
  // request the connection drain has to resolve.
  test.setTimeout(40_000);

  await stubOneSync(testproxy, SPENDING, []);
  const server = await startServer(
    { ...AKAHU_TOKENS, RUST_LOG: "error,sure_appbase=info" },
    { capture: true },
  );
  try {
    const api = createSureClient(server.baseURL);
    const provider = await akahuProvider(api, "Everyday", SPENDING);
    await testproxy.pause("akahu");

    const stalled = sync(api, provider.id);
    // The signal has to land while the request is being *served*: one that arrives after the
    // accept loop stops is refused, and this test would then assert nothing. The client already
    // has a keep-alive connection to this server (the two calls above), so the POST is on the
    // wire in a millisecond and this is slack, not a race — and the 422 below is what proves it,
    // rather than the sleep.
    await new Promise((r) => setTimeout(r, 300));
    const signalledAt = Date.now();
    server.proc.kill("SIGTERM");

    // Accepted, run through the adapter to its 6s ceiling, and answered — all after the signal.
    const answered = await stalled;
    expect(answered.response.status, "the sync was not in flight when the signal landed").toBe(422);

    const code = await server.waitForExit(25_000);
    const report = shutdownReport(server.output());
    expect(code, report).toBe(0);
    expect(report).toMatch(/\bdrain="drained"/);
    expect(report).toMatch(/\babandoned=0\b/);
    expect(report).toMatch(/\bclean=true\b/);
    // The relationship the ceiling exists to keep, measured: a stalled upstream costs one
    // `REQUEST_TIMEOUT` and nothing more, so it cannot outlive either grace waiting on it —
    // 15s for the connection drain inside `serve`, then 10s for the tracked tasks
    // (`docs/HTTP.md`). Bounded with room, because this is an assertion about two constants
    // rather than about how loaded the machine is; take the timeout away and the request hangs
    // until a grace expires instead, failing here *and* on `clean=true` above.
    expect(Date.now() - signalledAt).toBeLessThan(12_000);
  } finally {
    server.stop();
  }
});
