//! The shape of the telemetry tunables. Nothing here reads the environment — that is
//! `sure_server::config`'s job, in the same division of labour `sure_api::config` already
//! follows.

use std::str::FromStr;
use std::time::Duration;

/// One OpenTelemetry signal.
///
/// A closed set, so an enum rather than a string (CLAUDE.md rule 1). Text at exactly one
/// edge: [`Signals::from_str`], parsing `SURE_OTEL_SIGNALS`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Traces,
    Metrics,
    Logs,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Traces => "traces",
            Signal::Metrics => "metrics",
            Signal::Logs => "logs",
        }
    }
}

impl FromStr for Signal {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "traces" | "trace" | "tracing" => Ok(Signal::Traces),
            "metrics" | "metric" => Ok(Signal::Metrics),
            "logs" | "log" | "logging" => Ok(Signal::Logs),
            other => Err(format!("unknown telemetry signal {other:?}")),
        }
    }
}

/// Which signals are switched on.
///
/// Three bools rather than a `Vec<Signal>` or a `HashSet`: it is `Copy`, the "is this on?"
/// question is a field read, and [`Signals::enable`] is an exhaustive match, so adding a
/// fourth signal is a compile error here instead of a variant that silently never enables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Signals {
    pub traces: bool,
    pub metrics: bool,
    pub logs: bool,
}

impl Signals {
    /// Traces and metrics, but **not** logs — the default, and a deliberate one.
    ///
    /// Traces and metrics carry labels drawn from closed sets and route *templates*
    /// (`/api/accounts/{id}`, never the id). Logs carry free text, and in this application
    /// that text is financial: `sure_dal::connect` sets sqlx's `log_statements` to TRACE, so
    /// at that level the stream contains SQL with its bound parameters — account numbers,
    /// payees, amounts — and every handler's `#[instrument(err)]` renders an error's
    /// `Display`, which can include whatever was submitted.
    ///
    /// CLAUDE.md rule 3 is about identifiers not leaving the repository; this is the same
    /// question about the running process. Shipping logs off the machine stays a decision
    /// someone makes (`SURE_OTEL_SIGNALS=traces,metrics,logs`), not a default they inherit.
    pub fn default_set() -> Self {
        Self {
            traces: true,
            metrics: true,
            logs: false,
        }
    }

    /// Whether any signal at all is on. When nothing is, `otel::init` builds no SDK even if
    /// an endpoint was given.
    pub fn any(self) -> bool {
        self.traces || self.metrics || self.logs
    }

    pub fn enable(&mut self, signal: Signal) {
        match signal {
            Signal::Traces => self.traces = true,
            Signal::Metrics => self.metrics = true,
            Signal::Logs => self.logs = true,
        }
    }

    /// Render back to the `SURE_OTEL_SIGNALS` spelling, for the startup log line.
    pub fn to_list(self) -> String {
        let mut on = Vec::new();
        for (enabled, signal) in [
            (self.traces, Signal::Traces),
            (self.metrics, Signal::Metrics),
            (self.logs, Signal::Logs),
        ] {
            if enabled {
                on.push(signal.as_str());
            }
        }
        if on.is_empty() {
            "none".to_string()
        } else {
            on.join(",")
        }
    }
}

impl FromStr for Signals {
    type Err = String;

    /// A comma-separated list. An unrecognised entry is an error rather than a warning: the
    /// same reasoning `SURE_MCP` follows in `sure_server::config` — `SURE_OTEL_SIGNALS=metircs`
    /// silently exporting nothing is a confusing afternoon spent on a dashboard that was never
    /// going to fill in. `off` and `none` spell "no signals" explicitly.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "" | "off" | "none" | "false" | "0"
        ) {
            return Ok(Signals::default());
        }
        let mut signals = Signals::default();
        for part in trimmed.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            signals.enable(part.parse()?);
        }
        Ok(signals)
    }
}

/// Everything `otel::init` needs, already validated.
#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` — the **base** URL of the collector, e.g.
    /// `http://otel-collector:4318`, with no signal path. `None` switches export off
    /// entirely: no SDK, no exporter threads, no outbound sockets.
    ///
    /// The per-signal path is appended by [`crate::otel`] rather than by the exporter.
    /// That is not a stylistic choice — `opentelemetry-otlp`'s `resolve_http_endpoint` uses a
    /// *programmatically* supplied endpoint verbatim and only appends `/v1/<signal>` when the
    /// value came from the environment variable itself. Passing a bare base URL through
    /// `with_endpoint` would POST every signal to the collector's root.
    pub endpoint: Option<String>,
    /// `SURE_OTEL_SIGNALS`. Defaults to [`Signals::default_set`].
    pub signals: Signals,
    /// `SURE_OTEL_METRICS_INTERVAL_SECS` — how often the periodic reader exports.
    pub metrics_interval: Duration,
    /// `SURE_OTEL_SAMPLE_INTERVAL_SECS` — how often the domain-gauge sampler queries the
    /// database. Longer than the export interval on purpose: several of these gauges read
    /// real work, and a gauge the reader re-exports unchanged is not a stale gauge.
    pub sample_interval: Duration,
    /// `OTEL_SERVICE_NAME`, or `sure`. Becomes the `service.name` resource attribute.
    pub service_name: String,
    /// `SURE_OTEL_LOG_LEVEL` — the ceiling on how verbose an *exported log record* may be,
    /// applied on top of `SURE_OTEL_FILTER` and only to the log bridge.
    ///
    /// It exists because the two signals want different verbosities out of one filter. Traces
    /// want our crates at `debug`, so that handler and DAL spans nest under the request span —
    /// that nesting is most of why traces are worth exporting. But at `debug` those same
    /// `#[instrument]` attributes also emit their `ret(DEBUG)` **events**, whose fields are
    /// the values being returned: account rows, transaction rows. As spans that is structure;
    /// as exported log records it is the ledger. `info` keeps them out.
    pub log_max_level: tracing::level_filters::LevelFilter,
}

/// `EnvFilter` directives for the OTLP layers when `SURE_OTEL_FILTER` is unset.
///
/// Read by `sure_api::telemetry::init_tracing`, not from here — it is a sibling of `RUST_LOG`,
/// which that function already reads directly, and it has to be known before `Config` exists
/// because per-layer filters are fixed when the subscriber is built (see
/// [`crate::max_level::MaxLevel`]).
///
/// Our own crates at `debug` for the span nesting. Two entries are not optional:
///
/// * `sqlx=off` — sqlx emits one event per statement, and `sure_dal::connect` configures those
///   at TRACE *with their bound parameters*. See [`Signals::default_set`].
/// * `opentelemetry=off` — the SDK reports its own export failures through `tracing`, and a
///   layer feeding those back into the exporter is a feedback loop that gets louder the worse
///   the collector is doing.
pub const DEFAULT_FILTER: &str = "info,\
    sure_api=debug,\
    sure_app=debug,\
    sure_dal=debug,\
    sure_mcp=debug,\
    sure_providers=debug,\
    sure_scheduler=debug,\
    sqlx=off,\
    opentelemetry=off";

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            signals: Signals::default_set(),
            metrics_interval: Duration::from_secs(60),
            sample_interval: Duration::from_secs(300),
            service_name: "sure".to_string(),
            log_max_level: tracing::level_filters::LevelFilter::INFO,
        }
    }
}

impl TelemetryConfig {
    /// Whether [`crate::otel::init`] will build anything at all.
    pub fn is_enabled(&self) -> bool {
        self.endpoint.is_some() && self.signals.any()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_round_trips_through_its_text_form() {
        for signal in [Signal::Traces, Signal::Metrics, Signal::Logs] {
            assert_eq!(signal.as_str().parse::<Signal>(), Ok(signal));
        }
    }

    #[test]
    fn a_list_enables_exactly_what_it_names() {
        let signals: Signals = "traces,metrics".parse().unwrap();
        assert_eq!(
            signals,
            Signals {
                traces: true,
                metrics: true,
                logs: false
            }
        );
        assert_eq!(signals.to_list(), "traces,metrics");
    }

    #[test]
    fn spacing_repetition_and_case_are_all_tolerated() {
        let signals: Signals = " Logs , logs,  TRACES ".parse().unwrap();
        assert_eq!(
            signals,
            Signals {
                traces: true,
                metrics: false,
                logs: true
            }
        );
    }

    /// The departure from `sure_server::config::parsed`, which warns and falls back. A typo
    /// here would leave a dashboard permanently empty with one warning line to explain it.
    #[test]
    fn an_unknown_signal_is_rejected_rather_than_ignored() {
        let err = "traces,metircs".parse::<Signals>().unwrap_err();
        assert!(err.contains("metircs"), "{err}");
    }

    #[test]
    fn off_is_spelled_explicitly_rather_than_by_an_empty_list() {
        for off in ["off", "none", "", "  "] {
            let signals: Signals = off.parse().unwrap();
            assert!(!signals.any(), "{off:?}");
            assert_eq!(signals.to_list(), "none");
        }
    }

    /// Logs are implemented but not on by default. If this ever flips, it should be because
    /// someone decided to — see `Signals::default_set` for what is at stake.
    #[test]
    fn logs_are_not_exported_by_default() {
        let default = TelemetryConfig::default();
        assert!(default.signals.traces);
        assert!(default.signals.metrics);
        assert!(!default.signals.logs);
    }

    /// The master switch. Signals on but no endpoint must still build nothing.
    #[test]
    fn nothing_is_enabled_without_an_endpoint() {
        assert!(!TelemetryConfig::default().is_enabled());
        let configured = TelemetryConfig {
            endpoint: Some("http://127.0.0.1:4318".to_string()),
            ..TelemetryConfig::default()
        };
        assert!(configured.is_enabled());
        let silenced = TelemetryConfig {
            signals: Signals::default(),
            ..configured
        };
        assert!(!silenced.is_enabled());
    }

    /// The default filter must keep sqlx and the SDK's own diagnostics out — the first because
    /// those events carry bound parameter values, the second because it is a feedback loop.
    #[test]
    fn the_default_filter_excludes_sqlx_and_the_sdk_itself() {
        assert!(DEFAULT_FILTER.contains("sqlx=off"), "{DEFAULT_FILTER}");
        assert!(
            DEFAULT_FILTER.contains("opentelemetry=off"),
            "{DEFAULT_FILTER}"
        );
    }

    /// The filter runs our crates at `debug` so spans nest; the log ceiling is what stops that
    /// also exporting every `ret(DEBUG)` event, whose fields are ledger rows. If these two ever
    /// agree, enabling logs starts shipping the database.
    #[test]
    fn the_log_ceiling_is_stricter_than_the_span_filter() {
        assert!(DEFAULT_FILTER.contains("=debug"));
        assert_eq!(
            TelemetryConfig::default().log_max_level,
            tracing::level_filters::LevelFilter::INFO
        );
    }
}
