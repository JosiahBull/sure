// Spawning `sure-testproxy` and reading what it says it bound.
//
// Everything about *when* a proxy is started lives in fixtures.ts (one per Playwright worker);
// this file is only concerned with the process itself: bring it up in the configuration that
// cannot reach the internet, read the one line it prints, and hand back both the control-plane
// client and the environment that points a `sure-api` at its listeners.
//
// The handshake is the whole interface. `sure-testproxy` binds an ephemeral port per upstream
// and prints their addresses already joined with each upstream's path prefix
// (`packages/testproxy/src/main.rs::announce`), so nothing here — and nothing in a spec — has
// to know that Yahoo's charts sit under `/v8/finance/chart` or that a prefix exists at all.

import { spawn, type ChildProcess } from "node:child_process";

import { ProxyClient } from "./proxy-client";

/**
 * How long to wait for the handshake line.
 *
 * Generous, because the first thing this process does after `execve` is bind four listeners,
 * and a loaded CI box running four workers starts four of them at once. Short enough that a
 * binary which is missing, or built from a revision that never prints a handshake, fails the
 * run with a message instead of hanging until Playwright's own timeout.
 */
const HANDSHAKE_TIMEOUT_MS = 15_000;

/** How long a stopped proxy gets to exit on its own before it is killed. */
const EXIT_TIMEOUT_MS = 5_000;

/**
 * Bytes of the proxy's stderr kept for a startup error message.
 *
 * Bounded because this buffer lives as long as the worker does, and a suite that forgot a stub
 * logs a replay-miss WARN per unmatched request. The forwarding below is what makes those
 * visible; this copy exists only so a failure *before* the handshake can quote the reason.
 */
const DIAGNOSTICS_LIMIT = 64 * 1024;

/**
 * The single line of JSON `sure-testproxy` writes to stdout, and the only thing it writes
 * there — its logs go to stderr precisely so this parse cannot be broken by one.
 *
 * Mechanically aligned with `announce` in packages/testproxy/src/main.rs. A renamed field is
 * not a type error on this side, so {@link parseHandshake} checks the shape at runtime.
 */
type Handshake = {
  /** `"record"` or `"replay"`, echoed back so a harness can assert on what it asked for. */
  mode: string;
  /** `SocketAddr` of the JSON-Lines control plane. */
  control: string;
  /** Upstream name -> `"127.0.0.1:<port>"`, without the path prefixes. */
  upstreams: Record<string, string>;
  /** `FRANKFURTER_BASE_URL` and its two siblings, path prefixes already joined on. */
  env: Record<string, string>;
};

/** A running `sure-testproxy`, and everything a test harness needs from it. */
export type StartedProxy = {
  /** The control plane: stubs, traffic queries, blocking assertions. */
  client: ProxyClient;
  /**
   * Environment that points a `sure-api` at this cluster — the three `*_BASE_URL` values plus
   * the Landlock exemption they need on Linux. Pass it straight through to `spawn`.
   */
  env: Record<string, string>;
  /** Close the control socket and stop the process. Safe to call twice. */
  stop: () => Promise<void>;
};

/**
 * Start a proxy cluster and connect to its control plane.
 *
 * `binary` is passed in rather than resolved here so that "where cargo put the binaries" stays
 * a single decision in fixtures.ts, which has to resolve `sure-api` out of the same directory.
 */
export async function startProxy(binary: string): Promise<StartedProxy> {
  const proc = spawn(binary, [], {
    // stdin is a pipe and is deliberately left open for the life of the worker: the proxy stops
    // when it closes, and that is the *only* notification it gets when a worker is killed
    // rather than torn down — a Playwright timeout, a `^C`, a crashed shard. Without it an
    // orphaned proxy keeps a control port and four listeners and poisons the next run. See
    // `wait_for_stop` in packages/testproxy/src/main.rs.
    stdio: ["pipe", "pipe", "pipe"],
    env: {
      ...process.env,
      // Stated, never inherited. `record` mode dials the real upstreams with whatever
      // credentials are in the environment and reports the result as a passing test, so the
      // one variable that must not arrive from a developer's shell — where it is plausibly set
      // for a recording run — is this one.
      SURE_TESTPROXY_MODE: "replay",
      // Stubs only: no snapshot is loaded and none is written. This is the strongest form of
      // the no-internet guarantee — with no storage attached there is nowhere an answer could
      // come from except a stub the running test registered, and everything else is the 503
      // miss. It also keeps a suite run from leaving `.ndjson` files in the tree.
      SURE_TESTPROXY_SNAPSHOT_DIR: "",
      // Ephemeral, and stated for the same reason as the mode: an inherited fixed port would
      // make the second worker's proxy fail to bind, intermittently and with a message about
      // addresses rather than about workers.
      SURE_TESTPROXY_CONTROL_BIND: "127.0.0.1:0",
      // `warn` is where the replay-miss line lives, and that line — the method and URI of the
      // call nobody stubbed — is the diagnosis for the failure this whole arrangement is built
      // to produce. Quiet otherwise, so a passing run says nothing.
      RUST_LOG: "warn",
    },
  });

  let diagnostics = "";
  proc.stderr?.setEncoding("utf8");
  proc.stderr?.on("data", (chunk: string) => {
    // Forwarded so a replay miss lands in the reporter's output attributed to the test that
    // caused it, and copied (up to a bound) so a failure during startup can quote why.
    process.stderr.write(chunk);
    if (diagnostics.length < DIAGNOSTICS_LIMIT) diagnostics += chunk;
  });

  const exited = new Promise<void>((resolve) => proc.once("exit", () => resolve()));

  let handshake: Handshake;
  try {
    handshake = parseHandshake(await firstStdoutLine(proc, () => diagnostics));
  } catch (err) {
    proc.kill("SIGKILL");
    throw err;
  }

  const control = splitAddr(handshake.control, "control");
  let client: ProxyClient;
  try {
    client = await ProxyClient.connect({
      host: control.host,
      port: control.port,
      // No inactivity timeout. The vendored client's default (30s) is a *read* timeout that
      // fires on an idle socket and then poisons the client permanently — which for a
      // per-worker client that sits quiet through one long test is a guaranteed intermittent
      // failure, not a safety net. Liveness is already covered from three directions: the
      // socket's `close` event when the proxy dies, the command window each blocking assertion
      // carries, and Playwright's own per-test timeout.
      socketTimeoutMs: 0,
    });
  } catch (err) {
    proc.kill("SIGKILL");
    throw new Error(`could not reach the sure-testproxy control plane at ${handshake.control}: ${err}`);
  }

  let stopped = false;
  const stop = async (): Promise<void> => {
    if (stopped) return;
    stopped = true;
    // Closed first, so the client marks the shutdown as expected instead of manufacturing an
    // error for a caller that has already finished with it.
    await client.close().catch(() => undefined);
    proc.stdin?.end();
    const timer = setTimeout(() => proc.kill("SIGKILL"), EXIT_TIMEOUT_MS);
    try {
      await exited;
    } finally {
      clearTimeout(timer);
    }
  };

  return { client, env: serverEnvironment(handshake), stop };
}

// That the traffic ring is live at all — `assertCount`/`queryTraffic` reading a real exchange
// rather than answering zero — is pinned by packages/testproxy/tests/recording.rs, which fails
// first: both the pre-commit hook and CI run `cargo test` before `pnpm test:api`. The same file
// pins the limit: a clock-derived query parameter is `CANONICAL` by the time the recorder sees it.

/**
 * The environment a `sure-api` needs to talk to this cluster and nothing else.
 *
 * The three `*_BASE_URL` values come straight from the handshake. `SURE_SANDBOX_CONNECT_PORTS`
 * is the part that is easy to leave out and expensive to debug: on Linux the server's default
 * Landlock policy permits outbound TCP on 443 and 53 only (`packages/server/src/sandbox.rs`),
 * deliberately *not* deriving anything from the configured endpoints — so a proxy on an
 * ephemeral port is simply unreachable, and the failure surfaces as an ordinary connection
 * error from whichever adapter was pointed at it, naming nothing about Landlock. It is a no-op
 * on macOS, which is exactly why it has to be set here rather than remembered: everything is
 * green locally and red in CI otherwise.
 *
 * The control port is not listed. Nothing inside the sandbox talks to it — the control plane's
 * only client is this Node process, which Landlock does not mediate.
 */
function serverEnvironment(handshake: Handshake): Record<string, string> {
  const ports = Object.entries(handshake.upstreams).map(
    ([name, addr]) => splitAddr(addr, `upstream ${name}`).port,
  );
  return { ...handshake.env, SURE_SANDBOX_CONNECT_PORTS: ports.join(",") };
}

/**
 * Read the handshake, or fail saying which way it went wrong.
 *
 * Three distinguishable failures, because they have three different fixes: the binary would not
 * start (rebuild, or `pnpm test:api` from the wrong directory), it started and died (its stderr
 * is quoted), or it is alive and said nothing (a revision that does not announce itself).
 */
function firstStdoutLine(proc: ChildProcess, diagnostics: () => string): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    let buffered = "";

    const fail = (message: string) => {
      finish();
      const stderr = diagnostics().trim();
      reject(new Error(stderr ? `${message}\n--- sure-testproxy stderr ---\n${stderr}` : message));
    };
    const onData = (chunk: string) => {
      buffered += chunk;
      const end = buffered.indexOf("\n");
      if (end < 0) return;
      finish();
      // Anything after the newline is discarded, and the stream is left flowing: the proxy
      // never writes a second line, but a paused pipe that filled up would block it mid-log.
      proc.stdout?.resume();
      resolve(buffered.slice(0, end));
    };
    const onExit = (code: number | null, signal: NodeJS.Signals | null) =>
      fail(`sure-testproxy exited (code ${code}, signal ${signal}) before printing its handshake`);
    const onError = (err: Error) => fail(`could not start sure-testproxy: ${err.message}`);
    const timer = setTimeout(
      () => fail(`sure-testproxy printed no handshake within ${HANDSHAKE_TIMEOUT_MS}ms`),
      HANDSHAKE_TIMEOUT_MS,
    );

    function finish() {
      clearTimeout(timer);
      proc.stdout?.off("data", onData);
      proc.off("exit", onExit);
      proc.off("error", onError);
    }

    proc.stdout?.setEncoding("utf8");
    proc.stdout?.on("data", onData);
    proc.on("exit", onExit);
    proc.on("error", onError);
  });
}

/**
 * Parse and check the handshake line.
 *
 * The mode assertion is not defensive padding: `replay` is the property the whole suite rests
 * on — no upstream is ever dialled, so a call nobody stubbed is a 503 rather than someone
 * else's outage — and this is the one place it can be confirmed rather than assumed. A proxy
 * that came up in `record` mode would pass every test while reaching the real internet.
 */
function parseHandshake(line: string): Handshake {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch (err) {
    throw new Error(`sure-testproxy's handshake is not JSON: ${err}\n${line}`);
  }
  const handshake = parsed as Handshake;
  if (typeof handshake?.control !== "string" || !handshake.control) {
    throw new Error(`sure-testproxy's handshake names no control address:\n${line}`);
  }
  if (!handshake.upstreams || Object.keys(handshake.upstreams).length === 0) {
    throw new Error(`sure-testproxy's handshake reports no upstreams:\n${line}`);
  }
  if (!handshake.env || Object.keys(handshake.env).length === 0) {
    throw new Error(`sure-testproxy's handshake carries no endpoint environment:\n${line}`);
  }
  if (handshake.mode !== "replay") {
    throw new Error(
      `sure-testproxy came up in ${handshake.mode} mode; only replay can be trusted not to ` +
        `reach the real upstreams:\n${line}`,
    );
  }
  return handshake;
}

/** Split `host:port`, failing loudly rather than handing on a `NaN` port. */
function splitAddr(addr: string, what: string): { host: string; port: number } {
  const at = addr.lastIndexOf(":");
  const port = Number(addr.slice(at + 1));
  if (at < 0 || !Number.isInteger(port) || port <= 0) {
    throw new Error(`sure-testproxy reported ${what} address ${addr}, which has no port`);
  }
  return { host: addr.slice(0, at), port };
}
