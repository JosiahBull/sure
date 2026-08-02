# HTTP behaviour

How the backend caches, compresses, and defends itself. The routes and their handlers are
covered in [ARCHITECTURE.md](ARCHITECTURE.md); this is about everything wrapped around
them.

The whole stack is assembled in one place —
[`sure_api::build_app`](../packages/api/src/lib.rs) — with the connection-level parts in
[`sure_server::http`](../packages/server/src/http.rs). Every knob has a working default,
so an unconfigured deployment is already protected.

## The one thing to keep in mind

**There is no authentication.** A request that reaches this API is served. Everything
below follows from that: responses are `private`, shared caches are told `no-store`, and
CORS is an allowlist rather than a convenience.

## Caching

Policy is a single table in [`packages/api/src/cache.rs`](../packages/api/src/cache.rs),
keyed on the route template axum matched. It fails closed — a route that isn't named gets
the private, revalidating policy, never a public one.

| What | `Cache-Control` | `CDN-Cache-Control` |
| --- | --- | --- |
| Any mutation, and every error | `no-store` | `no-store` |
| `/api/health`, `/api/config/export`, `/api/provider-kinds/{kind}/accounts` | `no-store` | `no-store` |
| **Every other `/api` read** | `private, no-cache` | `no-store` |
| `/api/forecast` | `private, max-age=60, stale-while-revalidate=300` | `no-store` |
| `/api/accounts/{id}/stock-price` | `private, max-age=300, stale-while-revalidate=1500` | `no-store` |
| `/assets/**`, `/workbox-*.js` | `public, max-age=31536000, immutable` | same |
| `index.html`, `sw.js`, `registerSW.js`, `manifest.webmanifest`, SPA fallback | `public, max-age=0, must-revalidate` | `no-cache` |
| `/favicon.svg`, `/icon-*.png`, `/fonts/**` | `public, max-age=86400, stale-while-revalidate=604800` | `public, max-age=86400` |

### Conditional requests

`private, no-cache` means "ask every time", not "don't cache". Paired with the weak `ETag`
that [`etag.rs`](../packages/api/src/etag.rs) attaches, asking costs one empty `304`
instead of re-downloading a transaction list. The tags are **weak** because compression
runs outside the layer that computes them, so one tag legitimately covers the identity,
gzip, brotli, and zstd forms of the same response.

Static files get tags from the same layer. `ServeDir` offers only `Last-Modified`, which
has one-second granularity — the shell and the service worker revalidate on every app
launch, so a precise validator matters there.

### Why reports don't get a `max-age`

`/api/reports/*` is the expensive part of the API, and holding results for a minute is
tempting. It would also mean editing a transaction and then seeing stale numbers. They
revalidate instead: `304` saves the payload but not the recompute. Cutting the recompute
needs a server-side cache invalidated by every mutation — a bigger change than this.

### Cloudflare

Nothing here requires Cloudflare, and the CDN headers are inert without it. When one is in
front:

* `CDN-Cache-Control` and `Cloudflare-CDN-Cache-Control` are the RFC 9213 targeted
  directives. Cloudflare gives its own vendor header precedence over the generic one, so
  both are emitted rather than betting on which is read.
* **Do not add a "Cache Everything" rule for `/api`.** Such a rule overrides ordinary cache
  directives; on an API with no authentication that publishes one household's finances to
  whoever asks next. The `no-store` above is the backstop, not a licence to try it.
* A cache rule for `/assets/*` is safe and unnecessary — those responses are already
  `public, immutable`, and Cloudflare honours that on its own.
* To rate-limit by real client IP behind Cloudflare, set `TRUST_PROXY_HEADERS=true`. That
  makes the server believe `CF-Connecting-IP`/`X-Forwarded-For`, which is only correct when
  the peer is genuinely a proxy you control — otherwise any client can pick its own
  rate-limit key.

## Compression

Brotli, gzip, and zstd, at a quality deliberately below each algorithm's default: the top
levels cost far more CPU than they save bytes on JSON, and this runs on a small box. The
OpenAPI document compresses from ~99 KB to ~16 KB. Bodies under 32 bytes, images, gRPC,
and server-sent events are skipped. Disable with `COMPRESSION=off`.

**Request decompression is deliberately absent.** `RequestDecompressionLayer` has no
decompressed-size cap, so accepting `Content-Encoding: gzip` on `/api/config/import` would
turn it into a zip-bomb target. Nothing in the app needs it.

## HTTP/2 and HTTP/3

The server speaks **HTTP/1.1 and cleartext HTTP/2 (h2c)** on the same port; hyper detects
the protocol from the connection preface. h2c is for a reverse proxy configured to talk
HTTP/2 to its origin — no browser negotiates HTTP/2 without TLS.

**HTTP/3 is not implemented in this binary, and shouldn't be.** It needs QUIC over UDP,
TLS 1.3, and a certificate in-process, and behind any TCP-based proxy the origin never
sees it. Browsers get HTTP/2 and HTTP/3 from whatever terminates TLS:

* Cloudflare (proxied DNS or a tunnel) does both automatically, including the `Alt-Svc`
  advertisement.
* Self-hosting without Cloudflare, put Caddy in front — it does HTTP/3 with an automatic
  certificate and proxies to this server over HTTP/1.1 or h2c.
* On a LAN or Tailscale, HTTP/1.1 over a local link is not the bottleneck.

## Abuse guards

### Per connection ([`server/src/http.rs`](../packages/server/src/http.rs))

`axum::serve` builds its hyper connection internally and exposes none of these, so the
accept loop is written out. One of them isn't optional: hyper's `header_read_timeout`
defaults to 30s but is **silently disabled unless a timer is installed on the builder**,
which `axum::serve` never does. Without it, a client that opens a connection and sends one
byte holds it open forever.

| Guard | Default | Env |
| --- | --- | --- |
| Concurrent connections | 512 | `MAX_CONNECTIONS` |
| Request-head read timeout (slowloris) | 15s | `HEADER_READ_TIMEOUT_SECS` |
| HTTP/1 buffer per connection | 64 KiB | `HTTP1_MAX_BUF_BYTES` |
| HTTP/2 concurrent streams | 128 | `H2_MAX_CONCURRENT_STREAMS` |
| Shutdown drain deadline | 15s | `SHUTDOWN_GRACE_SECS` |

The loop also refuses to spin on a failed `accept()` — descriptor exhaustion backs off
rather than retrying thousands of times a second.

Note that `SHUTDOWN_GRACE_SECS` bounds only the *connection* drain. Signals, cancellation,
and waiting for everything else the process spawned belong to
[`sure-appbase`](../packages/appbase/src/lib.rs), which wraps the whole thing — see
[Shutdown](#shutdown).

## Shutdown

`SIGTERM`/`SIGINT` drains in-flight requests, then the background scheduler, and only then
closes the database pool, so a container restart doesn't cut a SQLite write short. The
sequence lives in `sure-appbase` rather than here because it has to outlive the HTTP
server: the accept loop returning is the *middle* of shutting down, not the end.

Its phases each get their own grace period, and the whole sequence is capped at their sum —
separate numbers rather than one budget carved up by subtraction, so an overrunning phase
can never leave a later one with a negative remainder.

| Phase | Default | Env |
| --- | --- | --- |
| Keep serving after a signal, before cancelling | 0s | `SHUTDOWN_PREDRAIN_SECS` |
| `serve` to return (covers the connection drain **and** the pool close) | 30s | `SHUTDOWN_APP_GRACE_SECS` |
| Tasks spawned through the `Shutdown` handle | 10s | `SHUTDOWN_DRAIN_GRACE_SECS` |
| Blocking-pool backstop | 5s | `SHUTDOWN_BLOCKING_GRACE_SECS` |

The pre-drain delay is zero because nothing routes to this process — behind a load
balancer it wants roughly the health-check interval, so the balancer stops sending traffic
before the server stops accepting it.

What makes this checkable rather than hopeful: tasks are *tracked*
(`tokio_util::task::TaskTracker`), not counted. A shutdown that leaves work running says so
on the way out —

```
WARN sure_appbase: drain deadline exceeded; tasks left running (spawn sites below) abandoned=1
WARN sure_appbase: task still running at shutdown site="packages/server/src/lib.rs:173:14"
INFO sure_appbase: shutdown complete trigger="terminate" app="finished" drain="timed_out" ... clean=false
```

— naming the line that spawned it, because `Shutdown::spawn` is `#[track_caller]` and debug
builds keep the call site. The `clean=true` case is asserted end-to-end by
[`shutdown.spec.ts`](../packages/api-tests/specs/shutdown.spec.ts). The catch is that only
tasks spawned *through the handle* are visible; a bare `tokio::spawn` is not tracked and
will be abandoned silently.

### Per request ([`api/src/limits.rs`](../packages/api/src/limits.rs))

| Guard | Default | Env |
| --- | --- | --- |
| Request body | 2 MiB | `MAX_BODY_BYTES` |
| ↳ `POST /api/config/import` | 32 MiB | `MAX_SNAPSHOT_BODY_BYTES` |
| ↳ `POST /api/accounts/{id}/brokerage/import` | 50 MiB | `MAX_IMPORT_BODY_BYTES` |
| Request deadline | 30s | `REQUEST_TIMEOUT_SECS` |
| ↳ syncs, imports, backfills, whole-ledger rule and cron runs | 300s | `LONG_REQUEST_TIMEOUT_SECS` |
| Requests in flight before shedding | 64 | `MAX_IN_FLIGHT` |
| Per-client rate | 50/s, burst 200 | `RATE_LIMIT_RPS`, `RATE_LIMIT_BURST` |
| Loopback exempt from the rate limit | yes | `RATE_LIMIT_EXEMPT_LOOPBACK` |
| Largest response given an `ETag` | 8 MiB | `MAX_ETAG_BODY_BYTES` |

The in-flight ceiling is the one that matters in practice: the realistic failure is a
handful of concurrent `/api/forecast` or `/api/reports/*` calls saturating the CPU, not a
botnet. It **sheds** rather than queues — a caller that gets a fast `503` with
`Retry-After` can back off, whereas a queue turns a burst into a pile of requests that all
time out together, having each held a connection and a database handle on the way.

Loopback is exempt from the rate limit by default: the same host runs `pnpm seed`, ad-hoc
`curl`, and the e2e suite.

### Headers and CORS ([`api/src/security.rs`](../packages/api/src/security.rs))

Every response carries `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`,
`X-Frame-Options: DENY`, and `Cross-Origin-Opener-Policy: same-origin` — including the
`429`s and `503`s the limiters generate, since a browser needs to be able to read those.

CORS is an allowlist, defaulting to `https://sure.bullfamilies.com` plus the two Vite dev
origins. Override with `CORS_ALLOWED_ORIGINS` (comma-separated); set it empty to disable
cross-origin access entirely. The SPA never needs it — it is same-origin in development
(Vite proxies `/api`) and in production (the binary serves `WEB_DIR`) — so the allowlist
exists only for the deployed hostname and a dev server pointed straight at the backend.
Credentials are never enabled.

There is **no Content-Security-Policy yet**. It needs validating against the Svelte build's
inline styles and the committed screenshot baselines; worth a focused change of its own.

## Error responses

Every `/api` failure comes back as `{ "error": { "code", "message" } }`, including the ones
generated below the handlers:

| Status | `code` | From |
| --- | --- | --- |
| `408` | `timeout` | the per-route deadline |
| `413` | `payload_too_large` | the body cap |
| `429` | `rate_limited` | the per-client rate limit (with `Retry-After`) |
| `503` | `overloaded` | the in-flight ceiling (with `Retry-After`) |
| `5xx` | `internal` | scrubbed, carrying only a `request_id` to correlate with the logs |

These statuses are **not in the OpenAPI document** — declaring them would mean editing
~45 `#[utoipa::path]` blocks for responses no client branches on. The envelope shape is the
same either way, so a generated client parses them fine.

Static responses keep their own bodies: `ServeDir::not_found_service` deliberately serves
the SPA shell *with* a 404 status, and replacing that with JSON would stop the app booting
on a deep link.

## Verifying by hand

```bash
pnpm build
WEB_DIR=packages/web/dist DATABASE_URL=sqlite:data/sure.db ./target/release/sure-api

curl -sI localhost:8080/api/accounts        # private, no-cache + a W/"…" etag
curl -sI localhost:8080/assets/index-*.js   # public, max-age=31536000, immutable
curl -s -o /dev/null -w '%{size_download}\n' --compressed localhost:8080/api/openapi.json
curl -s -o /dev/null -w '%{http_version}\n' --http2-prior-knowledge localhost:8080/api/health
```

The behaviour is covered end to end by
[`packages/api-tests/specs/http.spec.ts`](../packages/api-tests/specs/http.spec.ts), which
drives the real binary — including h2c over `node:http2` and a `SIGTERM` drain.
