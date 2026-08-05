import { spawn, execSync, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { DEMO_TODAY } from "./demo-date";
import { serverResponds, stopRecordedServer } from "./server-lifecycle";

const PORT = 8099;
const BASE = `http://127.0.0.1:${PORT}`;

/**
 * How long `sure-testproxy` gets to bind its listeners and print its handshake.
 *
 * Generous, because the first thing it does after `execve` is bind four sockets. Short enough
 * that a binary which is missing, or built from a revision that never prints a handshake, fails
 * the run with a message instead of hanging until Playwright's own timeout.
 */
const PROXY_HANDSHAKE_TIMEOUT_MS = 15_000;

/** How long a stopped proxy gets to exit on its own before it is killed. */
const PROXY_EXIT_TIMEOUT_MS = 5_000;

/**
 * Bytes of the proxy's stderr kept for a startup error message. Bounded because a suite that
 * reaches an unstubbed upstream logs a replay-miss WARN per call; the forwarding below is what
 * makes those visible, and this copy exists only so a failure *before* the handshake can quote
 * the reason.
 */
const PROXY_DIAGNOSTICS_LIMIT = 64 * 1024;

/**
 * The single line of JSON `sure-testproxy` writes to stdout — mechanically aligned with
 * `announce` in packages/testproxy/src/main.rs, which is why {@link parseProxyHandshake} checks
 * the shape at runtime: a renamed field is not a type error on this side.
 *
 * `control` (the JSON-Lines control plane) is deliberately not read. This suite registers no
 * stubs and asserts on no traffic — it wants the proxy for the one property in
 * {@link startProxy}'s comment — and a field nothing uses is a field nothing should validate.
 */
type ProxyHandshake = {
  mode: string;
  /** Upstream name -> `"127.0.0.1:<port>"`, without the path prefixes. */
  upstreams: Record<string, string>;
  /** `FRANKFURTER_BASE_URL` and its two siblings, path prefixes already joined on. */
  env: Record<string, string>;
};

/**
 * The proxy this run spawned.
 *
 * Held in module scope so {@link stopRecordedProxy} can stop it by closing its stdin — see there
 * for why that beats signalling the pid file, which is the fallback rather than the plan.
 */
let proxy: ChildProcess | undefined;

/**
 * Where the proxy's pid is recorded. Resolved from `cwd` the same way global-teardown resolves
 * the backend's, because Playwright runs both from packages/web.
 */
function proxyPidFile(): string {
  return path.join(process.cwd(), "tests", ".proxy.pid");
}

/**
 * Build the app, boot the real backend serving the built SPA against a fresh
 * throwaway SQLite database, and seed it. The backend PID is written to a file so
 * global-teardown can stop it.
 */
export default async function globalSetup() {
  const webDir = process.cwd(); // Playwright runs from packages/web
  const repoRoot = path.resolve(webDir, "..", "..");
  const dbPath = path.join(repoRoot, "data", "test-e2e.db");
  const pidFile = path.join(webDir, "tests", ".server.pid");

  // Build the frontend and backend (both are fast when already compiled). `sure-testproxy` is
  // built here rather than lazily so a missing binary is one `cargo` error at setup, not a
  // handshake timeout fifteen seconds later.
  execSync("pnpm run build:fast", { cwd: webDir, stdio: "inherit" });
  execSync("cargo build -p sure-api -p sure-testproxy", { cwd: repoRoot, stdio: "inherit" });

  // Stop a server a previous run left behind. A run whose setup threw never reached
  // global-teardown, so its detached backend is still holding the port — and still holding
  // the database it opened, since the unlink below only removes the *name* of a file a live
  // process has open. Left alone, the readiness probe would pass against that stale server
  // and the seed would run a second time against its already-seeded database, failing as
  // `POST /api/people -> 409` and blaming the API for a leftover process.
  //
  // There is deliberately no equivalent sweep for a leftover *proxy*. It holds nothing a next
  // run could collide with — ephemeral ports, no snapshot storage — and it cannot outlive the
  // process holding its stdin (see `startProxy`), so the only thing a sweep could do is signal
  // whatever pid a dead run's file happens to name, which by then may belong to something else.
  if (stopRecordedServer(pidFile)) {
    // It drains in-flight requests before letting go of the port, so give it a moment.
    for (let i = 0; i < 40 && (await serverResponds(BASE)); i++) {
      await sleep(150);
    }
  }
  // Anything still answering is not ours to reuse or to kill: its database is unknown, so
  // seeding it would be writing to somebody else's data (see the `data/sure.db` rule).
  if (await serverResponds(BASE)) {
    throw new Error(
      `something is already serving ${BASE} — stop it before running the visual suite ` +
        `(lsof -nP -iTCP:${PORT} -sTCP:LISTEN)`,
    );
  }

  // Fresh database each run.
  for (const suffix of ["", "-shm", "-wal"]) {
    rmSync(dbPath + suffix, { force: true });
  }
  mkdirSync(path.dirname(dbPath), { recursive: true });

  // Before the backend, because the backend has to be told where the upstreams are.
  const proxyEnv = await startProxy(path.join(repoRoot, "target", "debug", "sure-testproxy"));

  const bin = path.join(repoRoot, "target", "debug", "sure-api");
  // Deliberately unset rather than inherited. `SURE_ENV_FILE` below stops the backend reading a
  // developer's `.env`; these two are the same tokens by another route — an exported variable —
  // and they decide whether the Providers page reads "connected" or "not configured". A suite
  // whose screenshots assert on exact pixels cannot have that depend on whose shell it ran in.
  const { AKAHU_APP_TOKEN, AKAHU_USER_TOKEN, ...envWithoutAkahu } = process.env;
  const server = spawn(bin, [], {
    cwd: repoRoot,
    env: {
      ...envWithoutAkahu,
      // The backend otherwise searches upward from `cwd` for a `.env` — and `cwd` is the
      // repo root. Whatever a developer keeps in theirs must not reach a suite whose
      // screenshots assert on exact numbers. Empty = don't load one.
      SURE_ENV_FILE: "",
      DATABASE_URL: `sqlite:${dbPath}`,
      WEB_DIR: path.join(webDir, "dist"),
      BIND_ADDR: `127.0.0.1:${PORT}`,
      RUST_LOG: "warn",
      // Screenshots can only be stable if the seeded data is. The scheduler's first check
      // runs on startup, so the exchange-rate and stock-price tasks would fetch live
      // figures and rewrite the numbers these snapshots assert on.
      BACKGROUND_TASKS: "off",
      // After `...envWithoutAkahu`, so an exported `YAHOO_FINANCE_BASE_URL` cannot point this
      // backend back at the real host. Nothing here overrides it in turn — the keys are
      // disjoint from every name above.
      ...proxyEnv,
    },
    stdio: "ignore",
    detached: true,
  });
  server.unref();
  writeFileSync(pidFile, String(server.pid));

  // Everything from here on can fail, and a failure has to take the server *and* the proxy with
  // it — the whole point of the check above is that the next run finds the port free.
  try {
    // Wait for readiness.
    let up = false;
    for (let i = 0; i < 100; i++) {
      try {
        const res = await fetch(`${BASE}/api/health`);
        if (res.ok) {
          up = true;
          break;
        }
      } catch {
        /* not ready yet */
      }
      await sleep(150);
    }
    if (!up) throw new Error("backend did not become ready on " + BASE);

    // Seed demo data, dated against the suite's pinned "today" rather than the real one so
    // the screenshots stay byte-identical whatever day they run on.
    execSync("node scripts/seed.mjs", {
      cwd: repoRoot,
      env: { ...process.env, BASE, SEED_TODAY: DEMO_TODAY },
      stdio: "inherit",
    });

    if (!existsSync(pidFile)) throw new Error("missing server pid file");
  } catch (e) {
    stopRecordedServer(pidFile);
    await stopRecordedProxy();
    throw e;
  }
}

/**
 * Start `sure-testproxy` and return the environment that points a `sure-api` at it.
 *
 * Why this suite needs one at all. It already refuses to reach the internet *by accident* —
 * `BACKGROUND_TASKS: "off"` keeps the scheduler's exchange-rate and stock-price tasks from
 * running — but that is a guarantee about one caller, and the browser is another. The SPA has
 * buttons whose handlers make the backend dial a third party on demand: the Providers page's
 * "Sync now" and "Connect" (`POST /api/providers/{id}/sync`,
 * `GET /api/provider-kinds/{kind}/accounts` — Akahu) and an expanded brokerage account's
 * Revalue and Backfill (`POST /api/accounts/{id}/brokerage/{revalue,backfill}` — Yahoo). A spec
 * that clicks one of those reaches the real host, so today's property holds only as long as
 * nobody writes that spec.
 *
 * In replay mode with no snapshot storage there is nowhere an answer could come from, so every
 * such call is a `503 {}` that never leaves the machine and the proxy logs the method and URI
 * that went unstubbed. Third-party downtime becomes a deterministic failure in the adapter
 * rather than a red suite nobody can reproduce.
 *
 * The ~30 lines below are a deliberate copy of packages/api-tests/proxy.ts rather than a shared
 * module: `@sure/web` and `@sure/api-tests` are separate pnpm packages, and inventing a third
 * one to hold "spawn a process and read one line of JSON" would cost more than the duplication
 * does. This copy is also the smaller half — the backend suite needs the control-plane client
 * for stubs and traffic assertions, and this suite needs none of it.
 */
async function startProxy(binary: string): Promise<Record<string, string>> {
  const proc = spawn(binary, [], {
    // stdin is a pipe held open by *this* process for the whole run — Playwright's main process,
    // which runs global-setup and global-teardown and then force-exits. Closing that pipe is the
    // proxy's only notification when the run dies without teardown (a `^C`, a fatal config
    // error, a killed runner), and losing it is what stops an orphan holding four listeners.
    //
    // Not `detached: true` + `unref()`, the way the backend above is spawned: detaching would
    // put the proxy in its own process group with nothing holding its stdin, which is exactly
    // the arrangement that leaves one behind. The cost is that its group is now Playwright's
    // own — see `stopRecordedProxy` for the signal that must therefore never be sent.
    stdio: ["pipe", "pipe", "pipe"],
    env: {
      ...process.env,
      // Stated, never inherited. `record` mode dials the real upstreams with whatever
      // credentials are in the environment and reports the result as a passing test.
      SURE_TESTPROXY_MODE: "replay",
      // Stubs only: no snapshot is loaded and none is written — the strongest form of the
      // no-internet guarantee, and it keeps a run from leaving `.ndjson` files in the tree.
      SURE_TESTPROXY_SNAPSHOT_DIR: "",
      // Ephemeral, and stated for the same reason as the mode: an inherited fixed port would
      // fail to bind against whatever already holds it, with a message about addresses.
      SURE_TESTPROXY_CONTROL_BIND: "127.0.0.1:0",
      // `warn` is where the replay-miss line lives, and that line — the method and URI of the
      // call nobody stubbed — is the diagnosis for the failure this arrangement exists to
      // produce. Quiet otherwise, so a passing run says nothing.
      RUST_LOG: "warn",
    },
  });

  let diagnostics = "";
  proc.stderr?.setEncoding("utf8");
  proc.stderr?.on("data", (chunk: string) => {
    // Forwarded, not swallowed: a replay miss during a test is the one thing this process has to
    // say mid-run. It lands in the reporter's output unattributed — global-setup has no test to
    // blame it on — so the URI in the line is what identifies the caller.
    process.stderr.write(chunk);
    if (diagnostics.length < PROXY_DIAGNOSTICS_LIMIT) diagnostics += chunk;
  });

  proxy = proc;
  // Recorded before the handshake, so a proxy that starts and then wedges is still stoppable.
  writeFileSync(proxyPidFile(), String(proc.pid));

  let handshake: ProxyHandshake;
  try {
    handshake = parseProxyHandshake(await firstProxyLine(proc, () => diagnostics));
  } catch (e) {
    await stopRecordedProxy();
    throw e;
  }

  // `SURE_SANDBOX_CONNECT_PORTS` is the part that is easy to leave out and expensive to debug:
  // on Linux the server's default Landlock policy permits outbound TCP on 443 and 53 only
  // (packages/server/src/sandbox.rs) and derives nothing from the configured endpoints, so a
  // proxy on an ephemeral port is simply unreachable and the failure surfaces as an ordinary
  // connection error naming nothing about Landlock. It is a no-op on macOS — which is why it has
  // to be set here rather than remembered, since otherwise everything is green locally and red
  // in the Linux container CI runs this suite in.
  const ports = Object.entries(handshake.upstreams).map(([name, addr]) => proxyPort(addr, name));
  return { ...handshake.env, SURE_SANDBOX_CONNECT_PORTS: ports.join(",") };
}

/**
 * Stop the proxy this run started. Safe to call twice, and safe to call when none was started.
 *
 * Two paths, and the first is the one that should run. Ending stdin is how the proxy is designed
 * to be stopped (`wait_for_stop` in packages/testproxy/src/main.rs): no signal, no dependence on
 * a pid that may since have been recycled, and it takes the same code path a `^C` would.
 *
 * The pid file is the fallback for a teardown that does not share this module instance, and it
 * carries a warning worth stating: the pid must be signalled **alone**. `stopRecordedServer`
 * signals `-pid`, the process *group*, which is correct for the detached backend and would here
 * kill Playwright itself — the proxy is not detached, so its group is the runner's.
 */
export async function stopRecordedProxy(): Promise<void> {
  const pidFile = proxyPidFile();
  const proc = proxy;
  proxy = undefined;

  if (!proc) {
    try {
      const pid = Number(readFileSync(pidFile, "utf8").trim());
      // Never `-pid`. See above.
      if (pid) process.kill(pid, "SIGTERM");
    } catch {
      /* nothing recorded, or it is already gone */
    } finally {
      rmSync(pidFile, { force: true });
    }
    return;
  }

  const exited = new Promise<void>((resolve) => {
    if (proc.exitCode !== null || proc.signalCode !== null) resolve();
    else proc.once("exit", () => resolve());
  });
  proc.stdin?.end();
  const timer = setTimeout(() => proc.kill("SIGKILL"), PROXY_EXIT_TIMEOUT_MS);
  try {
    await exited;
  } finally {
    clearTimeout(timer);
    rmSync(pidFile, { force: true });
  }
}

/**
 * Read the handshake line, or fail saying which way it went wrong.
 *
 * Three distinguishable failures, because they have three different fixes: the binary would not
 * start (rebuild, or `pnpm test:web` from the wrong directory), it started and died (its stderr
 * is quoted), or it is alive and said nothing (a revision that does not announce itself).
 */
function firstProxyLine(proc: ChildProcess, diagnostics: () => string): Promise<string> {
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
      // Anything after the newline is discarded, and the stream is left flowing: the proxy never
      // writes a second line, but a paused pipe that filled up would block it mid-log.
      proc.stdout?.resume();
      resolve(buffered.slice(0, end));
    };
    const onExit = (code: number | null, signal: NodeJS.Signals | null) =>
      fail(`sure-testproxy exited (code ${code}, signal ${signal}) before printing its handshake`);
    const onError = (err: Error) => fail(`could not start sure-testproxy: ${err.message}`);
    const timer = setTimeout(
      () => fail(`sure-testproxy printed no handshake within ${PROXY_HANDSHAKE_TIMEOUT_MS}ms`),
      PROXY_HANDSHAKE_TIMEOUT_MS,
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
 * The mode assertion is not defensive padding: `replay` is the property the whole arrangement
 * rests on — no upstream is ever dialled — and this is the one place it can be confirmed rather
 * than assumed. A proxy that came up in `record` mode would let every test pass while reaching
 * the real internet.
 */
function parseProxyHandshake(line: string): ProxyHandshake {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch (err) {
    throw new Error(`sure-testproxy's handshake is not JSON: ${err}\n${line}`);
  }
  const handshake = parsed as ProxyHandshake;
  if (!handshake?.upstreams || Object.keys(handshake.upstreams).length === 0) {
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

/** The port out of a `host:port`, failing loudly rather than handing on a `NaN`. */
function proxyPort(addr: string, upstream: string): number {
  const at = addr.lastIndexOf(":");
  const port = Number(addr.slice(at + 1));
  if (at < 0 || !Number.isInteger(port) || port <= 0) {
    throw new Error(`sure-testproxy reported upstream ${upstream} at ${addr}, which has no port`);
  }
  return port;
}
