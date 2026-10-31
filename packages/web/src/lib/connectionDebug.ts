// What the connection page (pages/Debug.svelte) measures: what this launch cost, request by
// request, and what the path to the server looks like now.
//
// It reads only what the browser already records (Navigation, Resource and Paint Timing, and the
// marks in launchTiming.ts), what a few tiny requests take, and what the server saw of one
// (`GET /api/debug/request`). A field the browser does not expose is shown as "not exposed",
// never as a zero: a zero is a measurement.

import { api, type Schemas } from "./api";
import { keptStats } from "./lastKnown";
import { FRESH_DRAWN, KEPT_DRAWN } from "./launchTiming";

export const NOT_EXPOSED = "not exposed";

/** A label and a value; a line of its own; or a table whose first row is the header. */
export type Row = { k: string; v: string } | { note: string } | { table: string[][] };
export type Section = { title: string; rows: Row[] };

// ---- formatting ---------------------------------------------------------------------------

/** Milliseconds: whole above 10, one decimal below, a dash for nothing to report. */
export function ms(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "–";
  return n >= 10 ? String(Math.round(n)) : n.toFixed(1);
}

function bytes(n: number | null): string {
  if (n === null) return NOT_EXPOSED;
  if (n < 1000) return `${n} B`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)} kB`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)} MB`;
  return `${(n / 1_000_000_000).toFixed(1)} GB`;
}

/** Bytes in a table cell or a crowded row: `840`, `2.1k`, `1.3M`. */
function kb(n: number | null): string {
  if (n === null) return NOT_EXPOSED;
  if (n < 1000) return String(n);
  return n < 1_000_000 ? `${(n / 1000).toFixed(1)}k` : `${(n / 1_000_000).toFixed(1)}M`;
}

function ago(at: number): string {
  const mins = Math.round((Date.now() - at) / 60_000);
  if (mins < 1) return "just now";
  if (mins < 120) return `${mins} min ago`;
  const hours = Math.round(mins / 60);
  return hours < 48 ? `${hours} h ago` : `${Math.round(hours / 24)} d ago`;
}

const yesNo = (b: boolean) => (b ? "yes" : "no");

function clock(at: number): string {
  return new Date(at).toLocaleTimeString("en-GB", { hour12: false });
}

/** Plain text for pasting: one `## title` block per section, labels padded into a column. */
export function toText(sections: Section[]): string {
  const out: string[] = [];
  for (const section of sections) {
    out.push(`## ${section.title}`);
    const width = Math.max(0, ...section.rows.map((r) => ("k" in r ? r.k.length : 0)));
    for (const row of section.rows) {
      if ("k" in row) out.push(`${row.k.padEnd(width)}  ${row.v}`);
      else if ("note" in row) out.push(row.note);
      else {
        const widths = row.table[0].map((_, c) => Math.max(...row.table.map((r) => r[c].length)));
        for (const r of row.table) {
          out.push(r.map((cell, c) => (c === 0 ? cell.padEnd(widths[c]) : cell.padStart(widths[c]))).join("  "));
        }
      }
    }
    out.push("");
  }
  return out.join("\n");
}

// ---- one request --------------------------------------------------------------------------

/** One request's Resource Timing, reduced to what explains its latency. Milliseconds. */
export type RequestTiming = {
  /** Path and query, without the origin. */
  path: string;
  /** From navigation start. */
  start: number;
  /** `nextHopProtocol`; `null` when the browser reported none. */
  protocol: string | null;
  /** Answered without a connection at all, from the HTTP cache. */
  cached: boolean;
  /** Answered `304`: the body came from the HTTP cache after the server confirmed it. */
  revalidated: boolean;
  /** Opened a connection of its own rather than reusing one. */
  newConnection: boolean;
  /** In the service worker before the network, starting it included; `null` with no worker. */
  worker: number | null;
  /** Waiting for a free connection. HTTP/1.1's six per host shows up here. */
  queue: number;
  dns: number;
  tcp: number;
  /** `null` over plain HTTP. */
  tls: number | null;
  /** Request sent to first byte: one round trip plus the server's time. */
  ttfb: number | null;
  download: number | null;
  total: number;
  /** The server's own time, from `Server-Timing: app;dur=`; `null` when the response had none. */
  app: number | null;
  /** `ttfb − app`: what the network cost. */
  network: number | null;
  transfer: number | null;
  encoded: number | null;
  decoded: number | null;
};

const proto = PerformanceResourceTiming.prototype;
/** WebKit has had these since Safari 16.4; older engines have no such property at all. */
const SERVER_TIMING_EXPOSED = "serverTiming" in proto;
const SIZES_EXPOSED = "transferSize" in proto;

export function timingOf(e: PerformanceResourceTiming): RequestTiming {
  const url = new URL(e.name, location.href);
  const dns = e.domainLookupEnd - e.domainLookupStart;
  const connect = e.connectEnd - e.connectStart;
  const tlsStart = e.secureConnectionStart;
  const cached = SIZES_EXPOSED && e.transferSize === 0 && e.decodedBodySize > 0;
  // Resource Timing counts a response's headers as a flat 300 bytes, so a `200` transfers its
  // body plus 300 and a `304` transfers 300 alone while reporting the cached body's size.
  // (`responseStatus` would say so directly; WebKit does not expose it.)
  const revalidated =
    SIZES_EXPOSED && e.transferSize > 0 && e.encodedBodySize > 0 && e.transferSize < e.encodedBodySize + 300;
  const ttfb = e.requestStart > 0 ? e.responseStart - e.requestStart : null;
  const app = SERVER_TIMING_EXPOSED
    ? (e.serverTiming.find((t) => t.name === "app")?.duration ?? null)
    : null;
  return {
    path: url.pathname + url.search,
    start: e.startTime,
    protocol: e.nextHopProtocol || null,
    cached,
    revalidated,
    newConnection: !cached && connect > 0,
    worker: e.workerStart > 0 ? Math.max(0, e.fetchStart - e.workerStart) : null,
    queue: e.requestStart > 0 ? Math.max(0, e.requestStart - e.fetchStart - dns - connect) : 0,
    dns,
    tcp: tlsStart > 0 ? tlsStart - e.connectStart : connect,
    tls: url.protocol === "https:" ? (tlsStart > 0 ? e.connectEnd - tlsStart : 0) : null,
    ttfb,
    download: e.responseStart > 0 ? e.responseEnd - e.responseStart : null,
    total: e.duration,
    app,
    network: ttfb !== null && app !== null ? ttfb - app : null,
    transfer: SIZES_EXPOSED ? e.transferSize : null,
    encoded: SIZES_EXPOSED ? e.encodedBodySize : null,
    decoded: SIZES_EXPOSED ? e.decodedBodySize : null,
  };
}

function connLabel(t: RequestTiming): string {
  return t.cached ? "–" : t.newConnection ? "new" : "reuse";
}

/** Where the body came from: the network (`200`), the cache after a `304`, or the cache alone. */
function viaLabel(t: RequestTiming): string {
  return t.cached ? "cache" : t.revalidated ? "304" : "net";
}

/** "h2", or "h2 ×5 · http/1.1 ×2" when they differ. */
function protocols(timings: RequestTiming[]): string {
  const counts = new Map<string, number>();
  for (const t of timings) {
    const p = t.protocol ?? NOT_EXPOSED;
    counts.set(p, (counts.get(p) ?? 0) + 1);
  }
  if (counts.size === 1) return [...counts.keys()][0];
  return [...counts].map(([p, n]) => `${p} ×${n}`).join(" · ");
}

function setup(t: RequestTiming): string {
  return `dns ${ms(t.dns)} · tcp ${ms(t.tcp)} · tls ${t.tls === null ? "–" : ms(t.tls)}`;
}

const appCell = (t: RequestTiming) => (SERVER_TIMING_EXPOSED ? ms(t.app) : "n/e");

// ---- this launch --------------------------------------------------------------------------

const mark = (name: string) => performance.getEntriesByName(name, "mark")[0]?.startTime ?? null;

export function launchSection(): Section {
  const nav = performance.getEntriesByType("navigation")[0] as PerformanceNavigationTiming | undefined;
  const fcp = performance.getEntriesByName("first-contentful-paint", "paint")[0]?.startTime ?? null;
  const kept = mark(KEPT_DRAWN);
  const fresh = mark(FRESH_DRAWN);
  const rows: Row[] = [
    { k: "loaded", v: `${clock(performance.timeOrigin)} · ${ago(performance.timeOrigin)}` },
  ];
  if (!nav) return { title: "Launch", rows: [...rows, { k: "navigation", v: NOT_EXPOSED }] };

  const viaWorker = nav.workerStart > 0;
  const delivery = (nav as unknown as { deliveryType?: string }).deliveryType;
  let source: string;
  if (!SIZES_EXPOSED) source = NOT_EXPOSED;
  else if (nav.transferSize > 0) source = "network";
  else source = viaWorker ? "service worker" : "cache";
  if (delivery) source += ` · ${delivery}`;

  rows.push(
    { k: "navigation", v: nav.type },
    { k: "document", v: source },
    { k: "protocol", v: nav.nextHopProtocol || (source === "network" ? NOT_EXPOSED : "–") },
    { k: "sw → fetch", v: viaWorker ? `${ms(nav.fetchStart - nav.workerStart)} ms` : "no worker" },
    { k: "doc response", v: `${ms(nav.responseEnd)} ms` },
    { k: "first paint", v: fcp === null ? NOT_EXPOSED : `${ms(fcp)} ms` },
    { k: "dom ready", v: `${ms(nav.domContentLoadedEventEnd)} ms` },
    {
      k: "kept figures",
      v: kept !== null ? `${ms(kept)} ms` : fresh !== null ? "none kept" : "not recorded",
    },
    { k: "fresh figures", v: fresh !== null ? `${ms(fresh)} ms` : "not recorded" },
  );
  return { title: "Launch", rows };
}

/**
 * The API requests of this launch: everything under `/api` that started before the fresh
 * figures were drawn, or before this page opened when they never were (a launch on another
 * page). The page's own requests come after either.
 */
export function launchRequests(openedAt: number): PerformanceResourceTiming[] {
  const end = mark(FRESH_DRAWN) ?? openedAt;
  return (performance.getEntriesByType("resource") as PerformanceResourceTiming[]).filter(
    (e) => new URL(e.name, location.href).pathname.startsWith("/api/") && e.startTime < end,
  );
}

/** Short enough for a phone column: `reports/balances?to=…` → `balances?`. */
function shortName(path: string): string {
  const [route, query] = path.replace(/^\/api\//, "").replace(/^reports\//, "").split("?");
  return (query ? `${route}?` : route).slice(0, 14);
}

export function requestsSection(entries: PerformanceResourceTiming[]): Section {
  const timings = entries.map(timingOf);
  if (timings.length === 0) return { title: "Launch requests", rows: [{ note: "none recorded" }] };
  const table = [
    ["request", "conn", "via", "q", "sw", "ttfb", "app", "net"],
    ...timings.map((t) => [
      shortName(t.path),
      connLabel(t),
      viaLabel(t),
      ms(t.queue),
      ms(t.worker),
      ms(t.ttfb),
      appCell(t),
      ms(t.network),
    ]),
  ];
  const fresh = timings.filter((t) => t.newConnection);
  const sum = (pick: (t: RequestTiming) => number | null) => {
    const values = timings.map(pick);
    return values.some((v) => v === null) ? null : values.reduce<number>((a, v) => a + (v ?? 0), 0);
  };
  const rows: Row[] = [
    { k: "protocol", v: protocols(timings) },
    { k: "connections", v: `${fresh.length} new · ${timings.length - fresh.length} reused or cached` },
    { table },
    ...fresh.map((t) => ({ k: `${shortName(t.path)}`, v: setup(t) })),
    {
      k: "bytes",
      v: `wire ${kb(sum((t) => t.transfer))} · body ${kb(sum((t) => t.encoded))} · decoded ${kb(sum((t) => t.decoded))}`,
    },
  ];
  if (!SERVER_TIMING_EXPOSED) rows.push({ note: `app, net: Server-Timing ${NOT_EXPOSED}` });
  return { title: "Launch requests", rows };
}

// ---- the probe ----------------------------------------------------------------------------

export type Sample = { wall: number; timing: RequestTiming | null; entry: PerformanceResourceTiming | null };

async function probeOnce(url: string): Promise<Sample> {
  const t0 = performance.now();
  const res = await fetch(url, { cache: "no-store" });
  await res.arrayBuffer();
  const wall = performance.now() - t0;
  const href = new URL(url, location.href).href;
  const entry =
    (performance.getEntriesByName(href, "resource")[0] as PerformanceResourceTiming | undefined) ?? null;
  return { wall, timing: entry && timingOf(entry), entry };
}

const probeUrl = (tag: string, i: number) => `/api/health?probe=${tag}-${i}`;

/** `n` tiny requests, each sent when the last has answered. */
export async function sequential(n: number): Promise<Sample[]> {
  const tag = Date.now().toString(36);
  const samples: Sample[] = [];
  for (let i = 0; i < n; i++) samples.push(await probeOnce(probeUrl(`s${tag}`, i)));
  return samples;
}

/** `n` tiny requests at once, and how long the last took to answer. */
export async function parallel(n: number): Promise<{ samples: Sample[]; wall: number }> {
  const tag = Date.now().toString(36);
  const t0 = performance.now();
  const samples = await Promise.all(Array.from({ length: n }, (_, i) => probeOnce(probeUrl(`p${tag}`, i))));
  return { samples, wall: performance.now() - t0 };
}

function quantile(sorted: number[], q: number): number {
  return sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))];
}

/** first, min, median, p90, max — the first in send order, the rest over what was measured. */
function spread(label: string, values: (number | null)[]): string[] {
  const known = values.filter((v): v is number => v !== null);
  if (known.length === 0) return [label, "–", "–", "–", "–", "–"];
  const sorted = [...known].sort((a, b) => a - b);
  return [
    label,
    ms(values[0]),
    ms(sorted[0]),
    ms(quantile(sorted, 0.5)),
    ms(quantile(sorted, 0.9)),
    ms(sorted[sorted.length - 1]),
  ];
}

export function probeSection(
  seq: Sample[] | null,
  burst: { samples: Sample[]; wall: number } | null,
): Section {
  if (!seq) return { title: "Probe", rows: [{ note: "running…" }] };
  const timingsOf = (samples: Sample[]) =>
    samples.map((s) => s.timing).filter((t): t is RequestTiming => t !== null);
  const seqTimings = timingsOf(seq);
  const parTimings = burst ? timingsOf(burst.samples) : [];
  const first = seq[0]?.timing;
  const table = [
    ["ms", "first", "min", "med", "p90", "max"],
    spread("seq wall", seq.map((s) => s.wall)),
    spread("seq net", seq.map((s) => s.timing?.network ?? null)),
  ];
  if (burst) {
    table.push(
      spread("par wall", burst.samples.map((s) => s.wall)),
      spread("par net", burst.samples.map((s) => s.timing?.network ?? null)),
    );
  }
  const opened = (timings: RequestTiming[], n: number) => `${timings.filter((t) => t.newConnection).length}/${n}`;
  // Waiting for a connection, or opening one: what a request spent before it could be sent.
  const waits = parTimings.map((t) => t.queue + (t.newConnection ? t.dns + t.tcp + (t.tls ?? 0) : 0));
  const rows: Row[] = [
    {
      k: "requests",
      v: `GET /api/health · ${seq.length} seq` + (burst ? ` · ${burst.samples.length} par` : ""),
    },
    { k: "protocol", v: protocols([...seqTimings, ...parTimings]) },
    { table },
    {
      k: "first",
      v: !first ? NOT_EXPOSED : first.newConnection ? `new connection · ${setup(first)}` : "reused connection",
    },
    {
      k: "new conns",
      v: `seq ${opened(seqTimings, seq.length)}` + (burst ? ` · par ${opened(parTimings, burst.samples.length)}` : ""),
    },
  ];
  if (burst) {
    rows.push({ k: "par total", v: `${ms(burst.wall)} ms · max wait ${ms(Math.max(0, ...waits))} ms` });
  }
  return { title: "Probe", rows };
}

// ---- the server's view --------------------------------------------------------------------

export type Echo = {
  data: Schemas["RequestEcho"];
  /** Local clock just before sending and just after the answer, for the skew. */
  sentAt: number;
  answeredAt: number;
};

export async function fetchEcho(): Promise<Echo | string> {
  const sentAt = Date.now();
  try {
    const { data, error } = await api.GET("/api/debug/request", {});
    const answeredAt = Date.now();
    if (!data) return typeof error === "object" ? JSON.stringify(error) : "request failed";
    return { data, sentAt, answeredAt };
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}

const NETWORK: Record<Schemas["NetworkClass"], string> = {
  loopback: "loopback",
  tailscale: "Tailscale",
  lan: "LAN",
  public: "public internet",
};

const SOURCE: Record<Schemas["AddressSource"], string> = {
  cf_connecting_ip: "cf-connecting-ip",
  x_forwarded_for: "x-forwarded-for",
  x_real_ip: "x-real-ip",
  peer: "tcp peer",
};

/**
 * Shown elsewhere on the page: the addresses as classified rows, the user agent under Device.
 * The raw values are all in the JSON copy.
 */
const SHOWN_ELSEWHERE = new Set(["cf-connecting-ip", "x-forwarded-for", "x-real-ip", "user-agent"]);

export function serverSection(echo: Echo | string | null): Section {
  const title = "Server's view";
  if (echo === null) return { title, rows: [{ note: "waiting for the probe…" }] };
  if (typeof echo === "string") return { title, rows: [{ k: "error", v: echo }] };
  const { data, sentAt, answeredAt } = echo;
  const half = (answeredAt - sentAt) / 2;
  // Rounded before taking the sign, so a skew too small to show reads +0.00 rather than −0.00.
  const skew = Math.round((data.server_time_ms - (sentAt + half)) / 10) / 100;
  const forwarded = data.headers.find((h) => h.name === "x-forwarded-http-version")?.value;
  const client = data.client;
  const rows: Row[] = [
    {
      k: "client",
      v: client ? `${NETWORK[client.network]} · ${client.ip} (${SOURCE[client.source]})` : NOT_EXPOSED,
    },
    // `client` is the first of these, so the rest are the hops behind it.
    ...data.addresses.slice(1).map((a) => ({ k: SOURCE[a.source], v: `${a.ip} · ${NETWORK[a.network]}` })),
    { k: "browser → proxy", v: forwarded ?? "not forwarded" },
    { k: "→ app", v: data.http_version },
    {
      k: "clock skew",
      v: `${skew >= 0 ? "+" : "−"}${Math.abs(skew).toFixed(2)} s ± ${(half / 1000).toFixed(2)}`,
    },
    { k: "server", v: data.version },
    ...data.headers.filter((h) => !SHOWN_ELSEWHERE.has(h.name)).map((h) => ({ k: h.name, v: h.value })),
    { note: "Direct vs DERP relay: not visible here. tailscale ping <device> on the server." },
  ];
  return { title, rows };
}

// ---- this device --------------------------------------------------------------------------

/** What can be read synchronously; {@link deviceDetails} adds the rest. */
export function deviceSection(insets: string, details: Row[] = []): Section {
  const stats = keptStats();
  const conn = (navigator as unknown as { connection?: { effectiveType?: string; rtt?: number; downlink?: number } })
    .connection;
  const standalone = (navigator as unknown as { standalone?: boolean }).standalone;
  const bundle = new URL(import.meta.url).pathname.split("/").pop() ?? "";
  return {
    title: "Device",
    rows: [
      { k: "build", v: `${__APP_VERSION__} · ${bundle}` },
      { k: "api schema", v: __API_SCHEMA_HASH__ },
      {
        k: "standalone",
        v:
          yesNo(matchMedia("(display-mode: standalone)").matches) +
          (standalone === undefined ? "" : ` · navigator.standalone ${standalone}`),
      },
      ...details,
      {
        k: "last-known",
        v:
          stats.entries === 0
            ? "empty"
            : `${stats.entries} · ${(stats.chars / 1000).toFixed(1)}k chars · oldest ${ago(stats.oldestAt!)}`,
      },
      { k: "online", v: yesNo(navigator.onLine) },
      {
        k: "connection",
        v: conn ? `${conn.effectiveType ?? "?"} · rtt ${conn.rtt ?? "?"} · ${conn.downlink ?? "?"} Mb/s` : NOT_EXPOSED,
      },
      { k: "viewport", v: `${innerWidth}×${innerHeight} · dpr ${devicePixelRatio}` },
      { k: "safe area", v: insets },
      {
        k: "time zone",
        v: `${Intl.DateTimeFormat().resolvedOptions().timeZone} · UTC${utcOffset()}`,
      },
      { k: "language", v: navigator.language },
      { k: "user agent", v: navigator.userAgent },
    ],
  };
}

function utcOffset(): string {
  const minutes = -new Date().getTimezoneOffset();
  const sign = minutes >= 0 ? "+" : "−";
  const abs = Math.abs(minutes);
  return `${sign}${String(Math.floor(abs / 60)).padStart(2, "0")}:${String(abs % 60).padStart(2, "0")}`;
}

/** The service worker and storage rows, which are only available asynchronously. */
export async function deviceDetails(): Promise<Row[]> {
  const rows: Row[] = [];
  const sw = navigator.serviceWorker as ServiceWorkerContainer | undefined;
  if (!sw) {
    rows.push({ k: "worker", v: "unavailable (not a secure context)" });
  } else {
    const registration = await sw.getRegistration().catch(() => undefined);
    rows.push({
      k: "worker",
      v: sw.controller ? `controls page · ${sw.controller.state}` : "not controlling",
    });
    const pending = registration?.waiting ? "waiting" : registration?.installing ? "installing" : "none";
    rows.push({ k: "update", v: pending });
  }
  const storage = navigator.storage as StorageManager | undefined;
  const persisted = storage?.persisted ? await storage.persisted().catch(() => null) : null;
  const estimate = storage?.estimate ? await storage.estimate().catch(() => null) : null;
  rows.push(
    { k: "persisted", v: persisted === null ? NOT_EXPOSED : yesNo(persisted) },
    {
      k: "storage",
      v: estimate ? `${bytes(estimate.usage ?? null)} of ${bytes(estimate.quota ?? null)}` : NOT_EXPOSED,
    },
  );
  return rows;
}

/** Everything as JSON: the rendered rows, plus the raw entries they came from. */
export function toJson(
  sections: Section[],
  raw: { launch: PerformanceResourceTiming[]; probe: Sample[]; echo: Echo | string | null },
): string {
  return JSON.stringify(
    {
      at: new Date().toISOString(),
      sections,
      navigation: performance.getEntriesByType("navigation")[0]?.toJSON() ?? null,
      marks: performance.getEntriesByType("mark").map((m) => m.toJSON()),
      launch: raw.launch.map((e) => e.toJSON()),
      probe: raw.probe.map((s) => ({ wall: s.wall, entry: s.entry?.toJSON() ?? null })),
      echo: raw.echo,
    },
    null,
    2,
  );
}
