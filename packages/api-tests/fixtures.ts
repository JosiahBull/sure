import { test as base, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createSureClient, type SureClient } from "../client/src/index";
import { startProxy, type StartedProxy } from "./proxy";
import type { ProxyClient } from "./proxy-client";

const here = path.dirname(fileURLToPath(import.meta.url)); // packages/api-tests
const REPO_ROOT = path.resolve(here, "..", "..");
// A detector run (`pnpm test:api:blocked`) builds into its own target dir, so the binary
// global-setup just built is not always target/debug — see scripts/blocked.mjs.
const TARGET_DIR = process.env.CARGO_TARGET_DIR
  ? path.resolve(REPO_ROOT, process.env.CARGO_TARGET_DIR)
  : path.join(REPO_ROOT, "target");
const BIN = path.join(TARGET_DIR, "debug", "sure-api");
// Out of the same directory, and built by the same global-setup, so a detector run finds both
// binaries or neither — resolving the proxy from a hard-coded `target/debug` would have worked
// locally and then looked like a missing build under `pnpm test:api:blocked`.
const PROXY_BIN = path.join(TARGET_DIR, "debug", "sure-testproxy");
const BLOCKED = Boolean(process.env.SURE_BLOCKED);

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
  const proc = spawn(BIN, [], {
    env: {
      ...envWithoutAkahu,
      // ...and stripping them is only sufficient if the backend does not go and read a
      // `.env` of its own: it searches upward from the working directory, which from here
      // is the repo root, where a developer's real tokens live. Empty = don't load one.
      SURE_ENV_FILE: "",
      DATABASE_URL: `sqlite:${path.join(dir, "test.db")}`,
      BIND_ADDR: `127.0.0.1:${port}`,
      // Silent by default. Under the blocking detector the whole point is the WARN lines
      // it emits, so let those through — plus the detector's own startup warnings, which
      // are how a build that can't report anything says so instead of looking clean. Not a
      // bare `warn`: that would add every unrelated warning the suite deliberately
      // provokes, and the detector's "active" INFO line once per spawned server.
      RUST_LOG: BLOCKED ? "error,tokio_blocked=warn,sure_api::telemetry=warn" : "error",
      // No background scheduler. Its first check runs immediately, so the provider poll
      // would record an extra "error" sync row for any enabled provider a test just
      // created — a race the provider specs lost intermittently — and the exchange-rate
      // sweep nobody stubbed would put a replay-miss WARN in the output of every server
      // this suite spawns, which is one per test. Not for containment: `proxyEnvironment`
      // above already makes reaching a third party impossible, whatever the scheduler does.
      // `specs/shutdown.spec.ts` overrides this to drain a sweep that is really in flight.
      BACKGROUND_TASKS: "off",
      // Points all three provider adapters at this worker's proxy, and opens the sandbox port
      // they need on Linux — see `proxyEnvironment` for why every server gets this. Before
      // `...env` so a spec can still override an individual endpoint; none does, and one that
      // did would be opting out of the guarantee above.
      ...proxyEnvironment(),
      ...env,
    },
    // Normally the backend's own logs are noise around a test result. Under the detector
    // they *are* the result, so let them through (the tracing subscriber writes to stdout)
    // — Playwright attributes the output to the test that was running. `capture` keeps
    // them for the test to read instead.
    stdio: capture ? ["ignore", "pipe", "pipe"] : BLOCKED ? ["ignore", "inherit", "inherit"] : "ignore",
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
   */
  proxyIsolation: [
    async ({ proxyHost }, use) => {
      await proxyHost.client.resume();
      await proxyHost.client.clearStubs();
      await proxyHost.client.clearRecordings();
      await use();
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
