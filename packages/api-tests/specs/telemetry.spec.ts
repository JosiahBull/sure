// The OpenTelemetry export path, from the outside.
//
// What matters here is not that telemetry *works* — the metric names, the bucket boundaries and
// the layer wiring are covered by `cargo test` in `sure-telemetry`, and by pushing a real export
// into VictoriaMetrics (see docs/OBSERVABILITY.md). It is that a collector which is absent,
// unreachable, or misconfigured can never stop this server from serving, and can never make its
// shutdown dirty. A financial tracker that refuses to boot because a metrics endpoint moved would
// be a worse outcome than having no metrics.
//
// These specs deliberately *set* the `OTEL_*` variables that `fixtures.ts` strips from every other
// server this suite spawns — see the note there for why stripping is the default.
import { expect, startServer, test } from "../fixtures";

/** An OTLP endpoint on a port nothing is listening on. Port 1 is never a collector. */
const DEAD_COLLECTOR = "http://127.0.0.1:1";

/** All three signals, exporting fast enough that failures happen while a test is running. */
const EXPORTING = {
  OTEL_EXPORTER_OTLP_ENDPOINT: DEAD_COLLECTOR,
  SURE_OTEL_SIGNALS: "traces,metrics,logs",
  SURE_OTEL_METRICS_INTERVAL_SECS: "1",
  SURE_OTEL_SAMPLE_INTERVAL_SECS: "1",
};

/** `tracing`'s fmt layer colours its output; the log assertions below match plain text. */
function stripAnsi(output: string): string {
  // eslint-disable-next-line no-control-regex
  return output.replace(/\[[0-9;]*m/g, "");
}

test("a server serves normally with an unreachable collector configured", async () => {
  const server = await startServer(EXPORTING);
  try {
    // `startServer` already awaited /api/health, so reaching here is most of the assertion:
    // the process got past building three exporters and binding.
    for (let i = 0; i < 3; i += 1) {
      const res = await fetch(`${server.baseURL}/api/accounts`);
      expect(res.status).toBe(200);
    }
    // And the sampler, which runs only when export is enabled, is querying the database
    // without disturbing anything: a second pass has happened by now.
    const health = await fetch(`${server.baseURL}/api/health`);
    expect(health.status).toBe(200);
  } finally {
    server.stop();
  }
});

test("a failing exporter does not make shutdown dirty", async () => {
  // The property that keeps `specs/shutdown.spec.ts` honest. The SDK's exporter threads are OS
  // threads, not tokio tasks, so `sure-appbase`'s drain neither waits for them nor counts them
  // as abandoned. If that ever stopped being true, `clean` would flip to false here first — and
  // it would flip for every test in that file too, with a much less obvious cause.
  const server = await startServer(
    { ...EXPORTING, RUST_LOG: "error,sure_appbase=info" },
    { capture: true },
  );
  await fetch(`${server.baseURL}/api/health`);

  server.stop("SIGTERM");
  const code = await server.waitForExit();
  expect(code).toBe(0);

  const logs = stripAnsi(server.output());
  expect(logs).toContain("shutdown complete");
  expect(logs).toContain("clean=true");
  expect(logs).toContain("abandoned=0");
});

// The next three are one assertion each on purpose. A server that refuses to start never answers
// the health check `startServer` awaits, so each of these costs that 10s timeout — two in one test
// would exceed the suite's 20s per-test budget.
//
// All three are fatal by design, and the only telemetry settings that are. It is the same
// reasoning `SURE_MCP` and the provider base URLs follow: a mistyped byte limit still aims the
// right behaviour at the right place, while a mistyped endpoint sends this household's telemetry
// somewhere nobody chose, and a mistyped signal name leaves every dashboard permanently empty.

test("a collector URL with a scheme this exporter cannot speak stops startup", async () => {
  // `grpc://` is the spelling people reach for, because it is how OTLP/gRPC endpoints are written
  // everywhere — and this exporter speaks OTLP over HTTP.
  await expect(
    startServer({ OTEL_EXPORTER_OTLP_ENDPOINT: "grpc://collector:4317" }),
  ).rejects.toThrow();
});

test("a collector endpoint that is not a URL stops startup", async () => {
  await expect(startServer({ OTEL_EXPORTER_OTLP_ENDPOINT: "not a url" })).rejects.toThrow();
});

test("an unrecognised signal name stops startup", async () => {
  await expect(
    startServer({ ...EXPORTING, SURE_OTEL_SIGNALS: "traces,metircs" }),
  ).rejects.toThrow();
});

test("export is off by default, and `off` switches it off explicitly", async () => {
  // The state of every other server in this suite, and of the shipped container. Asserted
  // positively so that a future default flipping to "on" is a test failure rather than a
  // surprise outbound connection from a test run.
  const server = await startServer({ RUST_LOG: "info" }, { capture: true });
  try {
    await fetch(`${server.baseURL}/api/health`);
    expect(stripAnsi(server.output())).not.toContain("opentelemetry export enabled");
  } finally {
    server.stop();
  }

  // An endpoint with the signals turned off is also off — the setting to reach for when
  // silencing export without editing the endpoint out.
  const silenced = await startServer(
    { OTEL_EXPORTER_OTLP_ENDPOINT: DEAD_COLLECTOR, SURE_OTEL_SIGNALS: "off", RUST_LOG: "info" },
    { capture: true },
  );
  try {
    await fetch(`${silenced.baseURL}/api/health`);
    expect(stripAnsi(silenced.output())).not.toContain("opentelemetry export enabled");
  } finally {
    silenced.stop();
  }
});
