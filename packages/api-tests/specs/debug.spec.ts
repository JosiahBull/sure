/**
 * `GET /api/debug/request`: what the server saw of the request, for the SPA's connection page
 * (packages/api/src/routes/debug.rs). The API has no authentication, so the whitelist is the
 * contract: these tests send credentials alongside the proxy headers and assert that only the
 * proxy headers come back.
 *
 * Every address is from a documentation range or invented: 100.64.12.34 is a made-up tailnet
 * address, and 192.168.7.8 is any LAN.
 */
import type { Schemas } from "../../client/src/index";
import { test, expect } from "../fixtures";

type Echo = Schemas["RequestEcho"];

const SECRET = "do-not-echo";

async function echo(baseURL: string, headers: Record<string, string> = {}) {
  const res = await fetch(`${baseURL}/api/debug/request`, { headers });
  expect(res.status).toBe(200);
  const text = await res.text();
  return { res, text, body: JSON.parse(text) as Echo };
}

test("echoes the whitelisted proxy headers and nothing else", async ({ server }) => {
  const { text, body } = await echo(server.baseURL, {
    "X-Forwarded-For": "100.64.12.34",
    "X-Real-IP": "100.64.12.34",
    "X-Forwarded-Proto": "https",
    Forwarded: "for=100.64.12.34;proto=https",
    Via: "1.1 proxy.example",
    "User-Agent": "sure-api-tests",
    "CF-Ray": "0123456789abcdef-AKL",
    Cookie: `session=${SECRET}`,
    Authorization: `Bearer ${SECRET}`,
    "CF-Access-Jwt-Assertion": SECRET,
    "CF-Access-Client-Secret": SECRET,
    "X-Api-Key": SECRET,
  });

  expect(text).not.toContain(SECRET);
  const names = body.headers.map((h) => h.name);
  for (const name of [
    "host",
    "forwarded",
    "x-forwarded-for",
    "x-real-ip",
    "x-forwarded-proto",
    "via",
    "user-agent",
    "cf-ray",
  ]) {
    expect(names, name).toContain(name);
  }
  // A whitelist, not a blocklist: headers that are harmless but unlisted stay out too.
  for (const name of ["cookie", "authorization", "cf-access-jwt-assertion", "x-api-key", "accept", "sec-fetch-mode"]) {
    expect(names, name).not.toContain(name);
  }
  expect(body.headers.find((h) => h.name === "user-agent")?.value).toBe("sure-api-tests");
});

test("classifies each address the request carried", async ({ server }) => {
  const { body } = await echo(server.baseURL, {
    "X-Forwarded-For": "203.0.113.9, 192.168.7.8",
    "X-Real-IP": "100.64.12.34",
  });
  expect(body.addresses).toEqual([
    { source: "x_forwarded_for", ip: "203.0.113.9", network: "public" },
    { source: "x_forwarded_for", ip: "192.168.7.8", network: "lan" },
    { source: "x_real_ip", ip: "100.64.12.34", network: "tailscale" },
    { source: "peer", ip: "127.0.0.1", network: "loopback" },
  ]);
  expect(body.client).toEqual(body.addresses[0]);
});

test("with no proxy in front, the client is the TCP peer", async ({ server }) => {
  const { body } = await echo(server.baseURL);
  expect(body.client).toEqual({ source: "peer", ip: "127.0.0.1", network: "loopback" });
  expect(body.http_version).toBe("HTTP/1.1");
});

test("reports the server's clock and version, and is never stored", async ({ server }) => {
  const before = Date.now();
  const { res, body } = await echo(server.baseURL);
  const after = Date.now();

  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(res.headers.get("etag")).toBeNull();
  // Same machine, so the same clock, to within the request.
  expect(body.server_time_ms).toBeGreaterThanOrEqual(before - 5);
  expect(body.server_time_ms).toBeLessThanOrEqual(after + 5);
  expect(new Date(body.server_time).getTime()).toBe(body.server_time_ms);

  const health = await fetch(`${server.baseURL}/api/health`);
  expect(body.version).toBe(((await health.json()) as Schemas["Health"]).version);
});
