import { test as base, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createSureClient, type SureClient } from "../client/src/index";
import { startProxy, type StartedProxy } from "./proxy";
import { decodeBody, type ProxyClient, type RecordedExchange, type Upstream } from "./proxy-client";

const here = path.dirname(fileURLToPath(import.meta.url)); // packages/api-tests
const REPO_ROOT = path.resolve(here, "..", "..");
// Whatever directory cargo wrote to, which is not always `target/`: a developer with
// CARGO_TARGET_DIR exported has global-setup build somewhere else entirely, and a hard-coded
// path would then look like a missing build.
const TARGET_DIR = process.env.CARGO_TARGET_DIR
  ? path.resolve(REPO_ROOT, process.env.CARGO_TARGET_DIR)
  : path.join(REPO_ROOT, "target");
const BIN = path.join(TARGET_DIR, "debug", "sure-api");
// Out of the same directory, and built by the same global-setup, so the suite finds both
// binaries or neither.
const PROXY_BIN = path.join(TARGET_DIR, "debug", "sure-testproxy");

/**
 * The proxy belonging to this Playwright worker, published module-side.
 *
 * `startServer` has to reach it, and cannot take it as an argument: it is exported and called
 * directly from spec bodies, most of which never mention the proxy — http.spec.ts's cache and
 * compression tests want a backend with one setting changed, not a proxy handle threaded through
 * twenty call sites to reach code that only needs the endpoint map. A module-level binding is the
 * right scope rather than a compromise — a Playwright worker is its own process, so this module
 * is instantiated once per worker and holds exactly one proxy.
 */
let workerProxy: StartedProxy | undefined;

/**
 * The environment that points a spawned backend at this worker's proxy.
 *
 * Every server this suite starts gets it, not just the ones in tests that care. That is the
 * point of the arrangement: the proxy runs in replay mode with no snapshots, so an outbound
 * call nobody stubbed is answered `503 {}` and never leaves the machine. "A test accidentally
 * reached a third-party API" becomes a loud, deterministic failure in the adapter instead of a
 * silent dependency on someone else's uptime — and the WARN the proxy logs names the method and
 * URI that went unstubbed.
 */
function proxyEnvironment(): Record<string, string> {
  if (!workerProxy) {
    throw new Error(
      "startServer was called before this worker's proxy came up. The `proxyHost` fixture is " +
        "`auto`, so every test has one — this can only happen from module scope, outside a test.",
    );
  }
  return workerProxy.env;
}

/**
 * The status the replay-miss handler answers with, and the body it answers with.
 *
 * `sure_testproxy::start`'s `on_replay_miss` builds exactly this — `503` with `{}` and
 * `application/json` — and `packages/providers/tests/proxy_contract.rs` asserts the shape, so
 * this is a mirror of a pinned contract rather than a guess. Both halves are matched below
 * because the status alone is ambiguous: a spec is free to *stub* a 503 (none does today), and
 * mistaking that for a miss would fail a test for asserting exactly what it meant to assert.
 */
const MISS_STATUS = 503;
const MISS_BODY = "{}";

/**
 * A replay miss the running test is content to produce.
 *
 * The matcher is the same shape as {@link ProxyClient.stub}'s, deliberately: an allowance is the
 * negative of a stub, and a reader comparing the two should not have to translate between two
 * spellings of "which request".
 */
export type UnstubbedAllowance = {
  upstream: Upstream;
  /** Regex against the request **path**, exactly as `stub`'s `path_pattern` — never the query. */
  path_pattern: string;
  /** Why this call is deliberately unanswered. Quoted in the failure message. */
  why: string;
};

/**
 * Allowances declared by the test currently running, cleared by `proxyIsolation` on the way in.
 *
 * Module-level for the same reason `workerProxy` is: a Playwright worker is its own process and
 * runs one test at a time, so "the current test" is a well-defined thing to hold here — and a
 * spec declaring one should not have to thread a fixture through to do it.
 */
let allowedUnstubbed: UnstubbedAllowance[] = [];

/**
 * Declare that this test expects a request nobody stubbed, and why.
 *
 * Without this, any outbound call the proxy answered with its replay miss fails the test — see
 * {@link failOnUnstubbedRequests}. A handful of tests genuinely want the miss: it is how they
 * assert that an unanswered upstream surfaces as a 502 rather than reaching the internet, or that
 * a retired `times: 1` stub makes an unwanted second call fail. Those say so here.
 *
 * Permission, not an expectation: nothing checks that the call actually happened. Several of the
 * misses this covers come from fire-and-forget background work whose request may or may not reach
 * the proxy before the test's server is killed, and a test that wants the stronger statement has
 * `assertCount`/`assertSeen` — which see a miss like any other exchange — to make it with.
 */
export function allowUnstubbed(allowance: UnstubbedAllowance): void {
  allowedUnstubbed.push(allowance);
}

/** The path half of a recorded `uri` (origin form: path + query). */
function pathOf(uri: string): string {
  const query = uri.indexOf("?");
  return query < 0 ? uri : uri.slice(0, query);
}

function isAllowed(exchange: RecordedExchange): boolean {
  return allowedUnstubbed.some(
    (allowance) =>
      allowance.upstream === exchange.upstream &&
      new RegExp(allowance.path_pattern).test(pathOf(exchange.request.uri)),
  );
}

/**
 * Fail the test if anything it did reached an upstream that no stub answered.
 *
 * The proxy already logs a WARN naming the method and URI of every such call, and a green run
 * used to print several — which made the line worthless as a signal, because a reader had to know
 * which of them were deliberate. A miss is not a harmless log line: the adapter got a 503 it did
 * not expect, so whatever the test thought it was exercising ran down an error path instead, and
 * the assertions that still passed passed for the wrong reason. It is also latent flakiness in two
 * directions — a miss is *recorded*, so it counts towards an `assertCount` filtered on the same
 * path, and an unstubbed call from a background task can arrive late enough to land in the next
 * test's traffic instead.
 *
 * So the default is that a miss fails the test that caused it, and a test that wants one says so
 * with {@link allowUnstubbed}. Run from `proxyIsolation`'s teardown, which is after the `server`
 * fixture's — that fixture depends on `proxyIsolation`, so the backend is already gone and
 * nothing can still be calling out.
 */
async function failOnUnstubbedRequests(client: ProxyClient): Promise<void> {
  const misses = (await client.queryTraffic({ status: MISS_STATUS })).filter(
    (exchange) =>
      exchange.outcome.kind === "response" && decodeBody(exchange.outcome.body) === MISS_BODY,
  );
  const unexpected = misses.filter((exchange) => !isAllowed(exchange));
  if (unexpected.length === 0) return;

  const listed = unexpected
    .map((e) => `  ${e.request.method} ${e.upstream ?? "?"} ${e.request.uri}`)
    .join("\n");
  const declared = allowedUnstubbed.length
    ? `\nThis test allows ${allowedUnstubbed
        .map((a) => `${a.upstream} ${a.path_pattern} (${a.why})`)
        .join(", ")}, which is not what arrived.`
    : "";
  throw new Error(
    `${unexpected.length} request(s) reached an upstream that no stub answered, so the proxy ` +
      `replied with its replay miss (${MISS_STATUS} ${MISS_BODY}) and the code under test took ` +
      `an error path:\n${listed}${declared}\n` +
      `Register a stub for each, or — if the unanswered call is the point of the test — declare ` +
      `it with allowUnstubbed({ upstream, path_pattern, why }) from the test body.`,
  );
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const port = (srv.address() as net.AddressInfo).port;
      srv.close(() => resolve(port));
    });
  });
}

async function waitForHealth(baseURL: string, timeoutMs = 10_000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(`${baseURL}/api/health`);
      if (res.ok) return;
    } catch {
      /* backend not up yet */
    }
    await new Promise((r) => setTimeout(r, 40));
  }
  throw new Error(`backend did not become healthy at ${baseURL}`);
}

type Server = { baseURL: string };

/** A backend spawned outside the fixture, for tests that need non-default configuration. */
export type StartedServer = Server & {
  proc: ReturnType<typeof spawn>;
  /** The temp directory holding this server's SQLite database. */
  dir: string;
  /**
   * Signal the process and remove its temp directory once it has actually exited. Safe to
   * call twice.
   *
   * Cleanup is deferred rather than immediate because a graceful stop is not instant: the
   * shutdown tests send `SIGTERM` and then assert on what the server did on its way out,
   * which it cannot do if its database has already been deleted underneath it.
   */
  stop: (signal?: NodeJS.Signals) => void;
  /** Resolve with the exit code, or `null` if the process was killed by a signal. */
  waitForExit: (timeoutMs?: number) => Promise<number | null>;
  /** Everything written to stdout and stderr. Empty unless `startServer` was given `capture`. */
  output: () => string;
};

/** Non-environment knobs for [`startServer`]. */
export type StartOptions = {
  /**
   * Buffer the backend's stdout/stderr so a test can assert on its log lines. Off by
   * default: the logs are noise around an ordinary API assertion, and piping them means
   * something has to keep draining the pipe.
   */
  capture?: boolean;
};

/**
 * Spawn the real `sure-api` binary on an ephemeral port against a throwaway temp-file
 * SQLite database.
 *
 * `env` overrides go on top of the defaults, which is how the HTTP-behaviour tests reach
 * configuration a normal run never sets — a tiny rate limit, a `WEB_DIR`, compression off.
 */
export async function startServer(
  env: Record<string, string> = {},
  options: StartOptions = {},
): Promise<StartedServer> {
  const port = await freePort();
  const dir = mkdtempSync(path.join(tmpdir(), "sure-e2e-"));
  const baseURL = `http://127.0.0.1:${port}`;
  const capture = Boolean(options.capture);
  // Deliberately unset rather than inherited: tests assert the specific "not configured"
  // error the Akahu provider returns when these are absent, which must hold regardless
  // of whatever the developer's own shell happens to have exported.
  const { AKAHU_APP_TOKEN, AKAHU_USER_TOKEN, ...envWithoutAkahu } = process.env;
  // Same reasoning, different blast radius: once a developer runs the observability stack,
  // `OTEL_EXPORTER_OTLP_ENDPOINT` lives in their shell — and every backend this suite spawns
  // would then build an SDK, start three exporter threads, and push this suite's traces and
  // metrics into their real VictoriaMetrics. Note the testproxy cannot catch that for us: it is
  // a reverse proxy with one listener per named `Upstream` (Frankfurter, Yahoo, Akahu, House
  // Pricer), so an exporter's connection never reaches it and `failOnUnstubbedRequests` never
  // sees it. Hence stripping rather than stubbing. `specs/telemetry.spec.ts` sets them back
  // deliberately.
  const otelVars = Object.keys(envWithoutAkahu).filter(
    (name) => name.startsWith("OTEL_") || name.startsWith("SURE_OTEL_"),
  );
  for (const name of otelVars) delete envWithoutAkahu[name];
  const proc = spawn(BIN, [], {
    env: {
      ...envWithoutAkahu,
      // ...and stripping them is only sufficient if the backend does not go and read a
      // `.env` of its own: it searches upward from the working directory, which from here
      // is the repo root, where a developer's real tokens live. Empty = don't load one.
      SURE_ENV_FILE: "",
      DATABASE_URL: `sqlite:${path.join(dir, "test.db")}`,
      BIND_ADDR: `127.0.0.1:${port}`,
      // Errors only. Not `warn`: the suite deliberately provokes plenty of those, and each
      // would land in the output of a test that is passing.
      RUST_LOG: "error",
      // No background scheduler. Its first check runs immediately, so the provider poll
      // would record an extra "error" sync row for any enabled provider a test just
      // created — a race the provider specs lost intermittently — and the exchange-rate
      // sweep nobody stubbed would put a replay-miss WARN in the output of every server
      // this suite spawns, which is one per test. Not for containment: `proxyEnvironment`
      // above already makes reaching a third party impossible, whatever the scheduler does.
      // `specs/shutdown.spec.ts` overrides this to drain a sweep that is really in flight.
      BACKGROUND_TASKS: "off",
      // Points every provider adapter at this worker's proxy, and opens the sandbox port
      // they need on Linux — see `proxyEnvironment` for why every server gets this. Before
      // `...env` so a spec can still override an individual endpoint; none does, and one that
      // did would be opting out of the guarantee above.
      ...proxyEnvironment(),
      ...env,
    },
    // The backend's own logs are noise around a test result, so they go nowhere unless
    // `capture` asks for them — which pipes them for the test itself to read.
    stdio: capture ? ["ignore", "pipe", "pipe"] : "ignore",
  });

  let logs = "";
  if (capture) {
    proc.stdout?.on("data", (chunk) => {
      logs += String(chunk);
    });
    proc.stderr?.on("data", (chunk) => {
      logs += String(chunk);
    });
  }

  const exited = new Promise<number | null>((resolve) => {
    proc.on("exit", (code) => resolve(code));
  });

  const waitForExit = async (timeoutMs = 15_000): Promise<number | null> => {
    let timer: NodeJS.Timeout | undefined;
    const expiry = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`server did not exit within ${timeoutMs}ms`)), timeoutMs);
    });
    try {
      return await Promise.race([exited, expiry]);
    } finally {
      clearTimeout(timer);
    }
  };

  const cleanup = () => rmSync(dir, { recursive: true, force: true });

  let stopped = false;
  const stop = (signal: NodeJS.Signals = "SIGKILL") => {
    if (stopped) {
      return;
    }
    stopped = true;
    // Already gone (a test awaited the exit itself): nothing to wait for.
    if (proc.exitCode !== null || proc.signalCode !== null) {
      cleanup();
      return;
    }
    void exited.then(cleanup);
    proc.kill(signal);
  };

  try {
    await waitForHealth(baseURL);
  } catch (err) {
    stop();
    throw err;
  }
  return { baseURL, proc, dir, stop, waitForExit, output: () => logs };
}

/**
 * Test-scoped fixtures. `proxyIsolation` is a dependency to declare, never a value to read —
 * Playwright builds the graph from what a fixture body destructures, so it has to be named there.
 *
 * `testproxy` and not the obvious `proxy`: Playwright's own `PlaywrightTestOptions` already
 * declares a `proxy` option (the browser's HTTP proxy, `{ server, bypass, … }`), and an override
 * has to keep the base type — so the name is taken, and taken by something a reader could
 * plausibly confuse this with. The spelling matches the binary it drives, `sure-testproxy`.
 */
type Fixtures = { server: Server; api: SureClient; testproxy: ProxyClient; proxyIsolation: void };

/** Worker-scoped fixtures. `proxyHost` is the process; `testproxy` above is the client onto it. */
type WorkerFixtures = { proxyHost: StartedProxy };

/**
 * Every test gets a fresh, isolated backend: the real `sure-api` binary bound to an
 * ephemeral port against a throwaway temp-file SQLite database. Mirrors the old Rust
 * harness, but exercised over HTTP through the generated client — so a test failure can
 * mean either the API *or* the client is wrong.
 *
 * Each backend is pointed at a `sure-testproxy` cluster standing in for every third-party host
 * `sure-providers` reaches, so no test in this suite can talk to the internet and any test can
 * say what an upstream returned. See `proxyEnvironment` above for the reasoning, and
 * `proxyHost`/`proxyIsolation` below for the scoping that keeps concurrent tests out of each
 * other's traffic.
 */
export const test = base.extend<Fixtures, WorkerFixtures>({
  /**
   * One proxy process per worker — not per test, and not one for the whole run.
   *
   * Per test would be ~150 processes for a suite that spawns a backend per test already. One
   * shared by the whole run would be wrong rather than merely wasteful: `fullyParallel` is on,
   * and the recorder's ring and the stub table are *cluster-wide*, so two tests running at once
   * would see each other's exchanges and one `assertCount` would count the other's traffic. A
   * worker runs exactly one test at a time, which makes per-worker both safe and cheap.
   *
   * `auto`, because a test that never mentions the proxy still gets a backend pointed at it —
   * every server in this suite is, deliberately — and because `startServer` reads the module
   * binding this fixture sets. A non-auto fixture's body only runs when something depends on it,
   * and most tests here depend on nothing: five of shutdown.spec.ts's six never name it, and
   * `proxyEnvironment` would throw on their first `startServer` rather than quietly spawning a
   * backend with no endpoints.
   */
  proxyHost: [
    async ({}, use) => {
      const proxy = await startProxy(PROXY_BIN);
      workerProxy = proxy;
      try {
        await use(proxy);
      } finally {
        workerProxy = undefined;
        await proxy.stop();
      }
    },
    { scope: "worker", auto: true },
  ],

  /**
   * Hand each test a proxy in a known state.
   *
   * The state is per-cluster and the cluster outlives the test, so without this a stub with no
   * `times` limit would answer a later test's request, and `queryTraffic` would return exchanges
   * from a test that has already reported. `resume` comes first for the worst of the three: a
   * test that paused an upstream and then failed before resuming would otherwise hand the next
   * test a proxy that holds every request until Playwright's timeout, with nothing in the
   * failure naming the previous test.
   *
   * Reset on the way *in* rather than the way out, so it covers the first test in a worker and
   * cannot turn a failing test into a confusing teardown error.
   *
   * The one thing that *does* happen on the way out is the unstubbed-request check
   * ({@link failOnUnstubbedRequests}), which has to: it is a statement about what this test did,
   * and the recordings are cleared by the next test's reset. Skipped when the test has already
   * failed — the real failure is the headline, the proxy's WARN lines are in the output either
   * way, and a missing stub is usually *why* such a test failed rather than a second finding.
   */
  proxyIsolation: [
    async ({ proxyHost }, use, testInfo) => {
      allowedUnstubbed = [];
      await proxyHost.client.resume();
      await proxyHost.client.clearStubs();
      await proxyHost.client.clearRecordings();
      await use();
      if (testInfo.status === testInfo.expectedStatus) {
        await failOnUnstubbedRequests(proxyHost.client);
      }
    },
    { auto: true },
  ],

  /**
   * The control plane: register stubs, assert on traffic, read what was actually requested.
   *
   * The same client the whole worker shares — commands are serialised on it — scoped to a test
   * only by `proxyIsolation` having just cleared it.
   */
  testproxy: async ({ proxyHost, proxyIsolation }, use) => {
    void proxyIsolation;
    await use(proxyHost.client);
  },

  // Depends on `proxyIsolation` explicitly rather than trusting Playwright to order two
  // same-scope fixtures: a reset that ran *after* a backend was up would clear whatever that
  // backend did on the way up. This one boots with `BACKGROUND_TASKS` off and so does nothing on
  // the way up — but that is a property of the default environment, not of the fixture graph, and
  // the graph is where the ordering belongs.
  server: async ({ proxyIsolation }, use) => {
    void proxyIsolation;
    const started = await startServer();
    try {
      await use({ baseURL: started.baseURL });
    } finally {
      started.stop();
    }
  },
  api: async ({ server }, use) => {
    await use(createSureClient(server.baseURL));
  },
});

export { expect };

// Re-exported so a spec needs one import for the whole harness. `decodeBody` turns a recorded
// body (base64 on the wire) back into text; `createSureClient` is for a spec that spawns its own
// backend with `startServer` and still wants the typed client the `api` fixture would have given
// it.
export { createSureClient };
export { decodeBody } from "./proxy-client";
export type {
  AssertionResult,
  ExchangeOutcome,
  RecordedExchange,
  RecordedRequest,
  StubOptions,
  TrafficFilter,
  Upstream,
} from "./proxy-client";
