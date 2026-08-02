import { test as base, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createSureClient, type SureClient } from "../client/src/index";

const here = path.dirname(fileURLToPath(import.meta.url)); // packages/api-tests
const REPO_ROOT = path.resolve(here, "..", "..");
// A detector run (`pnpm test:api:blocked`) builds into its own target dir, so the binary
// global-setup just built is not always target/debug — see scripts/blocked.mjs.
const TARGET_DIR = process.env.CARGO_TARGET_DIR
  ? path.resolve(REPO_ROOT, process.env.CARGO_TARGET_DIR)
  : path.join(REPO_ROOT, "target");
const BIN = path.join(TARGET_DIR, "debug", "sure-api");
const BLOCKED = Boolean(process.env.SURE_BLOCKED);

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
      // and stock-price tasks would hit their live upstreams from every test.
      BACKGROUND_TASKS: "off",
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
 * Every test gets a fresh, isolated backend: the real `sure-api` binary bound to an
 * ephemeral port against a throwaway temp-file SQLite database. Mirrors the old Rust
 * harness, but exercised over HTTP through the generated client — so a test failure can
 * mean either the API *or* the client is wrong.
 */
export const test = base.extend<{ server: Server; api: SureClient }>({
  server: async ({}, use) => {
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
