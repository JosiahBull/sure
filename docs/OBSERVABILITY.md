# Observability

Sure pushes OpenTelemetry **traces, metrics and logs** over OTLP/HTTP. There is no `/metrics`
endpoint to scrape — the process talks to a collector, which is what lets it work from behind a
NAT with one outbound connection and no inbound port.

**All of it is off unless you configure it.** With `OTEL_EXPORTER_OTLP_ENDPOINT` unset, no SDK is
built, no exporter thread is spawned, and every instrument is a no-op that costs an atomic load.
That is the state of `pnpm dev`, `cargo test`, both Playwright suites, and the container as
shipped.

## Quick start

```sh
docker compose --profile observability up -d
```

Then uncomment the four telemetry lines under the `sure` service in `docker-compose.yml` and
`docker compose up -d sure`. Grafana is on <http://localhost:3000> with the **Sure** dashboard
provisioned; VictoriaMetrics' own query UI is on <http://localhost:8428/vmui>.

Without Docker, point the app at any OTLP/HTTP collector:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 \
SURE_SANDBOX_CONNECT_PORTS=4318 \
./target/release/sure-api
```

## The one thing that will catch you: the sandbox

`sure_server::sandbox` permits outbound TCP to **443 and 53 only**, and it deliberately derives
nothing from configuration — the policy it logs should be predictable from what you set, so a
port that appears because of some *other* variable would defeat that.

So a plaintext collector on `:4318` needs its port named:

```sh
SURE_SANDBOX_CONNECT_PORTS=4318
```

Without it the connection is refused by the kernel, and it surfaces as an ordinary connection
error from the exporter that mentions nothing about Landlock. Two things follow:

- **A collector reached over `https://` on 443 needs no sandbox change at all.** That is the
  simplest production setup.
- Landlock's network rules match on **port, not host**. Allowing 4318 allows connecting to 4318
  anywhere, so prefer 443 + TLS if the collector is not on this machine.

The sandbox is Linux-only, so none of this bites on a macOS dev machine — which is exactly why
it is worth knowing before deploying. `docs/SANDBOX.md` has the whole policy.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | *(unset)* | **The master switch.** Base URL of the collector, e.g. `http://otel-collector:4318`, with no signal path. Unset disables everything. An unusable URL is **fatal at startup** |
| `SURE_OTEL_SIGNALS` | `traces,metrics` | Comma list of `traces`, `metrics`, `logs`; or `off`. An unrecognised name is **fatal** |
| `SURE_OTEL_METRICS_INTERVAL_SECS` | `60` | How often metrics are exported |
| `SURE_OTEL_SAMPLE_INTERVAL_SECS` | `300` | How often the domain gauges are recomputed (see below) |
| `SURE_OTEL_FILTER` | see below | `EnvFilter` directives for what reaches the OTLP layers, independent of `RUST_LOG` |
| `SURE_OTEL_LOG_LEVEL` | `info` | Ceiling on an exported **log record**'s level, on top of the filter |
| `OTEL_SERVICE_NAME` | `sure` | `service.name` resource attribute |

`OTEL_RESOURCE_ATTRIBUTES` and `OTEL_TRACES_SAMPLER` are read by the OpenTelemetry SDK itself
and work as specified; they are not parsed by `sure_server::config`.

The signal paths are appended to your base URL: `/v1/traces`, `/v1/metrics`, `/v1/logs`.

### Why logs are not on by default

`SURE_OTEL_SIGNALS` defaults to `traces,metrics`. Turning logs on is one token, and it is a
deliberate one:

- `sure_dal::connect` sets sqlx's `log_statements` to TRACE, so at that level the log stream is
  SQL **with its bound parameters** — account numbers, payees, amounts.
- Every handler's `#[instrument(err)]` renders an error's `Display`, which can include whatever
  was submitted.
- `SyncService` warnings carry upstream error text verbatim.

Traces and metrics carry labels from closed sets and route *templates* (`/api/accounts/{id}`,
never the id), so they are structurally much harder to leak through. Logs are free text. This is
the same question CLAUDE.md rule 3 asks about the repository, asked about the running process.

Two guards are in the default filter and should stay there:

- **`sqlx=off`** — the above.
- **`opentelemetry=off`** — the SDK reports its own export failures through `tracing`. Feeding
  those back into the exporter is a loop that gets louder the worse the collector is doing.

The default filter is:

```
info,sure_api=debug,sure_app=debug,sure_dal=debug,sure_mcp=debug,sure_providers=debug,sure_scheduler=debug,sqlx=off,opentelemetry=off
```

Our crates run at `debug` so handler and DAL spans nest under the request span — that nesting is
most of why traces are worth exporting. `SURE_OTEL_LOG_LEVEL=info` is what stops that *also*
exporting every `ret(DEBUG)` event, whose fields are the rows being returned. If you raise the
log level, you are choosing to ship ledger contents.

## Metric catalogue

Durations are **seconds**; money is **minor units**. `packages/telemetry/src/instruments.rs` is
the authoritative list — it is one file for exactly this reason.

### HTTP
| Metric | Type | Attributes |
| --- | --- | --- |
| `http.server.request.duration` | histogram | `http.request.method`, `http.route`, `http.response.status_code`, `error.type` |
| `http.server.active_requests` | up/down counter | — |

`http.route` is axum's matched route template, and `error.type` is `AppError::code` — both closed
sets, so neither can be driven wide by a client.

### SQLite
| Metric | Type | Attributes |
| --- | --- | --- |
| `db.client.operation.duration` | histogram | `db.operation.name` (`module.function`) |
| `sure.db.busy_retries` | counter | `operation`, `attempt` |
| `db.client.connection.count` | gauge | `db.client.connection.state` = `idle`\|`used` |
| `db.client.connection.max` | gauge | — |

`db.client.operation.duration` comes from a `tracing` layer over the `#[instrument]` spans
`sure-dal` already carries, so every repository function is timed with no call-site code.

### Providers
| Metric | Type | Attributes |
| --- | --- | --- |
| `sure.provider.request.duration` | histogram | `provider`, `operation`, `outcome` |
| `sure.provider.response.bytes` | histogram | `provider` |
| `sure.provider.sweep.pages` | histogram | `provider` |
| `sure.provider.sweep.limited` | counter | `provider`, `limit` = `time`\|`pages` |
| `sure.provider.throttle.wait.duration` | histogram | `provider` |

`sweep.limited` is worth an alert: it means a transaction sweep stopped at a ceiling rather than
at the end of the data. Yahoo turns a 404 for a delisted ticker into an empty result, so that
appears as `outcome=ok` with no rows rather than an error.

### Background work and use-cases
| Metric | Type | Attributes |
| --- | --- | --- |
| `sure.scheduler.job.total` / `.duration` | counter / histogram | `job`, `outcome` |
| `sure.provider.sync.duration` | histogram | `provider_kind`, `outcome` = `ok`\|`error`\|`conflict` |
| `sure.provider.sync.transactions` | counter | `provider_kind`, `disposition` |
| `sure.rules.run.duration` / `.rows` | histogram / counter | `kind`, `disposition` |
| `sure.import.commit.duration` / `sure.import.rows` | histogram / counter | `source`, `disposition` |
| `sure.report.duration` | histogram | `report`, `phase` = `load`\|`compute` |
| `sure.forecast.simulate.duration` | histogram | — |
| `sure.brokerage.backfill.duration` | histogram | — |

`report.duration`'s `phase` split is free because `sure_app::reports` already separates reading
from calculating — so "slow because of SQLite, or because of the arithmetic?" is answerable.

### Domain gauges
Written by the sampler in `packages/server/src/sampler.rs`, not by requests.

| Metric | Attributes |
| --- | --- |
| `sure.accounts.count` | `class` |
| `sure.net_worth.minor` | `currency` |
| `sure.provider.last_sync.age` | `provider_kind`, `provider_name` |
| `sure.scheduled_task.last_run.age` | `job` |
| `sure.transactions.uncategorized.count` | — |
| `sure.fx.unconverted.currencies` | — |
| `sure.tasks.tracked` | — |

`provider.last_sync.age` is the one to alert on. `providers.last_synced_at` is written **only on
success**, so a feed that has been erroring for a week shows a growing age here even if nobody
is watching its error counter.

The sampler runs on a longer interval than the export (300s vs 60s) on purpose: net worth reads
every account's valuations and transaction sums, and a gauge that costs a ledger scan every
minute forever is a self-inflicted load problem. A gauge re-exported unchanged is not a stale
gauge.

## Querying: two syntax surprises

VictoriaMetrics stores OTLP names **as they arrive** — the dots survive, and no unit suffix is
added. Only histograms gain `_bucket`/`_count`/`_sum`. Attribute keys keep their dots too. So:

```promql
# NOT http_server_request_duration_seconds_bucket
histogram_quantile(0.95, sum by (le, "http.route") (
  rate({__name__="http.server.request.duration_bucket"}[5m])))
```

Both quotings are required: `{__name__="a.b.c"}` because a dotted name is not a bare identifier,
and `by (le, "http.route")` because a dotted label name is not either. The widely-documented
`.`→`_` rewrite is what the *Prometheus exporter* does; VM's OTLP ingestion does not. A dashboard
written for the wrong one matches nothing and shows an empty panel rather than an error.

`deploy/grafana/dashboards/sure.json` has a working query for every panel; every one of them was
run against VictoriaMetrics before being written down.

## Without the collector

The collector exists so the app has one endpoint and one sandbox port, and so a VictoriaMetrics
restart costs a retry rather than a window of data. To skip it, point each signal at its store
directly — the paths are not the OTLP defaults, which is why this is the more fiddly option:

```sh
OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=http://victoriametrics:8428/opentelemetry/v1/metrics
OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=http://victorialogs:9428/insert/opentelemetry/v1/logs
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://victoriatraces:10428/insert/opentelemetry/v1/traces
SURE_SANDBOX_CONNECT_PORTS=8428,9428,10428
```

Those are read by the SDK, which uses a per-signal endpoint **verbatim** rather than appending
`/v1/<signal>` to it. VictoriaMetrics wants cumulative temporality, which is what the app
exports; do not enable deduplication or downsampling if you switch that.

## How it is wired, and the two orderings that matter

`packages/telemetry` (`sure-telemetry`) owns the SDK, the exporters, the instrument registry and
the `tracing` layers. Like `sure-appbase` it depends on **nothing else in the workspace** —
every layer from the DAL up records into it, so anything it depended on could not be
instrumented.

Two orderings in `packages/server/src/main.rs` are load-bearing:

1. **The providers are built after `sandbox::apply` and before the tokio runtime.** In
   opentelemetry 0.32 the periodic metric reader and the batch span/log processors each spawn a
   plain OS thread when the provider is *built*, and `sandbox::apply` refuses to run once the
   process has more than one thread (`landlock_restrict_self` only restricts the caller, so a
   sibling would keep an unrestricted domain). Building them any earlier makes every start fail
   with *"sandbox::apply must run before any thread is spawned"*.

2. **`Guard::shutdown()` runs after `sure_appbase::run` returns.** Those exporter threads are not
   tokio tasks, so the drain cannot see them — which is the good half: they can never make a
   shutdown look unclean, and `specs/shutdown.spec.ts` still reports `clean=true`. The other half
   is that nothing else will flush them.

Because the OTLP layers therefore cannot exist when the subscriber is built, they are installed
through a `tracing_subscriber::reload` slot. One consequence is worth knowing before you touch
it: a **per-layer filter cannot be installed that way**. `Filtered` is assigned its `FilterId`
when the subscriber is built, and a layer carrying its own filter that arrives through a reload
panics on the first event with *"a `Filtered` layer was used, but it had no `FilterId`"*. So
`SURE_OTEL_FILTER` is attached to the slot itself, and the log bridge's extra ceiling rides
inside the layer as `sure_telemetry::max_level::MaxLevel`. There are tests for both in
`sure_api::telemetry`.

## Tests never export

`packages/api-tests/fixtures.ts` and `packages/web/tests/global-setup.ts` strip every `OTEL_*`
and `SURE_OTEL_*` variable from the environment they hand a spawned backend. Without that, a
developer who has run the observability stack has `OTEL_EXPORTER_OTLP_ENDPOINT` exported, and
every test-spawned backend would build an SDK and push the suite's telemetry into their real
VictoriaMetrics.

Note that `sure-testproxy` cannot catch this for you: it is a reverse proxy with one listener per
named `Upstream` (Frankfurter, Yahoo, Akahu, House Pricer), so an exporter's connection never
reaches it and `failOnUnstubbedRequests` never sees it. Stripping is the guard, not stubbing.
