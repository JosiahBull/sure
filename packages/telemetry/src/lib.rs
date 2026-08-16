//! Observability: OpenTelemetry traces, metrics and logs, pushed over OTLP.
//!
//! # Why this is its own crate, depending on nothing
//!
//! Every layer of the workspace records into it — `sure-dal` times a query, `sure-providers`
//! times an upstream call, `sure-api` times a request, `sure-scheduler` counts a job outcome
//! — and those sit at four different levels of the dependency graph. Anything this crate
//! depended on would be a layer that could never be instrumented. So, like `sure-appbase`,
//! it names no other workspace member.
//!
//! It is also the only crate that names `opentelemetry_sdk` or `opentelemetry-otlp`.
//! Everything else uses [`instruments`], which reaches the *global* meter through the thin
//! `opentelemetry` API crate. A call site therefore never mentions an exporter, a provider,
//! or a version — and when export is switched off, the global meter is a no-op and the call
//! costs an atomic load.
//!
//! # Export is off unless it is configured
//!
//! [`TelemetryConfig::endpoint`] is the master switch: with no endpoint, [`otel::init`]
//! builds no SDK, spawns no thread, and returns no layers. That is what keeps `pnpm dev`,
//! `cargo test` and both Playwright suites free of exporter threads and outbound sockets.
//!
//! # Two orderings that are load-bearing
//!
//! **Providers must be built after the Landlock sandbox.** In opentelemetry 0.32 the periodic
//! metric reader and the batch span/log processors each run on a plain OS thread, spawned when
//! the provider is *built*. `sure_server::sandbox::apply` refuses to run once the process has
//! more than one thread — `landlock_restrict_self(2)` only restricts the calling thread, so a
//! sibling that already exists would keep an unrestricted domain, and the check makes that a
//! startup failure rather than a silent hole. Build the providers in the gap between the
//! sandbox and the tokio runtime.
//!
//! **Nothing else will shut them down.** Those threads are not tokio tasks, so
//! `sure_appbase`'s drain cannot see them — which is the good half: they can never make a
//! shutdown look unclean. The other half is that [`otel::Guard::shutdown`] has to be called
//! explicitly, after `sure_appbase::run` returns, to flush the final batch.
//!
//! # The sandbox does not open a port for you
//!
//! `sure_server::sandbox` permits outbound TCP to 443 and 53 only, and it *deliberately* does
//! not derive extra ports from configuration — a policy nobody can predict from what they set
//! is not a policy. So a plaintext collector on, say, `:4318` needs its port listed in
//! `SURE_SANDBOX_CONNECT_PORTS`. A collector reached over `https://` on 443 needs nothing.
//! See `docs/OBSERVABILITY.md`.

pub mod config;
pub mod instruments;
pub mod max_level;
pub mod otel;
pub mod span_duration;

pub use config::{Signal, Signals, TelemetryConfig};
pub use instruments::{ActiveRequest, instruments, secs};
/// Re-exported so a call site can label a measurement without naming `opentelemetry` — and so
/// there is one place the version is chosen. Every recording crate depends on `sure-telemetry`
/// alone; only this crate has `opentelemetry` in its manifest.
pub use opentelemetry::KeyValue;

/// A `tracing` layer erased for storage in the reloadable list [`otel::init`] returns.
///
/// Typed against [`tracing_subscriber::Registry`] rather than the full layered stack, which
/// is what lets `sure-api` compose these without this crate knowing the shape of its
/// subscriber. It costs one thing, and the caller has to honour it: the reload layer holding
/// these must be added to the registry **first**, before the fmt layer, or `S` is no longer
/// `Registry` and the types will not line up.
pub type BoxedLayer =
    Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync>;
