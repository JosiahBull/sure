/**
 * HTTP-boundary behaviour: cache directives, conditional requests, compression, protocol
 * support, and the abuse guards.
 *
 * These assert on headers and status codes rather than payloads, so they use `fetch`
 * directly where the generated client would hide the response. Anything needing
 * non-default configuration spawns its own backend via `startServer`.
 */
import http2 from "node:http2";
import net from "node:net";

import { test, expect, startServer } from "../fixtures";
import { createAccount } from "../helpers";

/** Case-insensitive header read that fails loudly rather than returning null. */
function header(res: Response, name: string): string {
  const value = res.headers.get(name);
  expect(value, `expected a ${name} header`).not.toBeNull();
  return value!;
}

/**
 * One attempt at POSTing `bytes` of body, resolving with the server's answer — or `null`
 * if the connection died before a complete response could be read.
 *
 * Deliberately not `fetch`. The body cap is enforced part-way through the upload: the
 * server reads up to the limit, answers 413, and closes with the rest of the body still
 * unread in its receive buffer — which makes the close an RST rather than a FIN, and an
 * RST tells the client's kernel to discard whatever it has buffered, response included.
 * `undici` turns that into `TypeError: fetch failed` and loses the 413 it had already
 * been sent. Reading the socket directly wins that race nearly every time, but "nearly"
 * is why [`postOversized`] retries: the outcome is decided in the kernel, not here, which
 * is why this only ever failed under the load of a full suite run.
 */
function attemptOversized(baseURL: string, bytes: number): Promise<Response | null> {
  const url = new URL(baseURL);
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: url.hostname, port: Number(url.port) });
    const chunks: Buffer[] = [];
    // The refusal closes the connection under our feet; that is the expected ending here,
    // not a failure. A response that never arrives is caught by the test timeout.
    socket.on("error", () => {});
    socket.on("data", (chunk) => chunks.push(chunk));
    socket.on("close", () => {
      const raw = Buffer.concat(chunks).toString("latin1");
      const separator = raw.indexOf("\r\n\r\n");
      // Reset before the response could be read in full — the caller tries again.
      if (separator === -1) return resolve(null);
      const [statusLine, ...headerLines] = raw.slice(0, separator).split("\r\n");
      const headers = new Headers(
        headerLines.map((line) => {
          const at = line.indexOf(":");
          return [line.slice(0, at), line.slice(at + 1).trim()] as [string, string];
        }),
      );
      // Rebuilt as a `Response` so the assertions below read like every other test here.
      resolve(
        new Response(raw.slice(separator + 4), {
          status: Number(statusLine.split(" ")[1]),
          headers,
        }),
      );
    });
    socket.on("connect", () => {
      socket.write(
        `POST /api/accounts HTTP/1.1\r\nHost: ${url.host}\r\n` +
          `Content-Type: application/json\r\nContent-Length: ${bytes}\r\n\r\n`,
      );
      // Written in chunks, and only while the socket is still up: one 3 MB `write` would
      // sit in Node's buffer and keep the process alive after the peer had gone.
      const chunk = Buffer.alloc(64 * 1024, "a");
      let sent = 0;
      const pump = () => {
        while (sent < bytes && !socket.destroyed && !socket.writableEnded) {
          const size = Math.min(chunk.length, bytes - sent);
          sent += size;
          if (!socket.write(chunk.subarray(0, size))) return socket.once("drain", pump);
        }
      };
      pump();
    });
  });
}

/**
 * POST an oversized body and return the server's refusal, retrying past a connection that
 * was reset before the response could be read (see [`attemptOversized`]).
 *
 * Retrying does not weaken what is under test: only the guard produces a response at all,
 * so a server that stopped refusing oversized bodies fails every attempt on the status
 * assertion rather than being retried into passing.
 */
async function postOversized(baseURL: string, bytes: number): Promise<Response> {
  for (let attempt = 0; attempt < 5; attempt++) {
    const res = await attemptOversized(baseURL, bytes);
    if (res) return res;
  }
  throw new Error("the server never returned a complete response to an oversized body");
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

// Shutdown lives in `shutdown.spec.ts`. It used to be one exit-code assertion here, which
// a process that exits while its tasks are still running passes just as happily as one
// that drained — so it moved somewhere it could assert on the shutdown report instead.
