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

test("the background scheduler is cancelled and waited for, not abandoned", async ({ testproxy }) => {
  // `BACKGROUND_TASKS` is off for the rest of the suite, so this is the only test that
  // exercises the scheduler's own shutdown — the loop that used to be a bare
  // `tokio::spawn` running until the process died under it, mid-sweep. `packages/server`
  // calls that drain load-bearing in as many words: "a provider poll part-way through
  // writing a sync row is exactly the thing that must not be cut off."
  //
  // Which means the sweep has to still be running when the signal lands, and the stub below
  // is the only thing that makes that true. On a fresh database the exchange-rate poll is
  // the one registered task with anything to fetch, and an unstubbed call is the proxy's
  // replay-miss 503 in about a millisecond — so for as long as this test relied on the sweep
  // reaching a live upstream, the 300ms sleep guaranteed the *opposite* of what it claimed
  // and the drain resolved an empty tracker. It passed identically with `scheduler.run`
  // never spawned at all.
  //
  // `delay_ms`, and deliberately not `pause` — the obvious tool for holding a call open, and
  // wrong here twice over. A paused request is never recorded, so `assertCount` below — the
  // only evidence the sweep ever reached the wire — could never see it; and it is never
  // answered, so the poll would die on `sure_providers::http`'s 6s `REQUEST_TIMEOUT` and the
  // drain would be timing a client-side abort rather than a request that finished. A delayed
  // stub is in flight for a known length and then completes, which is the production shape.
  //
  // Long enough that the sweep is provably still out when the signal lands 300ms in, short
  // enough to stay well inside both ceilings it would otherwise collide with: that same 6s
  // `REQUEST_TIMEOUT`, and the 10s drain grace whose expiry is what `abandoned=0` denies.
  const HELD_MS = 3_000;
  // Registered before `startServer`, because the scheduler's first check fires as the
  // process comes up: a stub registered after it returns is already racing the call it is
  // meant to answer (docs/TESTING.md's fourth gotcha).
  await testproxy.stub({
    upstream: "frankfurter",
    method: "GET",
    path_pattern: "^/v1/latest$",
    status: 200,
    response_headers: { "content-type": "application/json" },
    delay_ms: HELD_MS,
    // Well-formed, so what the drain waits on is the ordinary poll path rather than an error
    // unwinding out of a parse — a body the adapter rejects would hold the request for just
    // as long and prove rather less. The figures are invented; `specs/exchange-rates.spec.ts`
    // owns what the task does with them.
    body: JSON.stringify({ amount: 1, base: "NZD", date: "2026-03-09", rates: { USD: 0.625 } }),
  });

  const server = await startServer({ ...SHUTDOWN_LOGS, BACKGROUND_TASKS: "on" }, { capture: true });
  try {
    // Slack rather than a race: `serve` spawns the scheduler before it binds the listener,
    // so the poll is on the wire before the health check `startServer` just awaited could
    // answer, and it cannot come back for another `HELD_MS` regardless. The measurement
    // below is what confirms it rather than this comment — the poll turns out to leave
    // ~20ms ahead of the health check.
    await new Promise((r) => setTimeout(r, 300));
    const signalledAt = Date.now();
    server.proc.kill("SIGTERM");
    const code = await server.waitForExit();
    expectCleanShutdown(server, code, "terminate");

    // The assertion that tells a real drain from an idle one, and the reason it exists:
    // `clean=true` above holds in *both* worlds, because an empty tracker drains cleanly and
    // instantly — so the report alone cannot say whether anything was waited for, and for as
    // long as the sweep finished during the sleep it was saying nothing at all. The clock
    // can say it. An exit that beat the stub means the process walked away from a poll
    // instead of finishing it.
    //
    // The real figure is `HELD_MS` less the sleep, measured at 2676-2705ms across ten runs
    // on five parallel workers; the bound leaves over a second of that as slack, because this
    // is an assertion about two constants rather than about how loaded the machine is.
    const waited = Date.now() - signalledAt;
    expect(waited, `the drain returned in ${waited}ms, without waiting for the in-flight sweep`)
      .toBeGreaterThan(1_500);

    // …and the sweep really did put a request on the wire, rather than the drain having
    // waited on something else entirely. Counted after the exit, not before: an exchange is
    // recorded when it is *answered*, so this reads zero for as long as the stub is still
    // holding it. Which makes the two assertions one fact read from opposite sides — the
    // count is only observable because the drain kept the process alive to receive the
    // answer, and a process that exited early would fail both.
    const polled = await testproxy.assertCount({ upstream: "frankfurter" }, 1);
    expect(polled.passed, polled.message).toBe(true);
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
