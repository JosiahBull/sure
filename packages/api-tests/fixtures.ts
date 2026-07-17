import { test as base, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createSureClient, type SureClient } from "../client/src/index";

const here = path.dirname(fileURLToPath(import.meta.url)); // packages/api-tests
const BIN = path.resolve(here, "..", "..", "target", "debug", "sure-api");

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

/**
 * Every test gets a fresh, isolated backend: the real `sure-api` binary bound to an
 * ephemeral port against a throwaway temp-file SQLite database. Mirrors the old Rust
 * harness, but exercised over HTTP through the generated client — so a test failure can
 * mean either the API *or* the client is wrong.
 */
export const test = base.extend<{ server: Server; api: SureClient }>({
  server: async ({}, use) => {
    const port = await freePort();
    const dir = mkdtempSync(path.join(tmpdir(), "sure-e2e-"));
    const baseURL = `http://127.0.0.1:${port}`;
    // Deliberately unset rather than inherited: tests assert the specific "not configured"
    // error the Akahu provider returns when these are absent, which must hold regardless
    // of whatever the developer's own shell happens to have exported.
    const { AKAHU_APP_TOKEN, AKAHU_USER_TOKEN, ...envWithoutAkahu } = process.env;
    const proc = spawn(BIN, [], {
      env: {
        ...envWithoutAkahu,
        DATABASE_URL: `sqlite:${path.join(dir, "test.db")}`,
        BIND_ADDR: `127.0.0.1:${port}`,
        RUST_LOG: "error",
      },
      stdio: "ignore",
    });
    try {
      await waitForHealth(baseURL);
      await use({ baseURL });
    } finally {
      proc.kill("SIGKILL");
      rmSync(dir, { recursive: true, force: true });
    }
  },
  api: async ({ server }, use) => {
    await use(createSureClient(server.baseURL));
  },
});

export { expect };
