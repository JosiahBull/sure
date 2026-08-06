# Testing

Three tiers of test, and one property all of them share: **nothing in this repository reaches a
third-party host.** Every outbound provider request lands on
[`sure-testproxy`](../packages/testproxy/src/lib.rs) — a reverse-proxy cluster standing in for
the three hosts `sure-providers` talks to (Frankfurter, Yahoo Finance, Akahu) — in a mode that
cannot dial an upstream at all. A call nobody stubbed comes back `503 {}` with a WARN naming the
method and URI, instead of silently depending on someone else's uptime.

What each crate is: [ARCHITECTURE.md](ARCHITECTURE.md). Which CI job runs what:
[CI.md](CI.md). This is about which tier a test belongs in, how to add a fixture, and the traps.

## The three tiers

| Tier | Drives | Proxy | Fixture lives in |
| --- | --- | --- | --- |
| **Provider fixtures** (`packages/providers/tests`, `packages/testproxy/tests`) | one adapter, in-process | built by the test | Rust consts, plus committed captures in `tests/snapshots` |
| **Backend e2e** (`packages/api-tests`) | the real `sure-api` binary over HTTP | one process per Playwright worker | the spec, via the control plane |
| **Browser** (`packages/web/tests`) | mobile Chromium against the built SPA | one process per run, no stubs | seeded demo data |

Three rather than one because they fail differently and cost differently, and the cheapest tier
is the one that can actually see a provider bug.

**Provider fixtures** are `#[tokio::test]`s that stand up a cluster and point one adapter at it.
No binary, no SQLite, no HTTP server, no Playwright — a few milliseconds each. This is where an
adapter's *fetch path* belongs, and that is most of where the bugs are: the URL it builds, the
headers it sends, the window it asks for, whether its pagination loop follows the cursor, and
what `json_capped` does with a body that overruns the ceiling or arrives malformed. All of it
was previously reachable only against the live API, which is why none of it was covered. A
parsing test needs no proxy at all and stays a `#[cfg(test)]` module beside the parser.

This is also the tier that replays the committed recordings, and the only one that should: a
capture proves the *real document* still parses, which is an adapter's concern, and reading one
costs a file rather than a process. See "Record and replay" below.

**Backend e2e** spawns the real compiled binary on an ephemeral port against a throwaway
temp-file SQLite database and drives it through the generated `@sure/client`. Put a test here
when the thing under test spans layers a unit test cannot join up: a route's behaviour end to
end, a value written by one component and read back by another, or the scheduler running for
real. `specs/akahu.spec.ts`'s re-sync window is the shape to copy — the watermark
`sure_app::sync` writes has to come back out of SQLite through
`SyncContext.last_synced_at` and into the adapter's `?start=`, and a Rust fixture that hand-feeds
`last_synced_at` cannot see that seam at all. What does *not* belong here is anything an adapter
fixture can prove on its own: a parse, a URL, a header. A spec that boots a process and a database
to check string construction buys nothing `cargo test` did not already have, and pays a backend
spawn for it on every run.

**Browser** drives the built SPA with screenshot baselines and DOM assertions. It stubs nothing
and never touches the control plane; it has a proxy because the *browser* is a second caller.
`BACKGROUND_TASKS: "off"` stops the scheduler reaching out, but the Providers page's Sync/Connect
and an expanded brokerage account's Revalue/Backfill each make the backend dial a third party on
demand, so the containment held only as long as nobody wrote that spec. Now it holds either way.

## Running them

```bash
pnpm test                     # all three tiers, in cost order
pnpm test:rust                # cargo test --workspace --all-features (includes tier 1)
pnpm test:api                 # backend e2e
pnpm test:web                 # browser (needs `pnpm test:web:install` once, for Chromium)
```

Narrower, while working on one thing:

```bash
cargo test -p sure-providers --test akahu        # one provider fixture file
cargo test -p sure-testproxy                     # the proxy's own contract tests
pnpm --filter @sure/api-tests test specs/stock-prices.spec.ts
pnpm --filter @sure/api-tests test specs/akahu.spec.ts --repeat-each=3   # hunting a flake
pnpm --filter @sure/web exec playwright test tests/provider-ui.spec.ts
pnpm test:api:check                              # tsc over the specs against the client contract
```

Both Playwright suites build what they need in `global-setup` (`sure-api` **and**
`sure-testproxy`), so there is no separate build step to remember and a missing binary is one
cargo error at setup rather than a handshake timeout fifteen seconds later.

## How the proxy is wired in

`partly-proxy-lib` is a **reverse** proxy, not a `CONNECT` proxy: each upstream binds its own
listener and forwards to one fixed base URL. There is no ambient `HTTPS_PROXY` and no
interception — the system under test has to be *told* a different base URL per upstream. That is
why every provider adapter takes an injected `Endpoint` and reads no environment on a request
path, and why the harness gets `endpoints` back as a ready-made environment map with each
upstream's path prefix already joined on. Nothing in TypeScript has to know that Yahoo's charts
live under `/v8/finance/chart`.

Out of process, `cargo run -p sure-testproxy` prints one line of JSON on stdout (mode, control
address, listeners, and that env map) and takes commands over a TCP JSON-Lines control plane.
Its logs go to stderr precisely so the handshake parse cannot be broken by one. It stops when
its stdin closes, which is the only notification it gets when a Playwright worker is killed
rather than torn down.

`packages/api-tests` starts one per worker — not per test (a second process for every test, on top
of the backend each one already spawns) and not one per run, which would be wrong rather than
merely wasteful: the stub table and the traffic ring are *cluster-wide*, and `fullyParallel` is on,
so two concurrent tests would answer and count each other's calls. A worker runs one test at a
time, which makes per-worker both safe and cheap. The `proxyIsolation` fixture resets stubs,
recordings and the pause flag on the way *in* to every test — on the way in, so it also covers the
first test in a worker and cannot turn a failing test into a confusing teardown error.

## Adding a fixture

A fixture is a stub: a matcher plus the response it answers with. From a spec:

```ts
await testproxy.stub({
  upstream: "yahoo_finance",
  method: "GET",
  path_pattern: "^/v8/finance/chart/VOO$",
  status: 200,
  response_headers: { "content-type": "application/json" },
  body: chart("USD", NEW_YORK, [["2026-07-10", 212.34]]),
  times: 1,
});
```

From a Rust fixture, the same thing through `cluster.command_sender().send(Command::Stub { .. })`
— see `stub_rate_table` in `packages/providers/tests/frankfurter.rs`.

Then assert on what the app actually sent. `assertCount` **blocks on the proxy side** until the
count holds or its timeout elapses, so it is a synchronisation primitive rather than a poll: "I
have just kicked off work that should call the upstream once, wait up to 5s for it" is one
round-trip, not a sleep. It fails fast on an overshoot, since more matches can never come back
down. `queryTraffic` hands back the recorded exchanges — the method, path and count the app
produced, plus any query parameter that is not clock-derived (see the third gotcha below), with
bodies base64-encoded (`decodeBody`). `pause`/`resume` hold every request to an upstream *without*
refusing it, which is how a test gets a real outbound call to sit in flight while it does something
else — fire a second sync and expect a 409, or send `SIGTERM` and watch the drain. `assertSeen` is
the count-free form, and the weaker one wherever a count will do, since a spec that cares whether a
call happened usually cares how many times. It earns its keep on work nobody holds a handle to:
`specs/brokerage-pricing.spec.ts` blocks on it until the fire-and-forget import backfill reaches the
feed, because how many charts that backfill fetches is a *different* test's property and a count
would tie the two together. Neither form can see a **paused** request — one is not recorded until it
is answered, so both block to timeout on the case `pause` sets up. Wait on the effect there instead.

### A call nobody stubbed fails the test that made it

`packages/api-tests` checks, in `proxyIsolation`'s teardown, that nothing the test just did was
answered by the replay miss (`failOnUnstubbedRequests` in `fixtures.ts`: `queryTraffic` for the
`503 {}` the miss handler produces, and a failure naming the method, upstream and URI of each). It
runs only when the test would otherwise have passed — a test that already failed keeps its own error
as the headline, and the proxy's WARN lines are in the output either way.

The WARN alone was not enough, for the reason any always-present warning stops being read: a green
run printed six of them, so nobody could tell the deliberate ones from a fixture that had gone
missing. And a miss is not a harmless log line. The adapter got a 503 it did not expect, so the code
under test ran down an error path while the assertions that still passed passed for the wrong
reason; the miss is *recorded*, so it counts towards an `assertCount` on the same path; and an
unstubbed call from a background task can land in the next test's traffic.

A test that wants the miss says so, and says why:

```ts
allowUnstubbed({
  upstream: "yahoo_finance",
  path_pattern: "^/v8/finance/chart/MEL\\.NZ$",   // same matcher shape as `stub`
  why: "the unanswered call is the assertion: it is what makes the route answer 502",
});
```

Three tests declare one today, and each is a case where the unanswered call *is* the test:
`specs/stock-prices.spec.ts`'s 502, and `specs/http.spec.ts`'s two `pause` tests, where what is
needed is a handler suspended at an await and the answer after the resume is somebody else's
property. It is permission, not an expectation — nothing checks the call happened, because some of
these come from fire-and-forget work that may not reach the proxy before the server is killed. Use
`assertCount`/`assertSeen` (which see a miss like any other exchange) when the stronger statement is
the point.

The other way out is to answer the call. `specs/brokerage.spec.ts` and `specs/import.spec.ts` both
stub the import backfill's chart request with a body carrying no `timestamp` — the feed's own
"nothing for this window" — because a price is irrelevant to everything either file asserts but an
unanswered fetch still left a WARN per import and a 502 inside the backfill. Any spec that uploads
a Sharesies export needs that stub: the import hands the valuation walk back as a `FollowUp` the
route spawns, so the outbound call happens whether the test cares about it or not.

Four things catch people:

* **A matcher never sees the query string.** `path_pattern` is matched against `uri.path()`, so
  two requests differing only in a query parameter — every paginated fetch we have, since Akahu's
  page cursor is a query parameter — are indistinguishable to it. Give them different answers by
  registering two stubs with `times: 1`: the first match wins and retires. That ordering is
  pinned by `packages/providers/tests/proxy_contract.rs`, not assumed.
* **A query carrying a clock reading needs canonicalising before it can be replayed.** The replay
  index compares `(method, path + query verbatim, sha256(body))`, and two of the three feeds put
  the current time in the query: Yahoo's `?period1=&period2=` come from today's date, Akahu's
  `?start=` from the last successful sync. Recorded verbatim, both stop matching the day after
  they are taken. `CanonicaliseQuery` rewrites the parameters each `Upstream` declares volatile to
  a fixed `CANONICAL`, in `redact_request_for_snapshot` only — which runs on the record side *and*
  the replay-lookup side, so both keys are computed the same way and still agree. The live request
  keeps its real epochs. Adding an upstream, or a parameter, means extending
  `Upstream::volatile_query_params`; leaving it out costs a replay miss the day after recording.
* **The recorder is handed the request after redaction**, so `queryTraffic` shows
  `period1=CANONICAL` rather than the epoch that went on the wire, and shows no credential
  headers. Frankfurter declares no volatile parameters, so `?base=NZD` *is* assertable from a
  spec (`specs/exchange-rates.spec.ts` does exactly that); Yahoo's window and Akahu's overlap are
  not, and are pinned in-process instead by fixtures that build middleware-free clusters
  (`providers/tests/yahoo_finance.rs`, `providers/tests/akahu.rs`). Installing `CanonicaliseQuery`
  only when a snapshot directory is configured would make those two readable from a spec, and was
  considered and declined: it would make what the ring carries depend on the configuration, so an
  assertion written against a real epoch would start failing the day someone commits a snapshot.
  `packages/testproxy/tests/recording.rs` pins the current behaviour in both directions, so the
  decision is a test rather than a paragraph.
* **A stub for something that runs at boot has to be registered before the process starts.** The
  scheduler's first check fires as the process comes up and every task is due on a fresh
  database, so a stub registered after `startServer` returns is already racing the call it is
  meant to answer. `specs/exchange-rates.spec.ts` registers first, then spawns.

## Personal data in a recorded fixture

CLAUDE.md rule 3 covers every fixture in the tree. Recorded HTTP gets one extra rule, because a
recording is not written by hand and nobody proofreads it before it lands.

**Frankfurter and Yahoo are public market data.** An ECB rate table and a daily close series say
nothing about whose money it is, so recordings of those two may be committed.

**Akahu is a real bank feed, and its traffic is never recorded into this repository.** Account
numbers, balances, transaction memos and payee names — exactly what rule 3 exists to keep out,
arriving by the hundred. No scrub gets them back out of history afterwards; the last attempt cost
a 58-commit rewrite. So the policy is categorical rather than per-literal, and two guards enforce
it and must keep agreeing with each other:

* `.gitignore` excludes `/packages/api-tests/snapshots/akahu/`, which is where a local recording
  belongs if you make one.
* `scripts/pii-scan.mjs` fails on a staged Akahu recording **by path** (`AKAHU_SNAPSHOT_PATH` —
  for the `git add -f` that walks past the ignore rule) **and by content** (`"upstream":"akahu"`
  on any NDJSON line, with a textual fallback so a truncated line still trips it — for the
  recording dropped somewhere the ignore rule does not name). Neither check consults `data/sure.db`
  or the allowlist: the finding is not "that literal might be real", it is "this file cannot be in
  this repository at all".

A committable Akahu fixture is hand-authored with invented identifiers, and it can still exercise
the replay path: a stub-served exchange *is* recorded, so a hand-written fixture can be
materialised into a snapshot file without an upstream ever being contacted
(`providers/tests/proxy_contract.rs`).

### Why the base64 decode in `pii-scan.mjs` is load-bearing

An `*.ndjson` snapshot is text, so it sails past the binary skip in that script's `wholeTree` and
gets scanned — but a recorded body is base64, and **every pattern in the script matches nothing
against base64**. Before `expandSnapshots` existed, a snapshot could carry an account number in a
response body and the scan would print `✓ no personal-data shapes in the tree`. It now decodes
both bodies of every exchange into extra rows (keeping the raw line as well, and first, so a
header, a URI or a label is still covered as plain JSON) and undoes gzip/deflate recognised by
magic bytes rather than by the `content-encoding` header — a header is a claim, the magic is the
payload. Deleting that decode as a simplification would restore a silent hole, not remove a slow
path. The report line `↳ inside the base64 response body … decoded, so not greppable as text`
exists for the same reason: without it, a reviewer greps the file for the literal, finds nothing,
and writes the whole finding off as a false positive.

## Record and replay

Two of the three upstreams are recorded. `packages/providers/tests/snapshots/frankfurter.ndjson`
and `yahoo_finance.ndjson` are committed captures of what those APIs really sent, replayed by
`packages/providers/tests/recorded_upstreams.rs`.

**Why those two and not the third.** Both are public market data, so a capture carries nothing
personal. Akahu's traffic *is* the personal data — real account numbers, balances, transaction
memos, payee names — so it is never recorded into this repository, and `pii-scan` fails a commit
that tries, by path and by content (see the section above). Its fixtures are hand-authored with
invented identifiers.

**What a recording buys that a stub cannot.** A hand-written body contains the fields the person
writing it knew to include, so it pins what we *believe* the API returns — and a belief can be
wrong in the same direction as the code, leaving fixture and adapter agreeing with each other
while both disagree with the upstream. Yahoo's real chart response carries around forty `meta`
fields the adapter never reads, a `currentTradingPeriod` object, and pre/post-market flags; the
recording proves the parser copes with the whole document. Both kinds are used deliberately:
recordings for "the real shape still parses", stubs for the cases a recording cannot produce on
demand — a 404 for a delisted symbol, a body over the 8 MiB ceiling, a page whose cursor advances.

**The assertions over a capture are structural** — a currency, a count, a sign, a decimal scale —
not "yesterday's close was 708.98". Pinning a price would mean every re-record churns the test,
which is how a staleness guard gets deleted for being annoying.

To refresh:

```bash
pnpm fixtures:record   # reaches the real APIs; #[ignore]d so a normal run never does
```

Then **read the diff** — that is the point of re-recording. A couple of new `meta` fields is
routine; a renamed or vanished one is what the whole arrangement exists to catch.

**Staleness is not guarded, and that is a choice rather than an oversight.** Replay never fails
when the real API moves: it goes on answering the shape it captured while the suite stays green
against an endpoint that has changed underneath it, and Yahoo's is undocumented. So a capture is
evidence about the API *on the day it was taken*, and nothing in CI will tell you when that stops
being true — running `pnpm fixtures:record` and reading the diff is the only thing that will.

Worth doing when a price or FX path misbehaves against the live app but not in the suite, and
after any change to `ChartResponse` or the Frankfurter response structs. If the fresh capture
still parses, the schema is compatible; if it does not, the upstream moved. (An automated version
of that check was written and deliberately dropped — it needs two third parties to be reachable
from a runner, which no pull request controls, so it would have failed for reasons nobody could
act on.)

Two mechanics to know if you record by hand. In `Mode::Record` a stub that fails to match falls
through to the *real* target, which is why the Rust fixtures that need record mode point at
`http://unreachable.invalid`. And an attached snapshot is a dedup cache: a second pass fetches
only what is not already in the file, so refreshing a stale exchange means deleting it first —
which `pnpm fixtures:record` does for you.

The Playwright suites are a separate matter: both hard-code `SURE_TESTPROXY_MODE=replay` with an
*empty* snapshot directory, so every answer there comes from a stub the running test registered.
That is the strongest form of the containment guarantee, and it is also required — a request
answered from a snapshot is not recorded ("A snapshot-served request is not recorded", below), so
`assertCount` cannot see it.

## Gotchas, and the failure each produces

* **`default_mode(Replay)` turns recording off.** `ProxyClusterBuilder::default_mode` does not
  only set a mode — it overwrites the recording config (`Record` → enabled, `Replay` → disabled).
  Left at that default, every `assertCount`/`queryTraffic` can only answer zero,
  which reads as "the app never made the call" and sends you hunting a bug in the adapter.
  `sure_testproxy::start` therefore calls `.recording(RecordingConfig::default())` *after*
  `default_mode`, and `packages/testproxy/tests/recording.rs` pins both halves — so the diagnosis
  comes from the crate that owns the behaviour (and, in the pre-commit hook, before `pnpm test:api`
  runs at all) rather than as a spec insisting the app made no call. A cluster you build inline
  needs the same call, or `add_stub`, which leaves the recording config alone.
* **A snapshot-served request is not recorded.** The dedup rule: `listener.rs` returns early on
  `ResponseSource::Snapshot`, because the exchange is already on record. A *stub*-served one is
  recorded. So `assertCount` cannot see a call a snapshot answered — assert on the observable
  result instead. Every fixture in the Playwright suite is a stub today, so every call is visible;
  this bites the first spec that replays from a file and then counts.
* **The Landlock sandbox permits outbound 443 and 53 only.** It derives nothing from the
  configured endpoints (see [SANDBOX.md](SANDBOX.md)), so a proxy on an ephemeral port is simply
  unreachable and the failure arrives as an ordinary connection error from whichever adapter was
  pointed at it, naming nothing about Landlock. A harness must pass
  `SURE_SANDBOX_CONNECT_PORTS` with the listener ports out of the handshake. It is a no-op on
  macOS, which is exactly why it is easy to leave out: everything is green locally and red in
  CI's Linux container.
* **Nothing but `Mode::Record` may write a snapshot file.** One storage backend is both the replay
  source and the recording sink, so a replay cluster with the recorder on and a `JsonlStorage`
  attached would append every stub-served exchange and every 503 miss into a committed fixture,
  growing it on each CI run. Replay reads the file into an `InMemoryStorage` copy instead, and
  skips the attach entirely when the file does not exist — otherwise a replay-only run dirties the
  tree with three empty `.ndjson` files it never read.
* **A `record`-mode proxy would pass every test while dialling the real internet.** Which is why
  both harnesses state the mode rather than inheriting it, and then assert `mode === "replay"`
  off the handshake line — the one place the property can be confirmed instead of assumed.

## Verifying the guards themselves

```bash
node scripts/pii-scan.mjs --all    # sweep the tree (the pre-commit hook scans staged additions)
cargo test -p sure-testproxy       # the recording/no-write properties `start` depends on
cargo test -p sure-providers --test proxy_contract   # the three pinned partly-proxy behaviours
```

`proxy_contract.rs` is the file to read first if `partly-proxy-lib`'s pinned git rev is bumped:
it fails with the name of whichever load-bearing property went away, instead of leaving a fleet
of provider fixtures failing mysteriously.
