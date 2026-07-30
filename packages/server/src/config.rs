use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use sure_api::config::{ApiConfig, Limits, DEFAULT_CORS_ORIGINS};

use crate::http::HttpConfig;

/// Runtime configuration, sourced from the environment with sensible local defaults.
///
/// This is the only place the environment is read. `sure-api` defines the shape of its
/// own tunables ([`ApiConfig`]) but parses nothing — configuration is a concern of
/// *running* the server, not of the routes.
#[derive(Clone, Debug)]
pub struct Config {
    /// sqlx connection string, e.g. `sqlite:data/sure.db` or `sqlite::memory:`.
    pub database_url: String,
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Optional directory containing the built SPA. When set, the server serves it
    /// with SPA fallback so the whole app runs from a single binary in production.
    pub web_dir: Option<String>,
    /// Caching, compression, CORS, and the request-level abuse guards.
    pub api: ApiConfig,
    /// Connection-level abuse guards and the shutdown grace period.
    pub http: HttpConfig,
    /// Whether the background scheduler (exchange rates, provider polling, stock prices,
    /// transfer linking) runs at all. On outside tests — see `serve` for why the e2e suite
    /// turns it off.
    pub background_tasks: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:data/sure.db".to_string());
        let bind_addr = std::env::var("BIND_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
            .parse()?;
        let web_dir = std::env::var("WEB_DIR").ok().filter(|s| !s.is_empty());

        let defaults = Limits::default();
        let limits = Limits {
            max_body_bytes: parsed("MAX_BODY_BYTES", defaults.max_body_bytes),
            max_snapshot_body_bytes: parsed(
                "MAX_SNAPSHOT_BODY_BYTES",
                defaults.max_snapshot_body_bytes,
            ),
            max_import_body_bytes: parsed("MAX_IMPORT_BODY_BYTES", defaults.max_import_body_bytes),
            request_timeout: secs("REQUEST_TIMEOUT_SECS", defaults.request_timeout),
            long_request_timeout: secs("LONG_REQUEST_TIMEOUT_SECS", defaults.long_request_timeout),
            max_in_flight: parsed("MAX_IN_FLIGHT", defaults.max_in_flight),
            rate_limit_rps: parsed("RATE_LIMIT_RPS", defaults.rate_limit_rps),
            rate_limit_burst: parsed("RATE_LIMIT_BURST", defaults.rate_limit_burst),
            rate_limit_exempt_loopback: flag(
                "RATE_LIMIT_EXEMPT_LOOPBACK",
                defaults.rate_limit_exempt_loopback,
            ),
            max_etag_body_bytes: parsed("MAX_ETAG_BODY_BYTES", defaults.max_etag_body_bytes),
        };

        let api = ApiConfig {
            limits,
            cors_allowed_origins: cors_origins(),
            cdn_cache_headers: flag("CDN_CACHE_HEADERS", true),
            trust_proxy_headers: flag("TRUST_PROXY_HEADERS", false),
            compression: flag("COMPRESSION", true),
        };

        let http_defaults = HttpConfig::default();
        let http = HttpConfig {
            max_connections: parsed("MAX_CONNECTIONS", http_defaults.max_connections),
            header_read_timeout: secs(
                "HEADER_READ_TIMEOUT_SECS",
                http_defaults.header_read_timeout,
            ),
            http1_max_buf_size: parsed("HTTP1_MAX_BUF_BYTES", http_defaults.http1_max_buf_size),
            h2_max_concurrent_streams: parsed(
                "H2_MAX_CONCURRENT_STREAMS",
                http_defaults.h2_max_concurrent_streams,
            ),
            h2_keep_alive_interval: http_defaults.h2_keep_alive_interval,
            h2_keep_alive_timeout: http_defaults.h2_keep_alive_timeout,
            shutdown_grace: secs("SHUTDOWN_GRACE_SECS", http_defaults.shutdown_grace),
        };

        Ok(Self {
            database_url,
            bind_addr,
            web_dir,
            api,
            http,
            background_tasks: flag("BACKGROUND_TASKS", true),
        })
    }
}

/// Read `name` as `T`, falling back to `default` when unset — and, loudly, when set to
/// something unparseable. A typo in an env var should not silently disable a limit, but
/// it also should not stop the server from starting.
fn parsed<T: FromStr>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Err(_) => default,
        Ok(raw) => match raw.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(env = name, value = %raw, "unparseable value; using the default");
                default
            }
        },
    }
}

fn secs(name: &str, default: Duration) -> Duration {
    Duration::from_secs(parsed(name, default.as_secs()))
}

/// Accepts the spellings people actually type.
fn flag(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Err(_) => default,
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            other => {
                tracing::warn!(
                    env = name,
                    value = other,
                    "unrecognised flag; using the default"
                );
                default
            }
        },
    }
}

/// `CORS_ALLOWED_ORIGINS` as a comma-separated list. Explicitly set but empty disables
/// CORS altogether — the right setting for the single-binary deployment, where the SPA is
/// same-origin and nothing legitimately calls the API from elsewhere.
fn cors_origins() -> Vec<String> {
    match std::env::var("CORS_ALLOWED_ORIGINS") {
        Err(_) => DEFAULT_CORS_ORIGINS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        Ok(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}
