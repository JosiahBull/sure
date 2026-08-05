// Typed client for `sure-testproxy`'s JSON-Lines TCP control plane.
//
// VENDORED, not installed. Upstream ships this as `@partly/proxy-client`, but that package is
// not published to npm (the registry 404s) — so the alternatives were a git dependency on an
// untagged repo or this file. Vendoring wins twice: the wire protocol is ~90 lines of types
// and one framing loop with no runtime dependencies, and upstream's own copy carries a "keep
// this file mechanically aligned with crates/partly-proxy-lib/src/wire.rs" comment, i.e. it is
// hand-maintained and can drift from the Rust it describes. A copy in this repo drifts only
// when we change it.
//
// Verified field-by-field against `crates/partly-proxy-lib/src/wire.rs` at the rev pinned in
// the root Cargo.toml. If you bump that rev, re-read `wire.rs`: the compiler cannot catch a
// renamed JSON field, and the failure mode is a stub that silently never matches.
//
// One deliberate addition over upstream: `Upstream` is a string union rather than `string`, so
// a mistyped upstream name is a type error here instead of an assertion that never fires. The
// values must match `sure_testproxy::Upstream::name()`.

import * as net from "node:net";

/** The upstreams `sure-testproxy` binds. Must match `sure_testproxy::Upstream::name()`. */
export type Upstream = "frankfurter" | "yahoo_finance" | "akahu";

/** Filter shared by AssertCount and QueryTraffic. All conditions AND together. */
export interface TrafficFilter {
  upstream?: Upstream;
  method?: string;
  /** Regex against the URI **path** only — never the query string. */
  path_pattern?: string;
  /** Exact response status. Only matches exchanges that produced a response at all. */
  status?: number;
  labels?: Record<string, string>;
}

/**
 * Stub registration. Bodies are UTF-8 strings on the wire (unlike recorded exchange bodies,
 * which are base64) so a test can write a JSON payload inline.
 *
 * Note what the matcher cannot see: the query string. `path_pattern` is matched against
 * `uri.path()`, so two requests differing only in a query parameter — every paginated fetch
 * we have — are indistinguishable to a matcher. Give them different answers by registering
 * two stubs with `times: 1`; the first match wins and retires. See
 * `packages/providers/tests/proxy_contract.rs`, which pins that ordering.
 */
export interface StubOptions {
  upstream?: Upstream;
  // --- matcher ---
  method?: string;
  path_pattern?: string;
  /** Each entry must be present, with the given value found as a *substring*. */
  header_contains?: Record<string, string>;
  body_contains?: string;
  // --- response ---
  status?: number;
  response_headers?: Record<string, string>;
  body?: string;
  delay_ms?: number;
  /** Fire-count limit; omit for unlimited. */
  times?: number;
}

type WireCommand =
  | ({ type: "Stub" } & StubOptions)
  | { type: "ClearStubs"; upstream?: Upstream }
  | { type: "Pause"; upstream?: Upstream }
  | { type: "Resume"; upstream?: Upstream }
  | ({ type: "AssertSeen"; timeout_ms: number } & TrafficFilter)
  | ({ type: "AssertCount"; expected: number; timeout_ms: number } & TrafficFilter)
  | ({ type: "QueryTraffic" } & TrafficFilter)
  | { type: "ClearRecordings" };

export interface RecordedRequest {
  method: string;
  /**
   * Origin form: path + query — and the query is the recorder's copy, not the wire's. Every
   * parameter the upstream declares volatile (`Upstream::volatile_query_params`) reads
   * `CANONICAL` here, because the recorder is handed the request after redaction. So Frankfurter's
   * `?base=` is assertable and Yahoo's `?period1=`/Akahu's `?start=` are not; `docs/TESTING.md`'s
   * third gotcha has the whole story and where those two *are* pinned.
   */
  uri: string;
  headers: Array<[string, string]>;
  /** Base64. Use {@link decodeBody}. */
  body: string;
  body_sha256: string;
}

/** A non-2xx response is still `response`; only a transport failure lands as `error`. */
export type ExchangeOutcome =
  | { kind: "response"; status: number; headers: Array<[string, string]>; body: string }
  | { kind: "error"; message: string };

export interface RecordedExchange {
  id: string;
  upstream?: Upstream;
  timestamp: string;
  duration_ms: number;
  request: RecordedRequest;
  outcome: ExchangeOutcome;
  labels?: Record<string, string>;
}

type WireResponse =
  | { type: "Ok" }
  | { type: "Error"; message: string }
  | { type: "Exchanges"; exchanges: RecordedExchange[] }
  | { type: "AssertionResult"; passed: boolean; message: string };

export interface AssertionResult {
  passed: boolean;
  message: string;
}

export class ProxyControlError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProxyControlError";
  }
}

/** Decode a recorded request or response body. */
export function decodeBody(base64: string): string {
  return Buffer.from(base64, "base64").toString("utf8");
}

export interface ProxyClientOptions {
  host: string;
  port: number;
  /** Default timeout (ms) for assertion commands. */
  defaultAssertionTimeoutMs?: number;
  /** Per-command socket timeout (ms). */
  socketTimeoutMs?: number;
}

/**
 * One command per line, one response line back, matched FIFO.
 *
 * `assertCount` **blocks on the proxy side** until the predicate holds or the supplied timeout
 * elapses — so it is a synchronisation primitive, not a poll: "I have just kicked off work that
 * should call the upstream twice; wait up to 5s for it" is a single round-trip. An overshoot is
 * terminal (more matches than expected can never come back down), so it fails fast rather than
 * waiting out the clock.
 *
 * `assertSeen` is the same primitive without a count, and the weaker one wherever a count will do:
 * "one upstream call per sync" is usually the property, and two is the bug. Its one caller is
 * specs/brokerage-pricing.spec.ts, blocking until a *fire-and-forget* backfill reaches the feed —
 * the question there is only whether the import started the work, and how many charts a backfill
 * fetches is a different test's property, so a count would tie the two together and make tightening
 * one break the other.
 *
 * `TrafficFilter`'s and `StubOptions`' unused optional fields stay for the reason this file is
 * vendored at all: it is a mirror of `wire.rs`, and the header's claim to have been verified field
 * by field against it is only worth something if the mirror is complete. Dropping a field nothing
 * happens to read would make the next reader's diff against upstream show a difference that means
 * nothing, which is exactly the drift vendoring was meant to make visible — this file is protocol
 * documentation as much as it is code.
 */
export class ProxyClient {
  private readonly socket: net.Socket;
  private readonly defaultAssertionTimeoutMs: number;
  private buffer = "";
  private readonly pending: Array<{
    resolve: (line: string) => void;
    reject: (err: Error) => void;
  }> = [];
  private fatal?: Error;
  private closing = false;
  /** Chains sends so commands are serialised on the wire and replies stay FIFO-matchable. */
  private inflight: Promise<void> = Promise.resolve();

  private constructor(socket: net.Socket, opts: ProxyClientOptions) {
    this.socket = socket;
    this.defaultAssertionTimeoutMs = opts.defaultAssertionTimeoutMs ?? 5_000;

    socket.setEncoding("utf8");
    socket.setTimeout(opts.socketTimeoutMs ?? 30_000);
    socket.on("data", (chunk: string) => this.onData(chunk));
    socket.on("error", (err) => this.fail(err));
    // An expected close (`close()` below) must not manufacture an error for a caller that
    // has already finished with the client — upstream's copy does, and it surfaces as a
    // spurious unhandled rejection when a fixture tears down mid-flight.
    socket.on("close", () => {
      if (!this.closing) this.fail(new Error("proxy control-plane socket closed"));
    });
    socket.on("timeout", () => this.fail(new Error("proxy control-plane socket timed out")));
  }

  static async connect(opts: ProxyClientOptions): Promise<ProxyClient> {
    return await new Promise<ProxyClient>((resolve, reject) => {
      const socket = net.createConnection({ host: opts.host, port: opts.port }, () => {
        resolve(new ProxyClient(socket, opts));
      });
      socket.once("error", (err) => reject(err));
    });
  }

  /** Register a stub. Stubs are tried in registration order; the first match fires. */
  async stub(stub: StubOptions): Promise<void> {
    expectOk(await this.send({ type: "Stub", ...stub }));
  }

  /** Clear stubs for one upstream, or all of them when `upstream` is omitted. */
  async clearStubs(upstream?: Upstream): Promise<void> {
    expectOk(await this.send(upstream === undefined ? { type: "ClearStubs" } : { type: "ClearStubs", upstream }));
  }

  /**
   * Hold every request to `upstream` until {@link resume}. The request is not refused — it
   * waits — which is how a test gets a real outbound call to sit in flight while it does
   * something else (fire a second sync and expect a 409, or send SIGTERM and watch the drain).
   */
  async pause(upstream?: Upstream): Promise<void> {
    expectOk(await this.send(upstream === undefined ? { type: "Pause" } : { type: "Pause", upstream }));
  }

  async resume(upstream?: Upstream): Promise<void> {
    expectOk(await this.send(upstream === undefined ? { type: "Resume" } : { type: "Resume", upstream }));
  }

  /**
   * Block until any matching exchange appears, or the timeout elapses.
   *
   * Note what it cannot see: a *paused* request is not recorded until it is answered, so this
   * blocks to timeout on exactly the case `pause` sets up. Wait on the effect instead.
   */
  async assertSeen(filter: TrafficFilter, timeoutMs?: number): Promise<AssertionResult> {
    const timeout = timeoutMs ?? this.defaultAssertionTimeoutMs;
    return expectAssertion(
      await this.send({ type: "AssertSeen", timeout_ms: timeout, ...filter }, commandWindow(timeout)),
    );
  }

  /** Block until the match count equals `expected`, overshoots, or the timeout elapses. */
  async assertCount(filter: TrafficFilter, expected: number, timeoutMs?: number): Promise<AssertionResult> {
    const timeout = timeoutMs ?? this.defaultAssertionTimeoutMs;
    return expectAssertion(
      await this.send(
        { type: "AssertCount", expected, timeout_ms: timeout, ...filter },
        commandWindow(timeout),
      ),
    );
  }

  /** Every recorded exchange matching `filter` (or all of them), in insertion order. */
  async queryTraffic(filter?: TrafficFilter): Promise<RecordedExchange[]> {
    const resp = await this.send({ type: "QueryTraffic", ...(filter ?? {}) });
    if (resp.type !== "Exchanges") throw asProxyError(resp);
    return resp.exchanges;
  }

  async clearRecordings(): Promise<void> {
    expectOk(await this.send({ type: "ClearRecordings" }));
  }

  async close(): Promise<void> {
    this.closing = true;
    return await new Promise<void>((resolve) => this.socket.end(() => resolve()));
  }

  private async send(cmd: WireCommand, commandTimeoutMs?: number): Promise<WireResponse> {
    if (this.fatal) throw this.fatal;
    const myTurn = this.inflight.then(() => this.doSend(cmd, commandTimeoutMs));
    // Swallowed deliberately: `inflight` is only a turn-taking baton, and an unhandled
    // rejection on it would surface as a process-level warning in addition to the error the
    // caller already gets from `myTurn`.
    this.inflight = myTurn.then(
      () => undefined,
      () => undefined,
    );
    return await myTurn;
  }

  private async doSend(cmd: WireCommand, commandTimeoutMs?: number): Promise<WireResponse> {
    const responsePromise = new Promise<string>((resolve, reject) => {
      this.pending.push({ resolve, reject });
    });

    if (!this.socket.write(JSON.stringify(cmd) + "\n")) {
      await new Promise<void>((resolve) => this.socket.once("drain", () => resolve()));
    }

    let timer: NodeJS.Timeout | undefined;
    const timed = commandTimeoutMs
      ? new Promise<never>((_, reject) => {
          timer = setTimeout(
            () => reject(new ProxyControlError(`proxy command timed out after ${commandTimeoutMs}ms`)),
            commandTimeoutMs,
          );
        })
      : undefined;

    try {
      const raw = timed ? await Promise.race([responsePromise, timed]) : await responsePromise;
      return JSON.parse(raw) as WireResponse;
    } finally {
      if (timer) clearTimeout(timer);
    }
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    let idx: number;
    while ((idx = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, idx);
      this.buffer = this.buffer.slice(idx + 1);
      this.pending.shift()?.resolve(line);
    }
  }

  private fail(err: Error): void {
    this.fatal ??= err;
    while (this.pending.length > 0) {
      this.pending.shift()?.reject(err);
    }
  }
}

/**
 * How long to wait on the socket for a command the proxy answers *late by design*.
 *
 * A blocking assertion writes its response only once the predicate holds or its own
 * `timeout_ms` expires, so the socket window has to outlast it — otherwise the client gives
 * up first and reports a timeout for an assertion that was about to pass.
 */
function commandWindow(assertionTimeoutMs: number): number {
  return Math.max(assertionTimeoutMs + 2_000, 5_000);
}

function expectOk(resp: WireResponse): void {
  if (resp.type !== "Ok") throw asProxyError(resp);
}

function expectAssertion(resp: WireResponse): AssertionResult {
  if (resp.type === "AssertionResult") return { passed: resp.passed, message: resp.message };
  throw asProxyError(resp);
}

function asProxyError(resp: WireResponse): Error {
  return new ProxyControlError(
    resp.type === "Error" ? resp.message : `unexpected proxy response: ${JSON.stringify(resp)}`,
  );
}
