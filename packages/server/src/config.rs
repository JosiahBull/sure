use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sure_api::config::{ApiConfig, DEFAULT_CORS_ORIGINS, Limits};
use sure_appbase::LifecycleConfig;
use sure_mcp::config::{DEFAULT_MAX_ROWS, McpConfig, McpMode};
use sure_providers::Endpoint;

use crate::http::HttpConfig;
use crate::sandbox::{SandboxConfig, SandboxMode};

/// Runtime configuration, sourced from the environment with sensible local defaults.
///
/// This is the only place the environment is read, with one deliberate exception: the Akahu
/// token pair, which `serve` reads at the point it hands it to the adapter that carries it.
/// The two are secrets, and `Config` is `Clone + Debug` and travels the length of startup —
/// a value in here is a value that can end up in a `{config:?}`. Where those tokens are
/// *sent* is configuration and does live here ([`ProviderEndpoints`]); what they are is not.
///
/// `sure-api` defines the shape of its own tunables ([`ApiConfig`]) but parses nothing —
/// configuration is a concern of *running* the server, not of the routes.
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
    /// Where the three network-facing provider adapters point.
    pub provider_endpoints: ProviderEndpoints,
    /// The Landlock self-sandbox: how hard to insist on it, and anything to allow beyond
    /// what the server needs on its own.
    pub sandbox: SandboxConfig,
    /// How long each phase of shutdown gets. Distinct from
    /// [`HttpConfig::shutdown_grace`], which bounds only the connection drain *inside*
    /// the application future — these bound the sequence around it.
    pub lifecycle: LifecycleConfig,
    /// The MCP surface: off, read-only, or read-write. Off by default — see `docs/MCP.md`
    /// for why enabling it is a decision rather than a default.
    pub mcp: McpConfig,
}

/// The base URL of each provider adapter, already checked to be somewhere a credential may
/// travel (see [`sure_providers::Endpoint`]).
///
/// These are settings at all for one reason: `partly-proxy-lib` is a *reverse* proxy — one
/// listener per named upstream, forwarding to a fixed base URL — so the only way to put it in
/// front of an adapter is to tell the adapter a different URL. A `CONNECT` proxy honoured via
/// `HTTPS_PROXY` would have needed no configuration here at all; the library does not offer
/// one, and it would have meant a CA the process is made to trust, which is a wider hole than
/// three URLs that cannot name a plaintext host off this machine.
///
/// Nothing sets them in production, where each is its adapter's own `DEFAULT_BASE_URL`.
#[derive(Clone, Debug)]
pub struct ProviderEndpoints {
    /// `FRANKFURTER_BASE_URL` — exchange rates.
    pub frankfurter: Endpoint,
    /// `YAHOO_FINANCE_BASE_URL` — stock prices.
    pub yahoo_finance: Endpoint,
    /// `AKAHU_BASE_URL` — NZ bank feeds. The tokens that go with it are not part of `Config`;
    /// see the note on [`Config`].
    pub akahu: Endpoint,
}

/// Fold a `.env` file into the process environment, before anything reads it, and return
/// the file that was used.
///
/// Variables already present in the real environment always win: a `.env` is a
/// convenience for local runs, not an override of what a shell, a container, or the test
/// harness deliberately set. Nothing here is required — with no file, every value falls
/// back to the defaults below.
///
/// `SURE_ENV_FILE` decides where it comes from:
/// - **unset** — search the working directory and its parents for `.env`, and do nothing
///   if there isn't one. Walking up is what lets `pnpm dev` find the repo-root file from
///   whichever package directory a command happens to run in.
/// - **a path** — load exactly that file. Being explicit and wrong should be loud, so a
///   missing file is an error here where an absent `.env` is not.
/// - **empty** — skip entirely. For callers that need a hermetic environment; the API e2e
///   suite sets this so it can assert on the errors an *unconfigured* provider returns.
///
/// Unlike an unparseable *value* (which warns and falls back to the default), an
/// unparseable *file* stops startup: it loads line by line, so continuing would run with
/// half the file applied and no way to tell which half.
pub fn load_dotenv() -> anyhow::Result<Option<PathBuf>> {
    match std::env::var("SURE_ENV_FILE") {
        Ok(path) if path.trim().is_empty() => Ok(None),
        Ok(path) => {
            dotenvy::from_path(&path)
                .map_err(|err| anyhow::anyhow!("SURE_ENV_FILE={path}: {err}"))?;
            Ok(Some(PathBuf::from(path)))
        }
        Err(_) => match dotenvy::dotenv() {
            Ok(path) => Ok(Some(path)),
            Err(err) if err.not_found() => Ok(None),
            Err(err) => Err(anyhow::anyhow!("failed to load .env: {err}")),
        },
    }
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

        let mcp = McpConfig {
            ceiling: mcp_ceiling()?,
            max_rows: parsed("SURE_MCP_MAX_ROWS", DEFAULT_MAX_ROWS),
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

        let sandbox = SandboxConfig {
            mode: parsed("SURE_SANDBOX", SandboxMode::default()),
            read_paths: paths("SURE_SANDBOX_READ_PATHS"),
            write_paths: paths("SURE_SANDBOX_WRITE_PATHS"),
            connect_ports: ports("SURE_SANDBOX_CONNECT_PORTS"),
        };

        // Each default is the const in the adapter's own module rather than the URL retyped
        // here: one host, one spelling. A copy in this file is the one nobody would think to
        // change when an upstream moves, and it would be wrong in the direction that is hard
        // to notice — every request still succeeding, against the old host.
        let provider_endpoints = ProviderEndpoints {
            frankfurter: endpoint(
                "FRANKFURTER_BASE_URL",
                sure_providers::frankfurter::DEFAULT_BASE_URL,
            )?,
            yahoo_finance: endpoint(
                "YAHOO_FINANCE_BASE_URL",
                sure_providers::yahoo_finance::DEFAULT_BASE_URL,
            )?,
            akahu: endpoint("AKAHU_BASE_URL", sure_providers::akahu::DEFAULT_BASE_URL)?,
        };

        let lifecycle_defaults = LifecycleConfig::default();
        let lifecycle = LifecycleConfig {
            // Zero by default. Nothing routes to this process — it is a single binary a
            // person runs — so there is no endpoint slice to wait for, and a delay would
            // be pure latency between Ctrl-C and the prompt coming back.
            predrain_delay: secs("SHUTDOWN_PREDRAIN_SECS", lifecycle_defaults.predrain_delay),
            // Has to cover `serve`'s whole teardown: the HTTP drain (`SHUTDOWN_GRACE_SECS`,
            // 15s), then the background-task drain, then closing the pool. Raising that
            // one without raising this just moves where the deadline bites.
            app_grace: secs("SHUTDOWN_APP_GRACE_SECS", lifecycle_defaults.app_grace),
            drain_grace: secs("SHUTDOWN_DRAIN_GRACE_SECS", lifecycle_defaults.drain_grace),
            blocking_grace: secs(
                "SHUTDOWN_BLOCKING_GRACE_SECS",
                lifecycle_defaults.blocking_grace,
            ),
        };

        Ok(Self {
            database_url,
            bind_addr,
            web_dir,
            api,
            http,
            background_tasks: flag("BACKGROUND_TASKS", true),
            provider_endpoints,
            sandbox,
            lifecycle,
            mcp,
        })
    }
}

/// `SURE_MCP`, or [`McpMode::Off`] when unset — the **ceiling**, not the working mode.
///
/// What is served is this clamped against `settings.mcp_mode`, which the household sets in
/// the app. Unset therefore means the endpoint is absent no matter what the app stores:
/// turning agent access on requires someone with access to the host, and the toggle in the
/// app can only choose within what the host already allowed.
///
/// The one env var in this file that does **not** go through [`parsed`], for the same
/// reason [`endpoint`] doesn't: falling back on a typo would answer a question about access
/// with a guess. `SURE_MCP=wrtie` silently serving nothing is a confusing afternoon;
/// a value that fell back the *other* way would be an agent with write access to the
/// household ledger that nobody asked for. Neither is a warning line's worth of risk.
fn mcp_ceiling() -> anyhow::Result<McpMode> {
    match std::env::var("SURE_MCP") {
        Err(_) => Ok(McpMode::Off),
        Ok(raw) => raw.parse().map_err(|e: String| anyhow::anyhow!(e)),
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

/// Read `name` as a provider base URL, falling back to the adapter's own production const
/// when it is unset.
///
/// The one value in this file where a bad setting is **fatal**, and a deliberate break with
/// [`parsed`] two functions up. Warn-and-continue is right for a limit: the wrong number is
/// still the right behaviour aimed at the right place, and refusing to boot over a mistyped
/// byte ceiling is worse than running the default. For an endpoint the same fallback means
/// this process sends its requests — and, for Akahu, an app token — to a *different host*
/// than the operator named, while the warning explaining that scrolls past in the log. There
/// is no useful default for "somewhere else": either the URL is usable or the operator has to
/// see it. [`load_dotenv`] already draws the line here for `SURE_ENV_FILE`, on the same
/// reasoning — being explicit and wrong should be loud.
///
/// What counts as usable is [`Endpoint`]'s judgement, not this function's: `https://`
/// anywhere, `http://` only to this machine. That check lives on the type precisely so it
/// cannot be relaxed by anything set in the same file as the URL it is protecting.
fn endpoint(name: &str, default: &str) -> anyhow::Result<Endpoint> {
    let configured = std::env::var(name).ok();
    resolve_endpoint(name, configured.as_deref(), default)
}

/// The decision [`endpoint`] makes, given what the environment said.
///
/// Split from it so that decision — the fatal-vs-fallback one, which is the whole reason this
/// value doesn't go through [`parsed`] — is testable without `set_var`, which mutates the
/// process every other test in this binary is also reading.
fn resolve_endpoint(
    name: &str,
    configured: Option<&str>,
    default: &str,
) -> anyhow::Result<Endpoint> {
    let raw = configured
        // Trimmed before it is parsed, not after: `Endpoint` deliberately stores the string it
        // was given verbatim (the adapters append `/latest?…` to it), and a URL parser treats
        // surrounding whitespace as insignificant — so an `.env` line with a trailing space
        // would parse happily and then build `https://api.frankfurter.dev/v1 /latest`.
        .map(str::trim)
        // Set-but-empty reads as unset, as `WEB_DIR` does above: a blank line in a `.env` is
        // how that file spells "no value", and it is the one unusable setting that says nothing
        // about where the operator wanted to point — so there is nothing to be loud about.
        .filter(|raw| !raw.is_empty())
        .unwrap_or(default);
    // The context names the variable, because that is what the operator edits;
    // `Endpoint::parse`'s own message quotes the URL and says what is wrong with it.
    Endpoint::parse(raw).with_context(|| format!("{name}={raw}"))
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

/// Read `name` as a `:`-separated path list — the separator `PATH` and Landlock's own
/// sandboxer use, and the one a path can't contain by accident the way it can a comma.
fn paths(name: &str) -> Vec<PathBuf> {
    let Some(raw) = std::env::var_os(name) else {
        return Vec::new();
    };
    std::env::split_paths(&raw)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Read `name` as a comma-separated port list, dropping (loudly) anything that isn't a
/// port. Unlike a limit, a bad entry here can only ever *narrow* what the sandbox allows.
fn ports(name: &str) -> Vec<u16> {
    match std::env::var(name) {
        Err(_) => Vec::new(),
        Ok(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| match s.parse() {
                Ok(port) => Some(port),
                Err(_) => {
                    tracing::warn!(env = name, value = s, "not a port number; ignoring it");
                    None
                }
            })
            .collect(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every built-in default, i.e. the configuration of a fresh checkout.
    const DEFAULTS: [&str; 3] = [
        sure_providers::frankfurter::DEFAULT_BASE_URL,
        sure_providers::yahoo_finance::DEFAULT_BASE_URL,
        sure_providers::akahu::DEFAULT_BASE_URL,
    ];

    /// With nothing set, `from_env` parses three consts, and an unusable one is fatal — so this
    /// test is the difference between an empty environment booting and not booting.
    ///
    /// It is also the only cover any of the three has, and structurally so: `sure-providers` has
    /// no argument-free constructor left, so nothing in the workspace parses one of these consts
    /// except the line above. A default that stopped being `https://` — or stopped being a URL —
    /// would take down every boot, and this is the test that says so before a boot does.
    #[test]
    fn every_built_in_provider_default_reaches_the_real_api_over_tls() {
        for default in DEFAULTS {
            let endpoint = resolve_endpoint("UNSET_IN_THIS_TEST", None, default)
                .unwrap_or_else(|err| panic!("{default}: {err:#}"));
            assert_eq!(endpoint, Endpoint::Secure(default.to_string()));
        }
    }

    /// The departure from [`parsed`], which the rest of this file follows. Route these through
    /// it "for consistency" later and a run that mistypes `AKAHU_BASE_URL` — a test aiming at
    /// its proxy, say — warns once and then sends real credentials to the live api.akahu.io,
    /// which is the outcome the whole record/replay arrangement exists to make impossible.
    #[test]
    fn an_unusable_endpoint_stops_startup_and_names_both_the_variable_and_the_url() {
        for bad in [
            // Plaintext off this machine — `Endpoint`'s reason for existing.
            "http://evil.example/v1",
            "not-a-url",
            "ftp://api.akahu.io/v1",
        ] {
            let err = resolve_endpoint(
                "AKAHU_BASE_URL",
                Some(bad),
                sure_providers::akahu::DEFAULT_BASE_URL,
            )
            .expect_err("an unusable endpoint must not fall back to the default host");
            // Alternate Display, not `to_string()`: the variable is this file's context and the
            // diagnosis is `Endpoint::parse`'s cause, and only `{:#}` renders both — which is
            // also how `main` returning `anyhow::Result` prints it.
            let rendered = format!("{err:#}");
            assert!(rendered.contains("AKAHU_BASE_URL"), "{rendered}");
            assert!(rendered.contains(bad), "{rendered}");
        }
    }

    /// The two shapes a `.env` produces that are not really values: a variable listed with
    /// nothing after the `=`, and one with a stray trailing space. Neither may reach
    /// `Endpoint`, which keeps the string it is handed verbatim — " …/v1 " parses as a URL and
    /// then concatenates into a request path with a space in the middle of it.
    #[test]
    fn a_blank_setting_defaults_and_a_padded_one_is_trimmed() {
        let default = sure_providers::frankfurter::DEFAULT_BASE_URL;
        for blank in [Some(""), Some("   "), None] {
            let endpoint = resolve_endpoint("FRANKFURTER_BASE_URL", blank, default)
                .unwrap_or_else(|err| panic!("{blank:?}: {err:#}"));
            assert_eq!(endpoint.url(), default, "{blank:?}");
        }

        let padded = resolve_endpoint(
            "FRANKFURTER_BASE_URL",
            Some("  http://127.0.0.1:53219/v1\n"),
            default,
        )
        .expect("a padded loopback URL is still a loopback URL");
        assert_eq!(padded.url(), "http://127.0.0.1:53219/v1");
    }
}
