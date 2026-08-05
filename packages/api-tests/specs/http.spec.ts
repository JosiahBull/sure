/**
 * HTTP-boundary behaviour: cache directives, conditional requests, compression, protocol
 * support, and the abuse guards.
 *
 * These assert on headers and status codes rather than payloads, so they use `fetch`
 * directly where the generated client would hide the response. Anything needing
 * non-default configuration spawns its own backend via `startServer`.
 *
 * The last two sections need something the rest of the file does not: a handler genuinely
 * suspended at an await point. A deadline and an in-flight ceiling are both only observable
 * against a request that is *still running*, and the only await a test can hold open from
 * outside the process is an outbound provider call — so those tests drive `testproxy.pause`
 * and nothing else in the file does.
 */
import http2 from "node:http2";

import { test, expect, startServer, createSureClient } from "../fixtures";
import { createAccount, postOversized } from "../helpers";

/** Case-insensitive header read that fails loudly rather than returning null. */
function header(res: Response, name: string): string {
  const value = res.headers.get(name);
  expect(value, `expected a ${name} header`).not.toBeNull();
  return value!;
}


// ---- cache directives -------------------------------------------------------------

test("API reads are private and revalidated, never CDN-cacheable", async ({ server }) => {
  const res = await fetch(`${server.baseURL}/api/accounts`);
  expect(res.status).toBe(200);
  expect(header(res, "cache-control")).toBe("private, no-cache");
  // The API has no auth, so a shared cache must never keep a response — belt and braces
  // against a CDN rule that would otherwise ignore `private`.
  expect(header(res, "cdn-cache-control")).toBe("no-store");
  expect(header(res, "cloudflare-cdn-cache-control")).toBe("no-store");
});

test("mutations and sensitive reads are never stored", async ({ server, api }) => {
  const created = await api.POST("/api/accounts", {
    // A bank account needs an institution and an opening balance to save at all; this test
    // is about the response headers, so the values are the cheapest valid ones.
    body: {
      name: "x",
      kind: "bank",
      currency_code: "NZD",
      institution: "ANZ",
      archived: false,
      sort_order: 0,
      ownership: { kind: "joint" },
      opening_balance_minor: 0,
      opening_balance_date: "2020-01-01",
    },
  });
  expect(created.response.status).toBe(201);
  expect(created.response.headers.get("cache-control")).toBe("no-store");

  for (const path of ["/api/health", "/api/config/export"]) {
    const res = await fetch(`${server.baseURL}${path}`);
    expect(res.status, path).toBe(200);
    expect(header(res, "cache-control"), path).toBe("no-store");
  }
});

test("expensive projections may be reused briefly by the browser only", async ({ server }) => {
  const res = await fetch(`${server.baseURL}/api/forecast`);
  expect(res.status).toBe(200);
  expect(header(res, "cache-control")).toMatch(/^private, max-age=\d+/);
  expect(header(res, "cdn-cache-control")).toBe("no-store");
});

test("the CDN directives can be turned off", async () => {
  const server = await startServer({ CDN_CACHE_HEADERS: "off" });
  try {
    const res = await fetch(`${server.baseURL}/api/accounts`);
    expect(header(res, "cache-control")).toBe("private, no-cache");
    expect(res.headers.get("cdn-cache-control")).toBeNull();
    expect(res.headers.get("cloudflare-cdn-cache-control")).toBeNull();
  } finally {
    server.stop();
  }
});

// ---- conditional requests ---------------------------------------------------------

test("an unchanged read revalidates to an empty 304", async ({ server, api }) => {
  await createAccount(api, "Everyday", "bank");

  const first = await fetch(`${server.baseURL}/api/accounts`);
  expect(first.status).toBe(200);
  const etag = header(first, "etag");
  // Weak, because compression sits outside the layer that computes it.
  expect(etag).toMatch(/^W\/"/);

  const second = await fetch(`${server.baseURL}/api/accounts`, {
    headers: { "If-None-Match": etag },
  });
  expect(second.status).toBe(304);
  expect(await second.text()).toBe("");
  // RFC 9110 §15.4.5: a 304 carries the metadata a cache needs to refresh its entry.
  expect(header(second, "etag")).toBe(etag);
  expect(header(second, "cache-control")).toBe("private, no-cache");
  expect(header(second, "cdn-cache-control")).toBe("no-store");
});

test("the validator changes when the data does", async ({ server, api }) => {
  const before = header(await fetch(`${server.baseURL}/api/accounts`), "etag");
  await createAccount(api, "Savings", "savings");
  const after = await fetch(`${server.baseURL}/api/accounts`, {
    headers: { "If-None-Match": before },
  });
  expect(after.status).toBe(200);
  expect(header(after, "etag")).not.toBe(before);
});

test("responses that must not be stored carry no validator", async ({ server }) => {
  const res = await fetch(`${server.baseURL}/api/health`);
  expect(res.headers.get("etag")).toBeNull();
});

// ---- compression ------------------------------------------------------------------

test("responses compress when asked and not otherwise", async ({ server }) => {
  const compressed = await fetch(`${server.baseURL}/api/openapi.json`, {
    headers: { "Accept-Encoding": "gzip" },
  });
  expect(header(compressed, "content-encoding")).toBe("gzip");
  expect(compressed.headers.get("vary")?.toLowerCase()).toContain("accept-encoding");

  // `undici` adds its own Accept-Encoding unless told otherwise; "identity" is the
  // explicit way to ask for none.
  const plain = await fetch(`${server.baseURL}/api/openapi.json`, {
    headers: { "Accept-Encoding": "identity" },
  });
  expect(plain.headers.get("content-encoding")).toBeNull();
});

test("compression can be turned off", async () => {
  const server = await startServer({ COMPRESSION: "off" });
  try {
    const res = await fetch(`${server.baseURL}/api/openapi.json`, {
      headers: { "Accept-Encoding": "gzip" },
    });
    expect(res.headers.get("content-encoding")).toBeNull();
  } finally {
    server.stop();
  }
});

// ---- static assets ----------------------------------------------------------------

test("static assets are cached by how the build names them", async () => {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const path = await import("node:path");

  // Mirrors what `vite build` + vite-plugin-pwa emit: fingerprinted bundles under
  // /assets, a stable-named shell, service worker, and manifest, and unfingerprinted
  // fonts and icons.
  const webDir = mkdtempSync(path.join(tmpdir(), "sure-web-"));
  mkdirSync(path.join(webDir, "assets"));
  mkdirSync(path.join(webDir, "fonts"));
  writeFileSync(path.join(webDir, "assets", "index-Bl2CtK_1.js"), "console.log(1)\n");
  writeFileSync(path.join(webDir, "workbox-abeb32eb.js"), "// workbox\n");
  writeFileSync(path.join(webDir, "index.html"), "<!doctype html><title>Sure</title>\n");
  writeFileSync(path.join(webDir, "sw.js"), "// service worker\n");
  writeFileSync(path.join(webDir, "manifest.webmanifest"), "{}\n");
  writeFileSync(path.join(webDir, "icon-512.png"), "not-really-a-png\n");
  writeFileSync(path.join(webDir, "fonts", "Geist.woff2"), "not-really-a-font\n");

  const server = await startServer({ WEB_DIR: webDir });
  try {
    const cacheControl = async (p: string) =>
      header(await fetch(`${server.baseURL}${p}`), "cache-control");

    // Fingerprinted: the name changes with the bytes, so it can be held forever.
    expect(await cacheControl("/assets/index-Bl2CtK_1.js")).toBe(
      "public, max-age=31536000, immutable"
    );
    expect(await cacheControl("/workbox-abeb32eb.js")).toBe(
      "public, max-age=31536000, immutable"
    );

    // Stable names: caching a stale shell or service worker is how a deploy fails to
    // reach the device.
    for (const p of ["/index.html", "/sw.js", "/manifest.webmanifest"]) {
      expect(await cacheControl(p), p).toBe("public, max-age=0, must-revalidate");
    }

    // Stable-named but rarely changing.
    for (const p of ["/icon-512.png", "/fonts/Geist.woff2"]) {
      expect(await cacheControl(p), p).toMatch(/^public, max-age=86400/);
    }

    // The shell revalidates cheaply too — `ServeDir` only offers `Last-Modified`, which
    // has one-second granularity, so the validator comes from our own layer.
    const shell = await fetch(`${server.baseURL}/index.html`);
    const etag = header(shell, "etag");
    const again = await fetch(`${server.baseURL}/index.html`, {
      headers: { "If-None-Match": etag },
    });
    expect(again.status).toBe(304);

    // A client-side route still gets the SPA shell rather than an API error envelope.
    const deepLink = await fetch(`${server.baseURL}/settings/accounts`);
    expect(header(deepLink, "content-type")).toContain("text/html");
    expect(await deepLink.text()).toContain("<title>Sure</title>");
  } finally {
    server.stop();
  }
});

// ---- protocol ---------------------------------------------------------------------

test("serves HTTP/2 over cleartext to a client with prior knowledge", async ({ server }) => {
  // No browser does this; a reverse proxy configured to speak HTTP/2 to its origin does.
  const status = await new Promise<number>((resolve, reject) => {
    const session = http2.connect(server.baseURL);
    session.on("error", reject);
    const request = session.request({ ":path": "/api/health" });
    request.on("response", (headers) => {
      resolve(Number(headers[":status"]));
      session.close();
    });
    request.on("error", reject);
    request.end();
  });
  expect(status).toBe(200);
});

// ---- abuse guards -----------------------------------------------------------------

test("an oversized body is refused in the standard error envelope", async ({ server }) => {
  const res = await postOversized(server.baseURL, 3 * 1024 * 1024);
  expect(res.status).toBe(413);
  expect(header(res, "content-type")).toContain("application/json");
  expect(await res.json()).toEqual({
    error: { code: "payload_too_large", message: expect.any(String) },
  });
});

test("a config snapshot larger than the global body cap is still importable", async ({
  server,
}) => {
  // The matching export is a full database dump, so the 2 MB global cap would make a
  // snapshot round trip impossible on any established ledger.
  const padding = "x".repeat(4 * 1024 * 1024);
  const res = await fetch(`${server.baseURL}/api/config/import`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ not_a_real_snapshot: padding }),
  });
  // Rejected on content, not on size — 413 would mean the cap was never raised.
  expect(res.status).not.toBe(413);
  expect([200, 422]).toContain(res.status);
});

test("a client asking too often is refused with a retry hint", async () => {
  const server = await startServer({
    RATE_LIMIT_EXEMPT_LOOPBACK: "false",
    RATE_LIMIT_RPS: "1",
    RATE_LIMIT_BURST: "3",
  });
  try {
    // The health probe in `startServer` has already spent part of the burst, so keep
    // going until the limiter bites rather than assuming an exact count.
    let limited: Response | undefined;
    for (let i = 0; i < 10 && !limited; i++) {
      const res = await fetch(`${server.baseURL}/api/health`);
      if (res.status === 429) limited = res;
      else await res.text();
    }
    expect(limited, "expected the limiter to refuse within 10 requests").toBeTruthy();
    expect(Number(header(limited!, "retry-after"))).toBeGreaterThanOrEqual(1);
    expect(header(limited!, "cache-control")).toBe("no-store");
    expect(await limited!.json()).toEqual({
      error: { code: "rate_limited", message: expect.any(String) },
    });
  } finally {
    server.stop();
  }
});

test("loopback is exempt from rate limiting by default", async ({ server }) => {
  // Otherwise the seed script, local curl, and this very suite would trip the limiter.
  const results = await Promise.all(
    Array.from({ length: 80 }, () => fetch(`${server.baseURL}/api/health`))
  );
  expect(results.every((r) => r.status === 200)).toBe(true);
});

test("security headers are on every response, including rejections", async () => {
  const server = await startServer({
    RATE_LIMIT_EXEMPT_LOOPBACK: "false",
    RATE_LIMIT_RPS: "1",
    RATE_LIMIT_BURST: "2",
  });
  try {
    const expected = {
      "x-content-type-options": "nosniff",
      "referrer-policy": "no-referrer",
      "x-frame-options": "DENY",
      "cross-origin-opener-policy": "same-origin",
    };

    let rejection: Response | undefined;
    for (let i = 0; i < 10 && !rejection; i++) {
      const res = await fetch(`${server.baseURL}/api/health`);
      if (res.status === 429) rejection = res;
      else await res.text();
    }
    expect(rejection).toBeTruthy();
    for (const [name, value] of Object.entries(expected)) {
      expect(rejection!.headers.get(name), `${name} on a 429`).toBe(value);
    }
  } finally {
    server.stop();
  }
});

test("cross-origin reads are limited to the configured allowlist", async ({ server }) => {
  // Without authentication, CORS is the only thing between an arbitrary page in the
  // user's browser and `GET /api/config/export`.
  const allowed = await fetch(`${server.baseURL}/api/health`, {
    headers: { Origin: "https://sure.bullfamilies.com" },
  });
  expect(allowed.headers.get("access-control-allow-origin")).toBe(
    "https://sure.bullfamilies.com"
  );

  const denied = await fetch(`${server.baseURL}/api/health`, {
    headers: { Origin: "https://evil.example" },
  });
  expect(denied.headers.get("access-control-allow-origin")).toBeNull();

  // A request with no Origin at all (curl, this suite, the seed script) is untouched.
  const plain = await fetch(`${server.baseURL}/api/health`);
  expect(plain.status).toBe(200);
});

// ---- the per-request deadline -------------------------------------------------------
//
// `pause` is what makes this section possible. It holds every request to an upstream
// received-but-unanswered, so the handler waiting on it is suspended for exactly as long as
// the test likes — the one await point in a request that a spec can control. Neither test
// below registers a stub, and that is deliberate rather than an omission: the pause gate runs
// ahead of the proxy's stub scan, so an unstubbed request is held just the same and, once
// resumed, takes the ordinary replay miss. A deadline test that carried a provider's document
// shape would fail the day that document changed, for a reason having nothing to do with
// deadlines.

test("a handler stuck on an upstream is cut loose at the request deadline", async ({
  testproxy,
}) => {
  // `deadline_for` is unit-tested as a pure function, which leaves two things nothing can see.
  //
  // The layer reads `MatchedPath`, an extension that exists only because the layer sits inside
  // routing: hoist it above the router in a refactor and every route silently falls back to
  // the *normal* deadline while the pure test stays green. Here the 408 simply stops arriving.
  //
  // And an ordering that is otherwise invisible. `sure_providers::http` puts a 6s
  // `REQUEST_TIMEOUT` on the outbound call, so a 1s server deadline has to be the one that
  // fires; if it ever stopped winning, a client would wait six times the deadline it
  // configured and be told 502 (an upstream failed) rather than 408 (we gave up).
  const server = await startServer({ REQUEST_TIMEOUT_SECS: "1" });
  try {
    const api = createSureClient(server.baseURL);
    // `shares_us` comes with a ticker and an exchange (helpers.ts), which is what makes this
    // route reach the price feed at all — without them it 404s before a socket is opened.
    const account = await createAccount(api, "Vanguard S&P 500", "shares_us", "USD");

    await testproxy.pause("yahoo_finance");
    const started = Date.now();
    const res = await fetch(`${server.baseURL}/api/accounts/${account.id}/stock-price`);
    const elapsed = Date.now() - started;

    expect(res.status).toBe(408);
    // `timeout` is the code `cache::timeout` emits. Worth pinning rather than settling for the
    // status: 408 with any other code would mean something else on the request path gave up,
    // and the fix would be somewhere else entirely.
    expect(await res.json()).toEqual({
      error: { code: "timeout", message: expect.any(String) },
    });
    // The margin is against the adapter's 6s ceiling, which is the only other way this request
    // could have ended. The layer answers at 1s and the adapter cannot answer before 6s, so
    // 4s separates them by 3s in one direction and 2s in the other — a gap no ordinary runner
    // jitters across, and one that fails loudly (as a 502 at ~6s) if the deadline is lost.
    expect(elapsed, `408 arrived after ${elapsed}ms`).toBeLessThan(4_000);
  } finally {
    // Before the server goes, and on the failure path too: a paused upstream left behind holds
    // whatever is in flight, which turns a clean assertion failure into a teardown timeout.
    await testproxy.resume("yahoo_finance");
    server.stop();
  }
});

/**
 * Credentials, injected, so a sync gets far enough to make an outbound call — the fixture
 * strips both from every server it spawns. Invented and obviously so, and the same pair
 * `specs/provider-sync-behaviour.spec.ts` and `packages/providers/tests/akahu.rs` already use
 * (CLAUDE.md rule 3). `acc_spend01` likewise: `AccountId::new` only checks the `acc_` prefix.
 */
const AKAHU_TOKENS = { AKAHU_APP_TOKEN: "app_token_test", AKAHU_USER_TOKEN: "user_token_test" };

test("a long route is not held to the normal deadline", async ({ testproxy }) => {
  // `LONG_ROUTES` exists because `POST /api/providers/{id}/sync` waits on somebody else's API.
  // Its unit test proves the *table*; it cannot prove a request was given the table's answer,
  // and the ways that wiring breaks are all silent — a `Deadlines` built from one field twice,
  // the layer moved somewhere `MatchedPath` is absent, an entry whose template drifts from the
  // `.route(..)` call. Any of them and every bank sync starts dying at the ordinary deadline,
  // which on a real feed is most of them, reported as a timeout rather than as a lost setting.
  const server = await startServer({
    ...AKAHU_TOKENS,
    REQUEST_TIMEOUT_SECS: "2",
    LONG_REQUEST_TIMEOUT_SECS: "20",
  });
  try {
    const api = createSureClient(server.baseURL);
    const account = await createAccount(api, "Everyday", "bank");
    // `POST /api/providers`, not `/api/providers/link`: linking fires an immediate best-effort
    // sync, and this test wants the only outbound call to be the one it starts itself.
    const created = await api.POST("/api/providers", {
      body: {
        name: "Akahu — Everyday",
        kind: "akahu",
        account_id: account.id,
        enabled: true,
        config: { external_account_id: "acc_spend01" },
      },
    });
    expect(created.response.status, "create provider").toBe(201);

    await testproxy.pause("akahu");
    const syncing = api
      .POST("/api/providers/{id}/sync", {
        params: { path: { id: created.data!.id } },
        body: {},
      })
      .then((r) => r.response.status)
      // This promise is deliberately left unawaited for seconds, and abandoned entirely on a
      // failing path where `finally` then kills the server out from under it. A rejection there
      // is unhandled — a second, louder error stacked on the assertion that actually failed —
      // so it is caught here and asserted on below instead of being allowed to escape.
      .catch((err: unknown) => `no response at all: ${String(err)}`);

    // 3.5s, bracketed by the two events that could end this request early: the 2s normal
    // deadline is 1.5s behind it, and the 6s ceiling `sure_providers::http` puts on the
    // outbound call is 2.5s ahead. The 20s long deadline is never reached — waiting it out
    // would cost twenty seconds to learn what the first 1.5s of margin already says.
    const verdict = await Promise.race([
      syncing.then((status) => `answered ${status}`),
      new Promise<string>((r) => setTimeout(() => r("still running"), 3_500)),
    ]);
    expect(verdict, "the sync was cut off before the long deadline").toBe("still running");

    await testproxy.resume("akahu");
    // Answered by the handler once its upstream came back — a replay miss, since this test
    // stubs nothing. *Which* status a failed sweep produces belongs to
    // `specs/provider-sync-behaviour.spec.ts`; all this test asks is who answered. Checked for
    // a status first, so a request that never got one cannot satisfy "not 408" by default.
    const answered = await syncing;
    expect(answered, "the sync produced no response at all").toEqual(expect.any(Number));
    expect(answered, "the deadline layer answered after all").not.toBe(408);
  } finally {
    await testproxy.resume("akahu");
    server.stop();
  }
});

// ---- the in-flight ceiling ------------------------------------------------------------

test("an in-flight permit is held for the whole request and handed back afterwards", async ({
  testproxy,
}) => {
  // `RateLimiter` and the shape of `overloaded_response` both have unit tests; the layer
  // between them has none, and the property it owns is the permit's *lifetime*. Released
  // early, the ceiling is decorative and a burst walks through it. Never released, the first
  // slow request wedges the process into answering 503 to everything until a restart. A test
  // that only fires one request cannot tell either from a working shedder, which is why the
  // second half below — a request that succeeds once the first finishes — is the load-bearing
  // one rather than the 503.
  const server = await startServer({ MAX_IN_FLIGHT: "1" });
  try {
    const api = createSureClient(server.baseURL);
    const account = await createAccount(api, "Vanguard S&P 500", "shares_us", "USD");

    await testproxy.pause("yahoo_finance");
    let heldStatus: number | undefined;
    const held = fetch(`${server.baseURL}/api/accounts/${account.id}/stock-price`)
      .then(async (r) => {
        heldStatus = r.status;
        // Drained so the connection is released rather than left half-read behind us.
        await r.text();
      })
      // Swallowed, because on a failing path this promise is abandoned and `finally` kills the
      // server out from under it: without this, every assertion failure below arrives with a
      // "socket closed" rejection stacked on top of the one that matters. Observed while
      // checking that this test fails when the ceiling is raised.
      .catch(() => {});

    // Let the held request take the slot before anything else asks for it. The margin is not a
    // guess at how long the handler runs — the pause gate holds it indefinitely once it gets
    // there — so all 500ms has to cover is a loopback request reaching an axum handler, which
    // is a fraction of a millisecond.
    //
    // A sleep, because there is nothing to observe: a paused request is not recorded, so
    // `assertSeen` blocks to timeout on exactly this case. And it cannot be dropped in favour
    // of the poll below. A probe fired alongside the held request is not racing it by
    // microseconds, it is *simultaneous* with it: whichever reaches `try_acquire_owned` first
    // takes the only permit and the other is shed, which measured at roughly a coin flip —
    // this test failed two runs in five before the wait existed.
    await new Promise((r) => setTimeout(r, 500));

    // Now confirm the slot is *taken* rather than assuming it: while the paused request holds
    // the only one, every other request is shed, so a 503 from the cheapest route in the API
    // is the ceiling reporting its own state. Still a loop, so a runner slower than the margin
    // above costs an iteration rather than a failure — and the in-loop check names the case
    // where even the whole deadline was not enough, because "the paused request was itself
    // shed" and "the shedder is broken" look identical from out here and want opposite fixes.
    let shed: Response | undefined;
    const deadline = Date.now() + 5_000;
    while (!shed && Date.now() < deadline) {
      const probe = await fetch(`${server.baseURL}/api/health`);
      if (probe.status === 503) shed = probe;
      else await probe.text();
      expect(heldStatus, "the probe won the slot and the paused request was shed").toBeUndefined();
      if (!shed) await new Promise((r) => setTimeout(r, 20));
    }
    expect(shed, "nothing was shed while the only in-flight slot was occupied").toBeTruthy();

    // The whole trio `sure_core::error::overloaded_response` emits, because it is deliberately
    // one shape: a client that recognises the 503 but not the code cannot tell "this server is
    // busy" from "an upstream is down" — which is a 502 here precisely so the two back off
    // differently — and one that reads neither header has to guess how long to wait.
    // `OVERLOADED_RETRY_AFTER_SECS` is 1, because a full slot table clears in milliseconds.
    expect(header(shed!, "retry-after")).toBe("1");
    expect(await shed!.json()).toEqual({
      error: { code: "overloaded", message: expect.any(String) },
    });

    await testproxy.resume("yahoo_finance");
    await held;
    // No sleep between the two, and none needed: `_permit` is bound in the match arm that
    // awaits the inner service, so it is dropped as that arm ends — before the response is
    // handed back up the stack, let alone written to a socket. A client holding the whole
    // response therefore already knows the slot is free; waiting on the held request *is*
    // waiting on the permit.
    const after = await fetch(`${server.baseURL}/api/health`);
    expect(after.status, "the permit was not returned when the request finished").toBe(200);

    // And the held request was served rather than shed on its way in — the distinction the
    // first half of this test rests on. A 503 could only have come from the shedder: the
    // proxy's own replay-miss 503 reaches the client as a 502 (`AppError::Upstream`).
    expect(typeof heldStatus, "the held request produced no response at all").toBe("number");
    expect(heldStatus, "the held request was shed rather than served").not.toBe(503);
  } finally {
    await testproxy.resume("yahoo_finance");
    server.stop();
  }
});

// Shutdown lives in `shutdown.spec.ts`. It used to be one exit-code assertion here, which
// a process that exits while its tasks are still running passes just as happily as one
// that drained — so it moved somewhere it could assert on the shutdown report instead.
