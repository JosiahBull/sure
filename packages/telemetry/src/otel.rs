//! Building the SDK: three providers, an OTLP/HTTP exporter each, registered globally.
//!
//! See the crate docs for the two orderings this has to be called within — after the Landlock
//! sandbox (because building a provider spawns an OS thread) and before the process exits
//! (because nothing else will flush it).

use crate::BoxedLayer;
use crate::config::TelemetryConfig;
use crate::max_level::MaxLevel;
use anyhow::Context as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Compression, Protocol, WithExportConfig as _, WithHttpConfig as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, Temporality};
use opentelemetry_sdk::trace::SdkTracerProvider;

/// The `tracing` layers to install, and the handle that shuts the providers down.
pub struct Installed {
    pub layers: Vec<BoxedLayer>,
    pub guard: Guard,
}

/// Flushes and stops whatever [`init`] built.
///
/// Not a `Drop` impl. Shutdown has to happen at one specific point — after
/// `sure_appbase::run` has returned, on the main thread, with the runtime already gone — and a
/// value that flushed whenever it happened to be dropped would hide that from the reader of
/// `main`. It is also blocking, which is correct there and would not be anywhere else.
#[derive(Default)]
pub struct Guard {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
    logger: Option<SdkLoggerProvider>,
}

impl Guard {
    /// Export whatever is buffered and join the exporter threads.
    ///
    /// Failures are logged, never propagated: by the time this runs the application's own
    /// result is already decided, and losing the last batch of telemetry is not a reason to
    /// turn a clean run into a non-zero exit.
    pub fn shutdown(self) {
        // Logs first: the two shutdowns below can themselves emit records, and this is the
        // provider that would otherwise be asked to export them after being told to stop.
        if let Some(logger) = self.logger
            && let Err(err) = logger.shutdown()
        {
            tracing::warn!(error = %err, "could not flush the OTLP log exporter");
        }
        if let Some(tracer) = self.tracer
            && let Err(err) = tracer.shutdown()
        {
            tracing::warn!(error = %err, "could not flush the OTLP span exporter");
        }
        if let Some(meter) = self.meter
            && let Err(err) = meter.shutdown()
        {
            tracing::warn!(error = %err, "could not flush the OTLP metric exporter");
        }
    }
}

/// Build the providers `config` asks for, register them as the global ones, and hand back the
/// layers that bridge `tracing` into them.
///
/// With export switched off — no endpoint, or no signals — this builds nothing and returns an
/// empty [`Installed`]. That is the path every test and every plain `pnpm dev` takes: no
/// SDK, no threads, and a global meter that is a no-op.
pub fn init(config: &TelemetryConfig) -> anyhow::Result<Installed> {
    let Some(base) = config.endpoint.as_deref().filter(|_| config.signals.any()) else {
        return Ok(Installed {
            layers: Vec::new(),
            guard: Guard::default(),
        });
    };

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        // The workspace version, which every crate shares via `version.workspace = true`.
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();

    let mut layers: Vec<BoxedLayer> = Vec::new();
    let mut guard = Guard::default();

    if config.signals.traces {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(signal_url(base, "traces"))
            .with_protocol(Protocol::HttpBinary)
            .with_compression(Compression::Gzip)
            .build()
            .context("building the OTLP span exporter")?;
        let provider = SdkTracerProvider::builder()
            .with_resource(resource.clone())
            .with_batch_exporter(exporter)
            .build();
        let tracer = opentelemetry::trace::TracerProvider::tracer(&provider, "sure");
        opentelemetry::global::set_tracer_provider(provider.clone());
        // No `.with_filter(..)` here: `SURE_OTEL_FILTER` is applied once, to the reload slot
        // these layers are installed into, because a per-layer filter cannot survive being
        // swapped in later. See `MaxLevel`'s docs for the whole story.
        layers.push(Box::new(tracing_opentelemetry::layer().with_tracer(tracer)));
        guard.tracer = Some(provider);
    }

    if config.signals.metrics {
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(signal_url(base, "metrics"))
            .with_protocol(Protocol::HttpBinary)
            .with_compression(Compression::Gzip)
            // Cumulative is both the SDK default and what VictoriaMetrics wants; delta is
            // stored as-is but then needs `sum_over_time()`/`rate_over_sum()` to query and
            // must not be deduplicated or downsampled. Named rather than inherited so a
            // future change of default cannot quietly reshape every query built on it.
            .with_temporality(Temporality::Cumulative)
            .build()
            .context("building the OTLP metric exporter")?;
        let reader = PeriodicReader::builder(exporter)
            .with_interval(config.metrics_interval)
            .build();
        let provider = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_reader(reader)
            .build();
        opentelemetry::global::set_meter_provider(provider.clone());
        guard.meter = Some(provider);
        // Only worth installing alongside the meter: it turns `sure_dal` span closes into
        // `db.client.operation.duration`, so with no metrics provider it would time spans and
        // hand the numbers to a no-op instrument.
        layers.push(Box::new(crate::span_duration::DurationLayer::new()));
    }

    if config.signals.logs {
        let exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(signal_url(base, "logs"))
            .with_protocol(Protocol::HttpBinary)
            .with_compression(Compression::Gzip)
            .build()
            .context("building the OTLP log exporter")?;
        let provider = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build();
        // `MaxLevel`, not `.with_filter(..)`, for two reasons: a per-layer filter panics when
        // installed through a reload slot, and this is the one place the two signals need to
        // disagree about verbosity — see `TelemetryConfig::log_max_level`.
        layers.push(Box::new(MaxLevel::new(
            opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(&provider),
            config.log_max_level,
        )));
        guard.logger = Some(provider);
    }

    tracing::info!(
        endpoint = base,
        signals = config.signals.to_list(),
        metrics_interval_secs = config.metrics_interval.as_secs(),
        log_max_level = %config.log_max_level,
        service_name = config.service_name,
        "opentelemetry export enabled"
    );

    Ok(Installed { layers, guard })
}

/// The full URL for one signal.
///
/// Built here, not left to the exporter: `opentelemetry-otlp` appends `/v1/<signal>` only when
/// the endpoint came from `OTEL_EXPORTER_OTLP_ENDPOINT` itself, and uses a programmatically
/// supplied one verbatim (`resolve_http_endpoint`). Since the endpoint reaches us through
/// `Config` — so that an unusable URL is fatal at startup rather than at the first export —
/// appending is ours to do.
fn signal_url(base: &str, signal: &str) -> String {
    format!("{}/v1/{signal}", base.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Signals;

    #[test]
    fn a_signal_path_is_appended_to_the_base_exactly_once() {
        assert_eq!(
            signal_url("http://collector:4318", "metrics"),
            "http://collector:4318/v1/metrics"
        );
        // A trailing slash is what an operator copying a URL out of a browser leaves behind.
        assert_eq!(
            signal_url("http://collector:4318/", "traces"),
            "http://collector:4318/v1/traces"
        );
        // A base with a path prefix (an ingress routing /otel to a collector) keeps it.
        assert_eq!(
            signal_url("https://obs.example/otel", "logs"),
            "https://obs.example/otel/v1/logs"
        );
    }

    /// The off path must not construct an SDK, which is what keeps exporter threads out of
    /// every test process and out of `sandbox::apply`'s single-thread check.
    #[test]
    fn no_endpoint_builds_nothing() {
        let installed = init(&TelemetryConfig::default()).expect("the off path cannot fail");
        assert!(installed.layers.is_empty());
        installed.guard.shutdown();
    }

    /// An endpoint but no signals is also off. Worth pinning separately: it is the setting an
    /// operator reaches for to silence export without editing the endpoint out.
    #[test]
    fn an_endpoint_with_no_signals_builds_nothing() {
        let config = TelemetryConfig {
            endpoint: Some("http://127.0.0.1:4318".to_string()),
            signals: Signals::default(),
            ..TelemetryConfig::default()
        };
        assert!(!config.is_enabled());
        let installed = init(&config).expect("the off path cannot fail");
        assert!(installed.layers.is_empty());
        installed.guard.shutdown();
    }
}
