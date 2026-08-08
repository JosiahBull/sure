import { test, expect, startServer, createSureClient, type StartedServer } from "../fixtures";
import type { ProxyClient } from "../proxy-client";
import type { SureClient } from "../../client/src/index";
import { createAccount } from "../helpers";

// Akahu is the one upstream this repo will not hold a recording of at all — a real bank feed's
// account numbers, balances and payee names cannot be scrubbed back out of a snapshot after the
// fact (see `scripts/pii-scan.mjs`'s AKAHU_SNAPSHOT_PATH). So every response here is a stub
// registered by the test that needs it, served by the `sure-testproxy` every backend in this
// suite is pointed at, in replay mode with no snapshot storage: `https://api.akahu.io` is
// unreachable from this file even by mistake, and nothing is written to disk.
//
// That, plus credentials now being a *value* the composition root passes in rather than
// something read from the environment on every request, is what makes the success path
// testable: two invented tokens on a backend this spec spawns itself. Before, only the "not
// configured" paths could be covered, and they are all still here — a fresh checkout, and every
// CI run, is an unconfigured install, and the 422 that names the variable to set is how a user
// finds out what to do.
//
// What this file is *about* is the far side of the adapter: what reaches the ledger, the
// valuations and the sync history. The adapter's own wire behaviour — which URL, which headers,
// which window, whether the cursor loop follows a page cursor — is pinned in-process, and much
// more cheaply, by `packages/providers/tests/akahu.rs`.
//
// One property crosses that line, because only this side can claim it: that the *stored*
// watermark reaches the query. `akahu.rs` hands `SyncContext.last_synced_at` in by hand, so it
// cannot see `sure_app::sync` writing a timestamp the adapter's `parse_from_rfc3339(..).ok()`
// then drops on the floor — which would re-sweep the account's whole history on every six-hourly
// poll, forever, with no error anywhere and no symptom but a slow sync. The re-sync test below
// asserts the window is there. Its three-day *width* is not readable from TypeScript:
// `sure_testproxy::start` installs `CanonicaliseQuery` and `partly-proxy-lib` hands the recorder
// the request after every middleware's `redact_request_for_snapshot`, so the recorded `?start=`
// reads `CANONICAL`. `akahu.rs`'s `an_incremental_sync_asks_from_three_days_before_the_last_one`
// is where the three days are pinned, against a middleware-free cluster.

/**
 * The linked Akahu account every fixture below belongs to.
 *
 * Invented, like every other identifier here (CLAUDE.md rule 3, which bites hardest on this
 * provider). The `acc_` prefix is not decoration: `AccountId::new` rejects anything else, so a
 * config carrying a bare id fails the sync before a socket is opened.
 */
const EXTERNAL_ID = "acc_spend01";

/** Akahu's connection id for that account, and the institution it reports for it. */
const CONNECTION_ID = "conn_asb01";
const INSTITUTION = "ASB";

/**
 * `sure_app::sync::MAX_SYNC_DETAIL_CHARS`, restated because a spec cannot read a Rust const.
 *
 * If the two ever disagree the cap test below fails on the length, which is the right place to
 * find out: the number is a promise about what `GET /api/providers/{id}/syncs` may hand back.
 */
const MAX_SYNC_DETAIL_CHARS = 500;

/**
 * A backend that *is* configured for Akahu.
 *
 * The shared `server` fixture deliberately never is: it strips `AKAHU_APP_TOKEN` /
 * `AKAHU_USER_TOKEN` out of the environment and blanks `SURE_ENV_FILE`, so the "not configured"
 * tests hold regardless of what a developer has exported or keeps in a repo-root `.env`. A test
 * that wants a sync to succeed spawns its own and supplies the two invented tokens
 * `packages/providers/tests/akahu.rs` already uses — a real token in a fixture is a leak, not a
 * shortcut, and these two never leave loopback.
 */
async function configured(): Promise<{ server: StartedServer; api: SureClient }> {
  const server = await startServer({
    AKAHU_APP_TOKEN: "app_token_test",
    AKAHU_USER_TOKEN: "user_token_test",
  });
  return { server, api: createSureClient(server.baseURL) };
}

/** One settled transaction, as Akahu sends it. */
function txn(opts: {
  id: string;
  date: string;
  description: string;
  /** Major units, the way the feed quotes them; negative is money out. */
  amount: number;
  /** When set, the row also carries the enrichment engine's merchant and category. */
  merchant?: string;
}) {
  return {
    _id: opts.id,
    _account: EXTERNAL_ID,
    _connection: CONNECTION_ID,
    // When Akahu first saw the row, which is unrelated to when it was posted.
    created_at: "2026-01-08T10:00:00.000Z",
    date: opts.date,
    description: opts.description,
    amount: opts.amount,
    // Akahu falls back to these two when it can't name the transaction type.
    type: opts.amount < 0 ? "DEBIT" : "CREDIT",
    // `merchant` and `category` are siblings of the transaction on the wire — both fields of
    // one flattened `enriched_data` — so they arrive together or not at all. The category name
    // and group are NZFCC values, not free text: `akahu-client` parses them into an enum, so an
    // invented one would fail the whole page rather than one field.
    ...(opts.merchant
      ? {
          merchant: { _id: "merchant_e2e01", name: opts.merchant },
          category: {
            _id: "nzfcc_e2e01",
            name: "Supermarkets and grocery stores",
            groups: { personal_finance: { _id: "group_e2e01", name: "Food" } },
          },
        }
      : {}),
  };
}

/** The account object, as `GET /accounts` and `GET /accounts/{id}` both carry it. */
function account(opts: {
  /** Major units. Negative for anything owed. */
  balance: number;
  currency?: string;
  kind?: string;
  name?: string;
  /** An ongoing credit limit, which is what distinguishes a revolving facility from a loan. */
  limit?: number;
}) {
  return {
    _id: EXTERNAL_ID,
    // Per *login*, not per institution: what tells two household members' ASB accounts apart.
    _authorisation: "auth_e2e01",
    connection: { _id: CONNECTION_ID, name: INSTITUTION, connection_type: "official" },
    name: opts.name ?? "Everyday",
    status: "ACTIVE",
    refreshed: {},
    balance: {
      current: opts.balance,
      currency: opts.currency ?? "NZD",
      ...(opts.limit === undefined ? {} : { limit: opts.limit }),
    },
    type: opts.kind ?? "CHECKING",
    attributes: ["TRANSACTIONS"],
  };
}

/**
 * Answer the two calls one sync makes: the paginated transaction sweep, then the single-account
 * refetch `current_balance` uses.
 *
 * Both matchers are anchored, which is the only thing keeping them apart — `/v1/accounts/{id}`
 * is a prefix of `/v1/accounts/{id}/transactions`, and an unanchored pattern would answer the
 * sweep with a balance. Neither carries `times`, so a re-sync gets the same answers: what
 * changes between two syncs is the `?start=` in the query, which a matcher is never shown —
 * {@link askedFrom} reads it back off the recorder instead.
 *
 * Always one page, `cursor.next: null`. Two pages need two stubs a matcher cannot tell apart,
 * which works but says nothing new here: the cursor loop is pinned in
 * `packages/providers/tests/akahu.rs`, where it costs no server and no database.
 */
async function stubSync(
  testproxy: ProxyClient,
  fixtures: { items: ReturnType<typeof txn>[]; balance?: ReturnType<typeof account> },
): Promise<void> {
  await testproxy.stub({
    upstream: "akahu",
    method: "GET",
    path_pattern: `^/v1/accounts/${EXTERNAL_ID}/transactions$`,
    status: 200,
    response_headers: { "content-type": "application/json" },
    body: JSON.stringify({ success: true, items: fixtures.items, cursor: { next: null } }),
  });
  if (fixtures.balance) {
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: `^/v1/accounts/${EXTERNAL_ID}$`,
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({ success: true, item: fixtures.balance }),
    });
  }
}

/**
 * The `start` a recorded sweep asked from, or `null` for "everything since the account opened".
 *
 * A recorded URI is origin-form, so parsing one needs a base; the invalid host is never resolved.
 * `URLSearchParams` rather than a substring of the raw URI because `akahu-client` builds the query
 * with `Url::parse_with_params`, which percent-encodes the `:`s in an RFC 3339 timestamp — a
 * substring match would have to know that, and would keep passing if it stopped being true.
 */
function askedFrom(uri: string): string | null {
  return new URL(uri, "http://recorded.invalid").searchParams.get("start");
}

/** Create a provider against `accountId`, linked to {@link EXTERNAL_ID}. */
async function akahuProvider(api: SureClient, accountId: number) {
  const { data, response } = await api.POST("/api/providers", {
    body: {
      name: "Akahu — Everyday",
      kind: "akahu",
      account_id: accountId,
      enabled: true,
      config: { external_account_id: EXTERNAL_ID },
    },
  });
  expect(response.status, "create provider").toBe(201);
  return data!;
}

test("an akahu sync imports the feed's transactions and advances the watermark", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    const acc = await createAccount(api, "Everyday", "bank");
    const provider = await akahuProvider(api, acc.id);
    expect(provider.last_synced_at, "nothing has synced yet").toBeFalsy();

    await stubSync(testproxy, {
      items: [
        txn({ id: "trans_e2e01", date: "2026-01-05T09:30:00.000Z", description: "Coffee", amount: -4.5 }),
        txn({
          id: "trans_e2e02",
          date: "2026-01-06T18:12:00.000Z",
          description: "COUNTDOWN ONLINE",
          amount: -82.35,
          merchant: "Countdown",
        }),
        txn({ id: "trans_e2e03", date: "2026-01-07T00:00:00.000Z", description: "Salary", amount: 2500 }),
      ],
      balance: account({ balance: 1234.56 }),
    });

    const synced = await api.POST("/api/providers/{id}/sync", {
      params: { path: { id: provider.id } },
      body: {},
    });
    expect(synced.response.status).toBe(200);
    expect(synced.data?.imported).toBe(3);
    expect(synced.data?.skipped).toBe(0);
    expect(synced.data?.status).toBe("ok");
    // `detail` is where a refusal would surface (a balance in the wrong currency, say); a clean
    // sync leaves it empty, so asserting that is what makes the `ok` above mean "nothing to say".
    expect(synced.data?.detail).toBeFalsy();

    const txns = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
    expect(txns.data?.length).toBe(3);
    // Signed minor units, both directions: the feed quotes major units, and a sign or a factor
    // of 100 lost here is a ledger that silently disagrees with the bank.
    const salary = txns.data?.find((t) => t.description === "Salary");
    expect(salary?.amount_minor).toBe(250_000);
    expect(txns.data?.find((t) => t.description === "Coffee")?.amount_minor).toBe(-450);
    const groceries = txns.data?.find((t) => t.description === "COUNTDOWN ONLINE");
    expect(groceries?.amount_minor).toBe(-8_235);
    // The whole timestamp reaches the ledger, not just the day — the row shows a time of day,
    // and only the calendar day is range-checked on the way in.
    expect(salary?.posted_at).toBe("2026-01-07T00:00:00+00:00");
    expect(groceries?.posted_at).toBe("2026-01-06T18:12:00+00:00");
    // Akahu's enrichment becomes a first-class Merchant, the same as the CSV importer's
    // merchant column (`specs/providers.spec.ts`), so rules and reports can reuse it.
    expect(groceries?.merchant).toBe("Countdown");
    const merchants = await api.GET("/api/merchants", {});
    expect(merchants.data?.filter((m) => m.name === "Countdown").length).toBe(1);

    // The watermark advanced, which is what makes the next sync incremental rather than a
    // full re-sweep: `SyncContext.last_synced_at` is where `?start=` comes from.
    const providers = await api.GET("/api/providers", {});
    expect(providers.data?.find((p) => p.id === provider.id)?.last_synced_at).toBeTruthy();
  } finally {
    server.stop();
  }
});

/**
 * The sibling of "CSV sync imports then dedupes on re-sync" (`specs/providers.spec.ts`), for
 * the provider where it matters most: Akahu deliberately re-fetches a three-day overlap on
 * every sync, because a NZ bank can shift a transaction's settlement date as the data trickles
 * in. Every one of those rows arrives a second time, so dedupe on `external_id` is not a
 * nicety here — without it a six-hourly poll would triple-count three days of spending
 * indefinitely.
 */
test("akahu sync imports, then dedupes the overlap it deliberately re-fetches", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    const acc = await createAccount(api, "Everyday", "bank");
    const provider = await akahuProvider(api, acc.id);
    await stubSync(testproxy, {
      items: [
        txn({ id: "trans_e2e01", date: "2026-01-05T09:30:00.000Z", description: "Coffee", amount: -4.5 }),
        txn({ id: "trans_e2e02", date: "2026-01-06T18:12:00.000Z", description: "Groceries", amount: -82.35 }),
        txn({ id: "trans_e2e03", date: "2026-01-07T00:00:00.000Z", description: "Salary", amount: 2500 }),
      ],
      balance: account({ balance: 1234.56 }),
    });

    const sync = () =>
      api.POST("/api/providers/{id}/sync", { params: { path: { id: provider.id } }, body: {} });

    const first = await sync();
    expect(first.data?.imported).toBe(3);
    expect(first.data?.skipped).toBe(0);

    const second = await sync();
    // Not "imported 0, skipped 0": the second sweep really did fetch the same three rows and
    // recognise each one, which is the difference between dedupe working and the fetch quietly
    // returning nothing.
    expect(second.data?.imported).toBe(0);
    expect(second.data?.skipped).toBe(3);
    expect(second.data?.status).toBe("ok");

    const txns = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
    expect(txns.data?.length).toBe(3);

    // Two sweeps went out, and the second one was *windowed*. `skipped: 3` cannot tell a windowed
    // re-fetch from one that asked for all history again and happened to get the same page back —
    // and those two diverge as the account ages: the second reads years of transactions every six
    // hours to import nothing. Only the first sync may have no `start`; the second's proves the
    // watermark survived the round trip through SQLite and back into a query.
    const sweeps = await testproxy.queryTraffic({
      upstream: "akahu",
      path_pattern: `^/v1/accounts/${EXTERNAL_ID}/transactions$`,
    });
    expect(sweeps.length, "the re-sync must reach the upstream, not short-circuit").toBe(2);
    expect(askedFrom(sweeps[0]!.request.uri), "a first sync asks for everything").toBeNull();
    // Present, not compared: `CanonicaliseQuery` has already rewritten the value to `CANONICAL` by
    // the time the recorder sees it (see the file comment). When that stops being true, this is
    // the assertion to tighten to `last_synced_at` minus three days.
    expect(
      askedFrom(sweeps[1]!.request.uri),
      "the re-sync dropped the watermark and re-swept all history",
    ).not.toBeNull();

    // Both attempts are durably recorded — the history is how a user sees that a poll ran at
    // all on a day it imported nothing.
    const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider.id } } });
    expect(syncs.data?.length).toBe(2);
    expect(syncs.data?.every((s) => s.status === "ok")).toBe(true);
  } finally {
    server.stop();
  }
});

/**
 * Discovery → link → first sync, which is the entire path a user walks the first time they
 * connect a bank, and the only one where the three pieces have to agree about an `external_id`
 * nobody typed.
 *
 * The link route triggers the initial sync itself rather than leaving the account empty until
 * the next poll, so a successful link is also a populated account — and the sync is what fills
 * in what the connect dialog could not: the institution behind the connection.
 */
test("discovering, linking and the sync the link triggers populate the new account", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: "^/v1/accounts$",
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({ success: true, items: [account({ balance: 1234.56 })] }),
    });
    await stubSync(testproxy, {
      items: [
        txn({ id: "trans_e2e01", date: "2026-01-06T18:12:00.000Z", description: "Groceries", amount: -82.35 }),
      ],
      balance: account({ balance: 1234.56 }),
    });

    const discovered = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: "akahu" } },
    });
    expect(discovered.response.status).toBe(200);
    expect(discovered.data?.length).toBe(1);
    const offered = discovered.data![0];
    expect(offered.external_id).toBe(EXTERNAL_ID);
    // A suggestion the user confirms, not a decision: `CHECKING` maps to a plain bank account.
    expect(offered.kind_hint).toBe("bank");
    expect(offered.balance_minor).toBe(123_456);
    expect(offered.supports_transactions).toBe(true);

    const linked = await api.POST("/api/providers/link", {
      body: {
        kind: "akahu",
        external_id: offered.external_id,
        name: `Akahu — ${offered.name}`,
        new_account: {
          name: offered.name,
          kind: offered.kind_hint,
          currency_code: offered.currency_code,
          archived: false,
          sort_order: 0,
          ownership: { kind: "joint" },
        },
      },
    });
    expect(linked.response.status).toBe(201);

    // A sync row exists without anyone calling POST /providers/{id}/sync, and this time it
    // succeeded — the failure-path twin of this assertion is further down.
    const syncs = await api.GET("/api/providers/{id}/syncs", {
      params: { path: { id: linked.data!.id } },
    });
    expect(syncs.data?.length).toBe(1);
    expect(syncs.data![0].status).toBe("ok");
    expect(syncs.data![0].imported).toBe(1);

    const txns = await api.GET("/api/transactions", {
      params: { query: { account_id: linked.data!.account_id } },
    });
    expect(txns.data?.length).toBe(1);
    expect(txns.data![0].amount_minor).toBe(-8_235);

    // The account form demands an institution; a linked account has none until a sync reports
    // one, and this is the only place it can come from.
    const accounts = await api.GET("/api/accounts", {});
    const created = accounts.data?.find((a) => a.id === linked.data!.account_id);
    expect(created?.institution).toBe(INSTITUTION);
  } finally {
    server.stop();
  }
});

/**
 * The balance half of a sync, which is not a transaction at all.
 *
 * A bank feed's transaction history rarely reaches back to when the account was opened, so the
 * imported rows alone would leave the displayed balance drifting from the bank's. `sync_provider`
 * closes that by writing the upstream's live balance as a same-day provider-sourced valuation —
 * and, from the same response, backfills the amount that turns a balance into progress: the
 * facility's credit limit, which is what "remaining borrowing" is computed against. (A
 * mortgage's original principal rides the same path; both are no-ops on a kind with no such
 * concept, which is why this test uses one that has.)
 *
 * Asserted through the API rather than at the service, because the failure being prevented is a
 * number that renders: a valuation stored in the wrong currency, or a limit that never arrives,
 * is only visible from out here.
 */
test("a sync records the upstream balance as a valuation and backfills the credit limit", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    // Akahu reports revolving credit under its generic `LOAN` type, told apart from a term loan
    // by carrying an ongoing limit — the case that made `credit_limit_minor` fill in at all.
    const acc = await createAccount(api, "The Jam", "revolving_credit", "NZD", {
      metadata: { profile: "depository", credit_limit_minor: 1 },
    });
    const provider = await akahuProvider(api, acc.id);
    await stubSync(testproxy, {
      items: [],
      balance: account({
        balance: -12_345.67,
        limit: 50_000,
        kind: "LOAN",
        name: "The Jam",
      }),
    });

    const synced = await api.POST("/api/providers/{id}/sync", {
      params: { path: { id: provider.id } },
      body: {},
    });
    expect(synced.response.status).toBe(200);
    // Nothing was imported, and the sync is still a success: the balance is the point here.
    expect(synced.data?.imported).toBe(0);
    expect(synced.data?.status).toBe("ok");
    expect(synced.data?.detail).toBeFalsy();

    const valuations = await api.GET("/api/accounts/{id}/valuations", {
      params: { path: { id: acc.id } },
    });
    expect(valuations.data?.length).toBe(1);
    // Negative, because it is owed — net worth buckets on the sign alone.
    expect(valuations.data![0].value_minor).toBe(-1_234_567);
    expect(valuations.data![0].currency_code).toBe("NZD");
    // `provider`, not `manual`: the source is what lets a later sync replace this row instead
    // of piling a second valuation onto the same day.
    expect(valuations.data![0].source).toBe("provider");

    const accounts = await api.GET("/api/accounts", {});
    const refreshed = accounts.data?.find((a) => a.id === acc.id);
    expect(refreshed?.metadata).toMatchObject({ credit_limit_minor: 5_000_000 });
  } finally {
    server.stop();
  }
});

/**
 * The detail of a failed sync is bounded, and that is a data-exposure fix rather than a tidiness
 * one.
 *
 * A provider error is third-party text, and `akahu-client`'s deserialisation error used to
 * interpolate the *entire* response body into its `Display` — so one schema change on Akahu's
 * side turned a 200 holding a page of 100 real transactions into the error message. Both places
 * that message lands are exposures: `provider_syncs.detail` is an unbounded `TEXT` column served
 * straight back by `GET /api/providers/{id}/syncs`, and the 422 hands it to the client verbatim
 * (only 5xx is scrubbed). `sync_detail` caps it at `MAX_SYNC_DETAIL_CHARS`, at the one place both
 * copies are made.
 *
 * The upstream error below is deliberately synthetic filler rather than transaction-shaped text:
 * what is under test is the byte count, and a fixture never carries real data's identifiers. The
 * marker at its end is what proves the cap actually cut — a truncation that kept the tail would
 * have kept the last transactions on the page.
 */
test("an upstream failure is recorded as an error sync whose detail is capped", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    const acc = await createAccount(api, "Everyday", "bank");
    const provider = await akahuProvider(api, acc.id);
    // Akahu's error envelope: the `message` is what `akahu-client` renders into its `Display`,
    // which is the string that reaches `sync_detail`.
    const overlong = `${"SYNTHETIC-BODY-FILLER ".repeat(200)}TAIL-MARKER`;
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: `^/v1/accounts/${EXTERNAL_ID}/transactions$`,
      status: 500,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({ success: false, message: overlong }),
    });

    const failed = await api.POST("/api/providers/{id}/sync", {
      params: { path: { id: provider.id } },
      body: {},
    });
    expect(failed.response.status).toBe(422);
    const message = (failed.error as { error?: { message?: string } })?.error?.message ?? "";
    expect(message).toContain("sync failed:");
    expect(message).not.toContain("TAIL-MARKER");

    const syncs = await api.GET("/api/providers/{id}/syncs", {
      params: { path: { id: provider.id } },
    });
    expect(syncs.data?.length).toBe(1);
    expect(syncs.data![0].status).toBe("error");
    const detail = syncs.data![0].detail ?? "";
    // The cap's worth of the upstream's text plus the marker that says it was cut, and nothing
    // else: all 4KB of that message would otherwise have become the row and the response.
    expect([...detail].length).toBe(MAX_SYNC_DETAIL_CHARS + [..."… (truncated)"].length);
    expect(detail.startsWith("Internal server error: SYNTHETIC-BODY-FILLER")).toBe(true);
    expect(detail.endsWith("… (truncated)")).toBe(true);
    expect(detail).not.toContain("TAIL-MARKER");

    // Nothing landed in the ledger, and the watermark did not move — so the next poll asks for
    // the same window rather than skipping past history this sync never saw.
    const txns = await api.GET("/api/transactions", { params: { query: { account_id: acc.id } } });
    expect(txns.data?.length).toBe(0);
    const providers = await api.GET("/api/providers", {});
    expect(providers.data?.find((p) => p.id === provider.id)?.last_synced_at).toBeFalsy();
  } finally {
    server.stop();
  }
});

// ---- an unconfigured install, which is what a fresh checkout and every CI run is -----------

test("provider kinds list akahu as credential-based and discovery-capable", async ({ api }) => {
  const kinds = await api.GET("/api/provider-kinds", {});
  const akahu = kinds.data?.find((k) => k.kind === "akahu");
  expect(akahu?.accepts_payload).toBe(false);
  expect(akahu?.supports_account_discovery).toBe(true);
});

test("discovering akahu accounts without credentials fails clearly", async ({ api }) => {
  const { response, error } = await api.GET("/api/provider-kinds/{kind}/accounts", {
    params: { path: { kind: "akahu" } },
  });
  expect(response.status).toBe(422);
  expect((error as { error?: { message?: string } })?.error?.message).toContain("AKAHU_APP_TOKEN");
});

test("discovering an unknown provider kind fails clearly", async ({ api }) => {
  const { response } = await api.GET("/api/provider-kinds/{kind}/accounts", {
    params: { path: { kind: "nope" } },
  });
  expect(response.status).toBe(422);
});

test("syncing an akahu provider without credentials fails and is recorded", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");
  const { data: provider, response: createRes } = await api.POST("/api/providers", {
    body: {
      name: "Akahu — Everyday",
      kind: "akahu",
      account_id: acc.id,
      enabled: true,
      config: { external_account_id: "acc_manual" },
    },
  });
  expect(createRes.status).toBe(201);

  const { response: syncRes } = await api.POST("/api/providers/{id}/sync", {
    params: { path: { id: provider!.id } },
    body: {},
  });
  expect(syncRes.status).toBe(422);

  const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider!.id } } });
  expect(syncs.data?.length).toBe(1);
  expect(syncs.data![0].status).toBe("error");
});

test("linking a discovered account creates a new local account atomically", async ({ api }) => {
  const before = await api.GET("/api/accounts", {});
  const beforeCount = before.data?.length ?? 0;

  const { data: provider, response } = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_new_1",
      name: "Akahu — Spending",
      new_account: {
        name: "Spending",
        kind: "bank",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(response.status).toBe(201);
  expect(provider?.kind).toBe("akahu");
  expect((provider?.config as { external_account_id?: string })?.external_account_id).toBe("acc_new_1");

  const after = await api.GET("/api/accounts", {});
  expect(after.data?.length).toBe(beforeCount + 1);
  const created = after.data?.find((a) => a.name === "Spending");
  expect(created).toBeTruthy();
  expect(created?.id).toBe(provider?.account_id);
});

test("linking triggers an immediate sync attempt rather than waiting for the next poll", async ({ api }) => {
  const { data: provider, response } = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_autosync_1",
      name: "Akahu — Autosync",
      new_account: {
        name: "Autosync",
        kind: "bank",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  // Linking itself still succeeds even though the initial sync fails (no credentials on this
  // backend) — the failed attempt must not undo the just-created link.
  expect(response.status).toBe(201);

  const syncs = await api.GET("/api/providers/{id}/syncs", { params: { path: { id: provider!.id } } });
  // A sync row already exists without ever calling POST /providers/{id}/sync — proving
  // linking triggered it automatically rather than requiring a manual "Sync now" first.
  expect(syncs.data?.length).toBe(1);
  expect(syncs.data![0].status).toBe("error");
});

test("linking to an existing account doesn't create a new one", async ({ api }) => {
  const acc = await createAccount(api, "Savings", "savings");
  const before = await api.GET("/api/accounts", {});
  const beforeCount = before.data?.length ?? 0;

  const { data: provider, response } = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_existing_1",
      name: "Akahu — Savings",
      existing_account_id: acc.id,
    },
  });
  expect(response.status).toBe(201);
  expect(provider?.account_id).toBe(acc.id);

  const after = await api.GET("/api/accounts", {});
  expect(after.data?.length).toBe(beforeCount);
});

test("linking requires exactly one of new_account or existing_account_id", async ({ api }) => {
  const acc = await createAccount(api, "Everyday", "bank");

  const neither = await api.POST("/api/providers/link", {
    body: { kind: "akahu", external_id: "acc_x", name: "x" },
  });
  expect(neither.response.status).toBe(422);

  const both = await api.POST("/api/providers/link", {
    body: {
      kind: "akahu",
      external_id: "acc_y",
      name: "y",
      existing_account_id: acc.id,
      new_account: {
        name: "z",
        kind: "bank",
        currency_code: "NZD",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(both.response.status).toBe(422);
});

test("linking with an unknown provider kind is rejected", async ({ api }) => {
  const { response } = await api.POST("/api/providers/link", {
    body: { kind: "nope", external_id: "acc_x", name: "x", existing_account_id: 1 },
  });
  expect(response.status).toBe(422);
});

// A brokerage platform arrives as one Akahu account per currency wallet, and all of them belong
// to a single Sure account — so the connect dialog posts them together (`/link-group`) rather
// than once each. The body below is field-for-field the one `ProviderConnectModal.linkGroup`
// builds, which is the point of the test: `link_group` has DAL-level coverage already, and the
// half that was never checked from outside is whether the *client's* body is accepted. It omits
// `metadata` and both `opening_balance_*` fields — a brokerage account's value comes from its
// holdings ledger, not an opening figure — and either of those becoming required would surface
// to a user as the same thing a mis-keyed form did: "Failed to link brokerage account".
test("group-linking every wallet of a brokerage platform creates one account", async ({ api }) => {
  const before = await api.GET("/api/accounts", {});
  const beforeCount = before.data?.length ?? 0;

  const wallets = ["acc_grp_nzd", "acc_grp_usd", "acc_grp_aud"];
  const { data: providers, response } = await api.POST("/api/providers/link-group", {
    body: {
      kind: "akahu",
      members: wallets.map((id) => ({ external_id: id, name: `Akahu — ${id} Wallet` })),
      new_account: {
        name: "Sharesies",
        kind: "brokerage",
        currency_code: "NZD",
        institution: "Sharesies",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
      },
    },
  });
  expect(response.status).toBe(201);

  // One provider row per wallet, every one of them pointing at the same single new account.
  expect(providers?.length).toBe(3);
  const accountIds = new Set(providers?.map((p) => p.account_id));
  expect(accountIds.size).toBe(1);
  expect(
    providers?.map((p) => (p.config as { external_account_id?: string }).external_account_id).sort(),
  ).toEqual([...wallets].sort());

  const after = await api.GET("/api/accounts", {});
  expect(after.data?.length).toBe(beforeCount + 1);
  const created = after.data?.find((a) => a.id === providers![0].account_id);
  expect(created?.kind).toBe("brokerage");
  expect(created?.name).toBe("Sharesies");
});

/**
 * A joint account, which is the one duplicate `external_id` cannot catch.
 *
 * Akahu issues an id per *authorisation*, so an account both holders' logins can see arrives as
 * two rows that share nothing an id-based filter can compare: different `_id`, different
 * `_authorisation`, and — because each holder nicknames it themselves — usually a different
 * name. Only `formatted_account` pairs them.
 *
 * Linking both is not a cosmetic mistake. Each row becomes its own `providers` row against its
 * own account, both sweep the same upstream transactions, and the balance is counted twice in
 * net worth — with nothing in the UI to suggest the two accounts are one. The dialog used to
 * warn about it and let you do it anyway; worse, the warning needed both rows present to fire,
 * so once one copy was linked the API filtered it out and the survivor sat there unmarked.
 *
 * Three things are asserted here because each fails independently: the rows are *flagged*, the
 * twin is *withheld* after one is linked, and the link endpoint *refuses* it even when asked
 * directly — the last being the only one that holds against a stale page or a script.
 */
test("one account two logins report is joint, and can only be linked once", async ({
  testproxy,
}) => {
  const SHARED_NUMBER = "12-3456-0000123-00";
  const HERS = "acc_joint01";
  const HIS = "acc_joint02";

  /** The same bank account as one login sees it. */
  function view(id: string, authorisation: string, name: string, number: string) {
    return {
      ...account({ balance: 2410.55, name }),
      _id: id,
      _authorisation: authorisation,
      formatted_account: number,
    };
  }

  const { server, api } = await configured();
  try {
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: "^/v1/accounts$",
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({
        success: true,
        items: [
          view(HERS, "auth_hers", "Everyday", SHARED_NUMBER),
          view(HIS, "auth_his", "Joint acct", SHARED_NUMBER),
          // A third account only one login reports — the control. If this ever came back
          // joint, the pairing would be matching on something every row shares.
          view("acc_solo01", "auth_hers", "Savings", "12-3456-0000999-00"),
        ],
      }),
    });
    // The link triggers an initial sync of whichever row is linked; both are stubbed so the
    // test never depends on which one it picked.
    for (const id of [HERS, HIS]) {
      await testproxy.stub({
        upstream: "akahu",
        method: "GET",
        path_pattern: `^/v1/accounts/${id}/transactions$`,
        status: 200,
        response_headers: { "content-type": "application/json" },
        body: JSON.stringify({ success: true, items: [], cursor: { next: null } }),
      });
      await testproxy.stub({
        upstream: "akahu",
        method: "GET",
        path_pattern: `^/v1/accounts/${id}$`,
        status: 200,
        response_headers: { "content-type": "application/json" },
        body: JSON.stringify({ success: true, item: view(id, "auth_hers", "Everyday", SHARED_NUMBER) }),
      });
    }

    const discovered = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: "akahu" } },
    });
    expect(discovered.response.status).toBe(200);
    const byId = new Map(discovered.data!.map((a) => [a.external_id, a]));
    expect(byId.get(HERS)?.joint).toBe(true);
    expect(byId.get(HIS)?.joint).toBe(true);
    expect(byId.get("acc_solo01")?.joint).toBe(false);

    // Deliberately asking for one person's: the server decides ownership for a joint account,
    // so what the client sent must not survive.
    const linked = await api.POST("/api/providers/link", {
      body: {
        kind: "akahu",
        external_id: HERS,
        name: "Akahu — Everyday",
        new_account: {
          name: "Everyday",
          kind: "bank",
          currency_code: "NZD",
          archived: false,
          sort_order: 0,
          ownership: { kind: "person", person_id: 1 },
        },
      },
    });
    expect(linked.response.status).toBe(201);

    const accounts = await api.GET("/api/accounts", {});
    const created = accounts.data?.find((a) => a.id === linked.data!.account_id);
    expect(created?.ownership).toEqual({ kind: "joint" });

    // The twin goes with it. This is the assertion the old client-side warning could not make:
    // its pair had been filtered out, so there was nothing left to compare against.
    const after = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: "akahu" } },
    });
    expect(after.data?.map((a) => a.external_id)).toEqual(["acc_solo01"]);

    // And asking for it anyway — the case a stale page or a kept id produces — is refused
    // rather than quietly creating the second copy.
    const twin = await api.POST("/api/providers/link", {
      body: {
        kind: "akahu",
        external_id: HIS,
        name: "Akahu — Joint acct",
        new_account: {
          name: "Joint acct",
          kind: "bank",
          currency_code: "NZD",
          archived: false,
          sort_order: 0,
          ownership: { kind: "joint" },
        },
      },
    });
    expect(twin.response.status).toBe(422);
    // Both names, so the message describes what happened rather than just forbidding it.
    const refusal = (twin.error as { error?: { message?: string } })?.error?.message ?? "";
    expect(refusal).toContain("Joint acct");
    expect(refusal).toContain("Everyday");

    // Nothing was created by the refusal — the point of refusing before the write.
    const finalAccounts = await api.GET("/api/accounts", {});
    expect(finalAccounts.data?.filter((a) => a.name === "Joint acct").length).toBe(0);
  } finally {
    server.stop();
  }
});

/**
 * Suggesting a mortgage's original amount from the drawdown in its own history.
 *
 * ASB's mortgages arrive through Akahu with no `meta.loan_details.initial_principal`, and the
 * amount borrowed is demanded on *every* write path — `AMORTISING_REQUIRED` covers the link
 * path too, because a mortgage without its terms cannot be forecast. So the field has to be
 * answered to link at all, and until now it had to be typed: a number people reconstruct as
 * the balance on the day they connected the bank rather than the advance that opened the loan.
 *
 * Out here rather than only in `sure_app`'s unit tests because the sign is the whole game and
 * nothing in-process proves it. Akahu quotes a loan account's movements in the same signed
 * frame this app uses — an advance grows the debt and is negative, a repayment is positive —
 * and `map_transaction` passes the amount through unflipped. A unit test that hand-writes the
 * row assumes that; this asserts it against the adapter that actually parses the feed.
 */
test("a mortgage's original amount is suggested from the drawdown in its history", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: "^/v1/accounts$",
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({
        success: true,
        // No `loan_details`, which is the premise: nothing but the history to read.
        items: [account({ balance: -484_210.0, kind: "LOAN", name: "Prime Housing Lending" })],
      }),
    });
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: `^/v1/accounts/${EXTERNAL_ID}/transactions$`,
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({
        success: true,
        items: [
          // Negative: the advance grew the debt. This is the row the suggestion comes from.
          txn({
            id: "trans_draw01",
            date: "2026-03-02T00:00:00.000Z",
            description: "Loan drawdown",
            amount: -485_000.0,
          }),
          txn({
            id: "trans_int01",
            date: "2026-03-31T00:00:00.000Z",
            description: "Interest",
            amount: -2_310.0,
          }),
          txn({
            id: "trans_pay01",
            date: "2026-03-31T00:00:00.000Z",
            description: "Payment received",
            amount: 3_100.0,
          }),
        ],
        cursor: { next: null },
      }),
    });

    const discovered = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: "akahu" } },
    });
    expect(discovered.response.status).toBe(200);
    expect(discovered.data?.length).toBe(1);
    const offered = discovered.data![0];
    expect(offered.kind_hint).toBe("mortgage");
    expect(offered.original_amount_hint_minor).toBe(48_500_000);
  } finally {
    server.stop();
  }
});

/**
 * The ordinary case, and the one that has to stay quiet: a feed reaches back a year or so and a
 * mortgage runs for thirty, so the drawdown is usually off the front of the window. The largest
 * advance left is a monthly interest charge, nowhere near the balance still owed.
 */
test("a mortgage whose drawdown predates the feed's window is suggested nothing", async ({
  testproxy,
}) => {
  const { server, api } = await configured();
  try {
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: "^/v1/accounts$",
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({
        success: true,
        items: [account({ balance: -512_400.0, kind: "LOAN", name: "Old Lending" })],
      }),
    });
    await testproxy.stub({
      upstream: "akahu",
      method: "GET",
      path_pattern: `^/v1/accounts/${EXTERNAL_ID}/transactions$`,
      status: 200,
      response_headers: { "content-type": "application/json" },
      body: JSON.stringify({
        success: true,
        items: [
          txn({
            id: "trans_int02",
            date: "2026-06-28T00:00:00.000Z",
            description: "Interest",
            amount: -2_290.0,
          }),
          txn({
            id: "trans_pay02",
            date: "2026-06-28T00:00:00.000Z",
            description: "Payment received",
            amount: 3_100.0,
          }),
        ],
        cursor: { next: null },
      }),
    });

    const discovered = await api.GET("/api/provider-kinds/{kind}/accounts", {
      params: { path: { kind: "akahu" } },
    });
    // Still offered, and still linkable — just with the field left for the user to answer.
    expect(discovered.data?.length).toBe(1);
    expect(discovered.data![0].original_amount_hint_minor).toBeFalsy();
  } finally {
    server.stop();
  }
});
