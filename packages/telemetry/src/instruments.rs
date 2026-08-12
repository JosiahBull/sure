//! The instrument registry: every counter, histogram and gauge in the application, created
//! once.
//!
//! # Why they live here rather than at the call sites
//!
//! `Meter::f64_histogram(..).build()` is not free — it takes a lock, looks the instrument up
//! by name and unit, and validates it against what is already registered. Doing that per call
//! turns a metric into a contention point on the hot path. So each instrument is built once,
//! behind a [`LazyLock`], and a call site does `instruments().http_request_duration.record(..)`.
//!
//! Naming them in one file has a second benefit that matters more over time: this is the
//! metric catalogue. Adding one here, with the unit and the attribute keys it expects, is how
//! the set stays reviewable — rather than being spread across thirty modules where two of them
//! spell the same measurement differently.
//!
//! # When export is off
//!
//! [`opentelemetry::global::meter`] returns a no-op meter until `otel::init` sets a real
//! provider, and instruments built from it discard their measurements. The [`LazyLock`] is
//! therefore resolved *lazily on first record*, not at startup — which is what lets
//! `otel::init` run first and these still be live. A `record` in a process with export off
//! costs an `Arc` deref and a virtual call.
//!
//! # Naming
//!
//! OTEL semantic conventions where a stable one exists (`http.server.request.duration`,
//! `db.client.operation.duration`), and `sure.*` for everything the conventions do not cover.
//!
//! VictoriaMetrics stores these names **as they are**: the dots survive ingestion, no unit
//! suffix is added, and only a histogram gains `_bucket`/`_count`/`_sum`. Attribute keys keep
//! their dots too. So the series is `http.server.request.duration_bucket` with an
//! `http.route` label — which means PromQL has to quote both:
//!
//! ```promql
//! histogram_quantile(0.95, sum by (le, "http.route") (
//!   rate({__name__="http.server.request.duration_bucket"}[5m])))
//! ```
//!
//! (Verified against VictoriaMetrics 1.136 rather than assumed — the widely-documented
//! `.`-to-`_` rewrite is what the *Prometheus* exporter does, not what VM's OTLP ingestion
//! does, and a dashboard written for the wrong one silently matches nothing.)

use std::sync::LazyLock;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter, UpDownCounter};

/// The instrumentation scope name. Appears on every metric as `otel_scope_name`.
const SCOPE: &str = "sure";

/// Seconds, for every duration in here. OTEL semconv requires seconds for `*.duration`, and
/// VictoriaMetrics/Grafana dashboards assume it — a histogram in milliseconds silently makes
/// every bucket boundary and every `histogram_quantile` result wrong by 1000.
const SECONDS: &str = "s";
/// UCUM for bytes, which is what semconv and Prometheus both expect for a size.
const BYTES: &str = "By";

// ---- Bucket boundaries -------------------------------------------------------------------
//
// Every histogram below names its own, and none may be left to the SDK's default. That default
// is `[0, 5, 10, 25, 50, 75, 100, 250, 500, 750, 1000, 2500, 5000, 7500, 10000]` — boundaries
// shaped for **milliseconds**. Recorded in seconds, as semconv requires and as `secs` does, a
// 2ms request and a 4-second one both land in the `le=5` bucket, and `histogram_quantile`
// answers with an interpolation across four orders of magnitude. It is not an error and nothing
// warns about it: the dashboard simply reports a number that is not the latency.
//
// (Found exactly that way — by pushing a real export into VictoriaMetrics and reading the `le`
// labels back, which is the only place it is visible.)

/// The OTEL semconv recommendation for `http.server.request.duration`.
const HTTP_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// The OTEL semconv recommendation for `db.client.operation.duration`. Tighter at the bottom
/// than HTTP: a local SQLite read is tens of microseconds, and the interesting question is when
/// one starts taking milliseconds.
const DB_BUCKETS: &[f64] = &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0];

/// An outbound provider call. Bounded above by `sure_providers`' own 6s request timeout, so the
/// buckets bracket that rather than running to 10s.
const UPSTREAM_BUCKETS: &[f64] = &[0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 4.0, 6.0, 10.0];

/// Waiting on an adapter's self-imposed rate limit — bounded by `MIN_REQUEST_INTERVAL` (500ms)
/// per hop, but a queue of callers stacks those up.
const THROTTLE_BUCKETS: &[f64] = &[0.001, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0];

/// Background work and heavy computation: a provider sweep, a rule run over the ledger, a
/// Monte Carlo simulation, a valuation backfill. Minutes are normal here, so the top bucket is
/// ten of them rather than ten seconds.
const JOB_BUCKETS: &[f64] = &[0.1, 0.5, 1.0, 5.0, 15.0, 60.0, 300.0, 600.0];

/// A response body, against the 8 MiB ceiling in `sure_providers::http`.
const BYTE_BUCKETS: &[f64] = &[
    1_024.0,
    8_192.0,
    65_536.0,
    262_144.0,
    1_048_576.0,
    4_194_304.0,
    8_388_608.0,
];

/// Pages in one paginated sweep, against Akahu's 100-page cap.
const PAGE_BUCKETS: &[f64] = &[1.0, 2.0, 3.0, 5.0, 10.0, 25.0, 50.0, 100.0];

/// Every instrument the application records into.
pub struct Instruments {
    // ---- HTTP (semconv) ------------------------------------------------------------------
    /// `http.request.method`, `http.route`, `http.response.status_code`, and `error.type`
    /// when the response carried one.
    pub http_request_duration: Histogram<f64>,
    /// In-flight requests. Recorded with a `Drop` guard, so a cancelled or panicking request
    /// still decrements.
    pub http_active_requests: UpDownCounter<i64>,

    // ---- Database ------------------------------------------------------------------------
    /// `db.operation.name` — the name of the `#[tracing::instrument]` span in `sure-dal`,
    /// which is the repository function's own name. Produced by
    /// [`crate::span_duration::DurationLayer`], not by a call site.
    pub db_operation_duration: Histogram<f64>,
    /// `operation`, `attempt`. A SQLite write refused because another writer held the lock.
    /// Non-zero is normal under concurrency; growing steadily means writers are queueing.
    pub db_busy_retries: Counter<u64>,
    /// `db.client.connection.state` = `idle` | `used`. Sampled, not event-driven.
    pub db_pool_connections: Gauge<u64>,
    /// The pool ceiling, so a dashboard can draw utilisation without hard-coding it.
    pub db_pool_max: Gauge<u64>,

    // ---- Scheduled jobs ------------------------------------------------------------------
    /// `job`, `outcome`. Every run the scheduler actually starts, by how it ended.
    pub scheduler_job_total: Counter<u64>,
    /// `job`, `outcome`. Absent for a run that never started (the schedule could not be read).
    pub scheduler_job_duration: Histogram<f64>,

    // ---- Provider adapters ---------------------------------------------------------------
    /// `provider`, `operation`, `outcome`. One outbound call to a bank, price feed or FX API.
    pub provider_request_duration: Histogram<f64>,
    /// `provider`. Decompressed JSON actually read back, against the 8 MiB body ceiling.
    pub provider_response_bytes: Histogram<u64>,
    /// `provider`. Pages fetched by one incremental transaction sweep.
    pub provider_sweep_pages: Histogram<u64>,
    /// `provider`, `limit` = `time` | `pages`. A sweep that stopped at a ceiling rather than at
    /// the end of the data — the condition that silently leaves transactions unimported.
    pub provider_sweep_limited: Counter<u64>,
    /// `provider`. Time spent waiting on an adapter's self-imposed rate limit. Invisible
    /// otherwise: it is a sleep inside a mutex, so it shows up only as latency somewhere else.
    pub provider_throttle_wait: Histogram<f64>,

    // ---- Application use-cases -----------------------------------------------------------
    /// `provider_kind`, `outcome` = `ok` | `error` | `conflict`. A whole provider sync: fetch,
    /// import, categorise, balance refresh. `conflict` is the single-flight refusal, which does
    /// no work at all.
    pub sync_duration: Histogram<f64>,
    /// `provider_kind`, `disposition` = `imported` | `skipped`. Rows a sync accounted for.
    pub sync_transactions: Counter<u64>,
    /// `report`, `phase` = `load` | `compute`. The report services already split reading from
    /// calculating, so the two are separable without guessing which one is slow.
    pub report_duration: Histogram<f64>,
    /// The Monte Carlo simulation, which is the heaviest single computation in the app.
    pub forecast_duration: Histogram<f64>,
    /// `kind` = `single` | `all` | `auto`. A rule run over the ledger.
    pub rules_run_duration: Histogram<f64>,
    /// `kind`, `disposition` = `matched` | `changed`. What a rule run did.
    pub rules_run_rows: Counter<u64>,
    /// `source`. Committing a parsed upload.
    pub import_duration: Histogram<f64>,
    /// `source`, `disposition` = `imported` | `skipped`. Rows an upload accounted for.
    pub import_rows: Counter<u64>,
    /// The post-import valuation backfill, which routinely outlives the response that started
    /// it — so its duration appears nowhere else.
    pub brokerage_backfill_duration: Histogram<f64>,

    // ---- Domain gauges (sampled) ---------------------------------------------------------
    // These answer "is the application doing its job?", where everything above answers "is the
    // process healthy?". A server can serve every request in 2ms and still have not spoken to a
    // bank in a fortnight. All are written by the sampler in `sure-server`, never by a request.
    /// `class` — the `AccountClass` roll-up (cash, liability, investment, asset).
    pub accounts_count: Gauge<u64>,
    /// `currency`. Net worth in minor units, honouring `excluded_from_net_worth`.
    pub net_worth_minor: Gauge<i64>,
    /// `provider_kind`, `provider_name`. Seconds since a provider last synced **successfully**
    /// — `providers.last_synced_at` is only written on success, which is what makes this the
    /// number that catches a feed that has been quietly failing.
    pub provider_last_sync_age: Gauge<i64>,
    /// `job`. Seconds since a scheduled task last completed. The counterpart to the above for
    /// work that has no provider.
    pub scheduled_task_last_run_age: Gauge<i64>,
    /// Transactions with no category. A backlog that only grows means the rule set has stopped
    /// covering what is arriving.
    pub transactions_uncategorized: Gauge<u64>,
    /// Currencies held with no reachable exchange rate, so their value is being left out of
    /// every converted total. A correctness signal, not a performance one.
    pub fx_unconverted_currencies: Gauge<u64>,
    /// Tasks `sure_appbase::Shutdown` is currently tracking — the exact in-flight count of
    /// background work this process would have to drain to stop cleanly.
    pub tracked_tasks: Gauge<u64>,
}

impl Instruments {
    fn new(meter: &Meter) -> Self {
        Self {
            http_request_duration: meter
                .f64_histogram("http.server.request.duration")
                .with_boundaries(HTTP_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of inbound HTTP requests.")
                .build(),
            http_active_requests: meter
                .i64_up_down_counter("http.server.active_requests")
                .with_description("Requests currently being handled.")
                .build(),
            db_operation_duration: meter
                .f64_histogram("db.client.operation.duration")
                .with_boundaries(DB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of a repository operation against SQLite.")
                .build(),
            db_busy_retries: meter
                .u64_counter("sure.db.busy_retries")
                .with_description("Transactions replayed because SQLite reported the lock busy.")
                .build(),
            db_pool_connections: meter
                .u64_gauge("db.client.connection.count")
                .with_description("Connections in the SQLite pool, by state.")
                .build(),
            db_pool_max: meter
                .u64_gauge("db.client.connection.max")
                .with_description("The configured ceiling on pool connections.")
                .build(),
            scheduler_job_total: meter
                .u64_counter("sure.scheduler.job.total")
                .with_description("Scheduled job runs, by outcome.")
                .build(),
            scheduler_job_duration: meter
                .f64_histogram("sure.scheduler.job.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of a scheduled job run.")
                .build(),
            provider_request_duration: meter
                .f64_histogram("sure.provider.request.duration")
                .with_boundaries(UPSTREAM_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of one outbound provider request.")
                .build(),
            provider_response_bytes: meter
                .u64_histogram("sure.provider.response.bytes")
                .with_boundaries(BYTE_BUCKETS.to_vec())
                .with_unit(BYTES)
                .with_description("Size of a provider's JSON response body.")
                .build(),
            provider_sweep_pages: meter
                .u64_histogram("sure.provider.sweep.pages")
                .with_boundaries(PAGE_BUCKETS.to_vec())
                .with_description("Pages fetched by one incremental transaction sweep.")
                .build(),
            provider_sweep_limited: meter
                .u64_counter("sure.provider.sweep.limited")
                .with_description("Sweeps that stopped at a page or time ceiling.")
                .build(),
            provider_throttle_wait: meter
                .f64_histogram("sure.provider.throttle.wait.duration")
                .with_boundaries(THROTTLE_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Time spent waiting on an adapter's own rate limit.")
                .build(),
            sync_duration: meter
                .f64_histogram("sure.provider.sync.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of a whole provider sync.")
                .build(),
            sync_transactions: meter
                .u64_counter("sure.provider.sync.transactions")
                .with_description("Transactions a sync imported or skipped.")
                .build(),
            report_duration: meter
                .f64_histogram("sure.report.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of a report, split into loading and computing.")
                .build(),
            forecast_duration: meter
                .f64_histogram("sure.forecast.simulate.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of one Monte Carlo forecast simulation.")
                .build(),
            rules_run_duration: meter
                .f64_histogram("sure.rules.run.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of a rule run over the ledger.")
                .build(),
            rules_run_rows: meter
                .u64_counter("sure.rules.run.rows")
                .with_description("Transactions a rule run matched or changed.")
                .build(),
            import_duration: meter
                .f64_histogram("sure.import.commit.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of committing a parsed upload.")
                .build(),
            import_rows: meter
                .u64_counter("sure.import.rows")
                .with_description("Rows an upload imported or skipped.")
                .build(),
            brokerage_backfill_duration: meter
                .f64_histogram("sure.brokerage.backfill.duration")
                .with_boundaries(JOB_BUCKETS.to_vec())
                .with_unit(SECONDS)
                .with_description("Duration of the post-import valuation backfill.")
                .build(),
            accounts_count: meter
                .u64_gauge("sure.accounts.count")
                .with_description("Non-archived accounts, by class.")
                .build(),
            net_worth_minor: meter
                .i64_gauge("sure.net_worth.minor")
                .with_description("Net worth in minor units of the base currency.")
                .build(),
            provider_last_sync_age: meter
                .i64_gauge("sure.provider.last_sync.age")
                .with_unit(SECONDS)
                .with_description("Seconds since a provider last synced successfully.")
                .build(),
            scheduled_task_last_run_age: meter
                .i64_gauge("sure.scheduled_task.last_run.age")
                .with_unit(SECONDS)
                .with_description("Seconds since a scheduled task last completed.")
                .build(),
            transactions_uncategorized: meter
                .u64_gauge("sure.transactions.uncategorized.count")
                .with_description("Transactions with no category.")
                .build(),
            fx_unconverted_currencies: meter
                .u64_gauge("sure.fx.unconverted.currencies")
                .with_description("Currencies held with no reachable exchange rate.")
                .build(),
            tracked_tasks: meter
                .u64_gauge("sure.tasks.tracked")
                .with_description("Background tasks the shutdown handle is tracking.")
                .build(),
        }
    }
}

static INSTRUMENTS: LazyLock<Instruments> =
    LazyLock::new(|| Instruments::new(&opentelemetry::global::meter(SCOPE)));

/// The process-wide instruments. Cheap enough to call per measurement.
pub fn instruments() -> &'static Instruments {
    &INSTRUMENTS
}

/// Seconds as `f64`, the unit every `*.duration` histogram here is built with.
///
/// A helper rather than an inline `as_secs_f64()` so the conversion cannot drift: OTEL semconv
/// requires seconds, and a duration recorded in millis would make every bucket boundary and
/// every `histogram_quantile` in a dashboard wrong by three orders of magnitude, silently.
pub fn secs(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64()
}

/// The pool identifier semconv requires on every `db.client.connection.*` metric. There is
/// exactly one pool in this process, and naming it keeps the series valid if that ever changes.
const POOL_NAME: &str = "sqlite";

/// Record the state of the SQLite connection pool.
///
/// Sampled by the periodic sampler rather than observed through a callback: an observable-gauge
/// callback runs on the metric reader's own thread, where there is no tokio runtime and no way
/// to `await` anything — fine for these three numbers, which are synchronous reads, but the
/// sampler needs to be async for the domain gauges anyway and one mechanism is better than two.
///
/// `used` is derived rather than read: sqlx exposes `size` (connections the pool holds) and
/// `num_idle` (those not currently checked out), so busy is the difference. Saturating, because
/// the two are read without a lock between them and can disagree by one under load.
pub fn record_pool(size: u32, idle: usize, max: u32) {
    let instruments = instruments();
    let idle = u64::try_from(idle).unwrap_or(0);
    let size = u64::from(size);
    let pool = KeyValue::new("db.client.connection.pool.name", POOL_NAME);
    instruments.db_pool_connections.record(
        idle.min(size),
        &[
            pool.clone(),
            KeyValue::new("db.client.connection.state", "idle"),
        ],
    );
    instruments.db_pool_connections.record(
        size.saturating_sub(idle),
        &[
            pool.clone(),
            KeyValue::new("db.client.connection.state", "used"),
        ],
    );
    instruments.db_pool_max.record(u64::from(max), &[pool]);
}

/// Times the enclosing scope and records the elapsed seconds into one histogram on drop.
///
/// The general form of [`ReportPhase`], for the use-cases that want one series rather than a
/// two-phase split. `Drop` rather than an explicit call for the same two reasons: it works
/// identically in `async` and non-`async` code, and an early `?` return is still timed — a
/// use-case that failed slowly is exactly the one worth seeing.
pub struct Timer {
    histogram: &'static Histogram<f64>,
    attributes: Vec<KeyValue>,
    started: std::time::Instant,
}

impl Timer {
    /// `histogram` is `&'static` because every instrument lives in the process-wide registry,
    /// so a call site never has to think about keeping one alive.
    pub fn new(histogram: &'static Histogram<f64>, attributes: Vec<KeyValue>) -> Self {
        Self {
            histogram,
            attributes,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        self.histogram
            .record(secs(self.started.elapsed()), &self.attributes);
    }
}

/// Times a report phase and records it when dropped.
///
/// A guard rather than a wrapper function because the two halves have different shapes — the
/// `*_inputs` loaders are `async` and the `*_from` calculators are not — and one line at the top
/// of a function reads better at eight call sites than eight closures. Recording on drop also
/// means a phase that returned `Err` is still timed, which is what a latency series should show:
/// a report that failed slowly is the interesting case.
///
/// The phase split is free here only because `sure_app::reports` already separates reading from
/// calculating so the calculation can go to the blocking pool. That seam is what makes "is this
/// report slow because of SQLite or because of the arithmetic?" answerable at all.
pub struct ReportPhase {
    report: &'static str,
    phase: &'static str,
    started: std::time::Instant,
}

impl ReportPhase {
    /// The `*_inputs` half: everything read out of the database.
    pub fn load(report: &'static str) -> Self {
        Self::new(report, "load")
    }

    /// The `*_from` half: pure computation over what was loaded.
    pub fn compute(report: &'static str) -> Self {
        Self::new(report, "compute")
    }

    fn new(report: &'static str, phase: &'static str) -> Self {
        Self {
            report,
            phase,
            started: std::time::Instant::now(),
        }
    }
}

impl Drop for ReportPhase {
    fn drop(&mut self) {
        instruments().report_duration.record(
            secs(self.started.elapsed()),
            &[
                KeyValue::new("report", self.report),
                KeyValue::new("phase", self.phase),
            ],
        );
    }
}

/// Adds `1` on construction and takes it away on drop.
///
/// For [`Instruments::http_active_requests`], where a matched pair of `add(1)`/`add(-1)` would
/// leak on any path that does not reach the second call — and there are two such paths here: a
/// panic inside a handler unwinds through the middleware (`CatchPanicLayer` sits *outside* it),
/// and a client that goes away has its response future dropped. Either one would ratchet the
/// gauge up permanently.
pub struct ActiveRequest;

impl ActiveRequest {
    pub fn enter() -> Self {
        instruments().http_active_requests.add(1, &[]);
        Self
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        instruments().http_active_requests.add(-1, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no provider installed the global meter is a no-op, and everything here has to keep
    /// working against it — that is the state of every test process and of any run with export
    /// switched off. A panic on this path would be a metric taking down the application.
    #[test]
    fn recording_against_the_no_op_meter_is_harmless() {
        let instruments = instruments();
        instruments.http_request_duration.record(0.25, &[]);
        instruments.db_busy_retries.add(1, &[]);
        instruments.db_pool_max.record(8, &[]);
        let guard = ActiveRequest::enter();
        drop(guard);
    }

    #[test]
    fn a_duration_is_converted_to_seconds() {
        assert!((secs(std::time::Duration::from_millis(1500)) - 1.5).abs() < f64::EPSILON);
    }
}
