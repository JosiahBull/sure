/**
 * Shutdown: does the process actually stop, or does it just exit?
 *
 * The distinction matters because `sure` writes to SQLite. A process that exits while a
 * task is mid-write leaves a WAL segment behind and, worse, leaves the schedule claiming
 * work was never done. `sure-appbase` cancels everything it was given, waits for it, and
 * then reports what — if anything — was still running; these tests assert on that report
 * rather than on the exit code alone, because a clean exit code is exactly what an
 * abandoned task looks like from outside.
 *
 * The report is one INFO line from the `sure_appbase` target, so every server here is
 * spawned with that target turned up and its output captured.
 */
import net from "node:net";
import { existsSync } from "node:fs";
import path from "node:path";

import { test, expect, startServer, type StartedServer } from "../fixtures";

/** Quiet, except for the shutdown report we are here to read. */
const SHUTDOWN_LOGS = { RUST_LOG: "error,sure_appbase=info" };

/** The summary line `ShutdownReport::record` emits, parsed back into its fields. */
type Report = {
  trigger: string;
  app: string;
  drain: string;
  blocking: string;
  abandoned: number;
  clean: boolean;
};

/**
 * The backend's fmt subscriber colours its output, and it wraps every field name and `=`
 * in escape sequences — so a line that reads `trigger="terminate"` in a terminal does not
 * contain that substring at all.
 */
function stripAnsi(output: string): string {
  // The ESC has to go too: leaving it in would put stray bytes between `trigger` and
  // `=`, which is exactly as unmatchable as the whole sequence.
  return output.replace(/\x1B\[[0-9;]*m/gu, "");
}

function parseReport(output: string): Report {
  const line = output.split("\n").find((l) => l.includes("shutdown complete"));
  expect(line, `no shutdown report in output:\n${output}`).toBeDefined();

  const field = (name: string): string => {
    // tracing's fmt layer quotes strings and leaves numbers and booleans bare.
    const match = line!.match(new RegExp(`\\b${name}=("([^"]*)"|[^\\s]+)`));
    expect(match, `no ${name} field in: ${line}`).not.toBeNull();
    return match![2] ?? match![1];
  };

  return {
    trigger: field("trigger"),
    app: field("app"),
    drain: field("drain"),
    blocking: field("blocking"),
    abandoned: Number(field("abandoned")),
    clean: field("clean") === "true",
  };
}

/** Every assertion that "nothing was left running", in one place. */
function expectCleanShutdown(server: StartedServer, code: number | null, trigger: string) {
  const output = stripAnsi(server.output());
  expect(code, `expected a clean exit, got ${code}:\n${output}`).toBe(0);

  const report = parseReport(output);
  expect(report.trigger).toBe(trigger);
  expect(report.app).toBe("finished");
  expect(report.drain).toBe("drained");
  expect(report.blocking).toBe("drained");
  expect(report.abandoned).toBe(0);
  expect(report.clean).toBe(true);

  // The per-task diagnostics that only appear when something was abandoned. Asserting
  // their absence catches a regression that still manages to report `abandoned=0`.
  expect(output).not.toContain("task still running at shutdown");
  expect(output).not.toContain("drain deadline exceeded");
  expect(output).not.toContain("did not return before its deadline");
}

test("SIGTERM drains every spawned task and exits cleanly", async () => {
  // The container runtime — and `pnpm dev`'s restart — send SIGTERM first.
  const server = await startServer(SHUTDOWN_LOGS, { capture: true });
  try {
    // Serve something first, so the shutdown has a real server to take down rather than
    // one that never got past startup.
    const res = await fetch(`${server.baseURL}/api/accounts`);
    expect(res.status).toBe(200);

    server.proc.kill("SIGTERM");
    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "terminate");
  } finally {
    server.stop();
  }
});

test("SIGINT is reported as its own trigger and is just as clean", async () => {
  // Ctrl-C at a terminal. Distinguishable in the report from SIGTERM, because "who asked
  // for this?" is the first question when a process disappears in production.
  const server = await startServer(SHUTDOWN_LOGS, { capture: true });
  try {
    server.proc.kill("SIGINT");
    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "interrupt");
  } finally {
    server.stop();
  }
});

test("the background scheduler is cancelled and waited for, not abandoned", async () => {
  // `BACKGROUND_TASKS` is off for the rest of the suite, so this is the only test that
  // exercises the scheduler's own shutdown — the loop that used to be a bare
  // `tokio::spawn` running until the process died under it, mid-sweep.
  //
  // Its first check fires immediately and its tasks reach live upstreams, so this test
  // tolerates whatever those do: it asserts only that the loop stopped when asked.
  const server = await startServer({ ...SHUTDOWN_LOGS, BACKGROUND_TASKS: "on" }, { capture: true });
  try {
    // Long enough for the immediate first sweep to be under way when the signal lands.
    await new Promise((r) => setTimeout(r, 300));
    server.proc.kill("SIGTERM");
    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "terminate");
  } finally {
    server.stop();
  }
});

test("an idle keep-alive connection does not keep the server alive", async () => {
  // Connection tasks are tracked, so a socket a client left open is something the drain
  // has to actively resolve rather than something the process can walk away from. If the
  // graceful drain missed it, this shows up as `abandoned=1` naming `http.rs`.
  const server = await startServer(SHUTDOWN_LOGS, { capture: true });
  const url = new URL(server.baseURL);
  const socket = net.createConnection({ host: url.hostname, port: Number(url.port) });

  try {
    await new Promise<void>((resolve, reject) => {
      socket.once("connect", () => resolve());
      socket.once("error", reject);
    });
    // A complete request on a keep-alive connection, then nothing — the socket stays
    // open, idle, with the connection task parked on the next read.
    const responded = new Promise<string>((resolve) => {
      socket.once("data", (chunk) => resolve(String(chunk)));
    });
    socket.write(`GET /api/health HTTP/1.1\r\nHost: ${url.host}\r\nConnection: keep-alive\r\n\r\n`);
    expect(await responded).toContain("200");

    server.proc.kill("SIGTERM");
    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "terminate");
  } finally {
    socket.destroy();
    server.stop();
  }
});

test("requests in flight when the signal lands are drained, not cut off", async () => {
  const server = await startServer(SHUTDOWN_LOGS, { capture: true });
  try {
    // Fire a burst, then signal without waiting for any of them. Whatever the server
    // chooses to do with each one, it must not exit until it has decided.
    const inFlight = Array.from({ length: 24 }, () =>
      fetch(`${server.baseURL}/api/accounts`).then(
        (res) => res.status,
        // A connection refused or reset mid-shutdown is a legitimate outcome for a request
        // that arrived after the accept loop stopped; a *hung* one is not, and would fail
        // this test by timing out.
        () => "rejected" as const,
      ),
    );
    server.proc.kill("SIGTERM");

    const results = await Promise.all(inFlight);
    expect(results.every((r) => r === 200 || r === "rejected")).toBe(true);

    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "terminate");
  } finally {
    server.stop();
  }
});

test("a clean shutdown leaves no WAL behind", async () => {
  // The reason any of this matters. SQLite checkpoints and removes the `-wal` and `-shm`
  // files when the last connection closes; they survive only if the process went away
  // while the pool was still open. Their absence is the on-disk proof that the drain
  // really did finish before the pool was closed.
  const server = await startServer(SHUTDOWN_LOGS, { capture: true });
  try {
    // Write something, so there is a WAL to checkpoint in the first place.
    const created = await fetch(`${server.baseURL}/api/accounts`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        name: "shutdown",
        kind: "bank",
        currency_code: "NZD",
        institution: "ANZ",
        archived: false,
        sort_order: 0,
        ownership: { kind: "joint" },
        opening_balance_minor: 0,
        opening_balance_date: "2020-01-01",
      }),
    });
    expect(created.status).toBe(201);
    expect(existsSync(path.join(server.dir, "test.db-wal"))).toBe(true);

    server.proc.kill("SIGTERM");
    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "terminate");

    expect(existsSync(path.join(server.dir, "test.db")), "the database itself should remain").toBe(true);
    expect(existsSync(path.join(server.dir, "test.db-wal")), "WAL not checkpointed").toBe(false);
    expect(existsSync(path.join(server.dir, "test.db-shm")), "shared-memory file left behind").toBe(false);
  } finally {
    server.stop();
  }
});
