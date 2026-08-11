//! The out-of-process host: one proxy cluster, configured from the environment, announced on
//! stdout, and torn down when whoever spawned it goes away.
//!
//! Spawned once per Playwright run (`@sure/api-tests`, `@sure/web`) alongside the real
//! `sure-api` binary. The suite reads the handshake line this prints, hands the `env` map
//! straight to `sure-api`, and thereafter drives the cluster over the TCP JSON-Lines control
//! plane (`SPECIFICATION.md` §12.2) — stubs, traffic queries, blocking assertions. In-process
//! Rust tests never run this binary; they call [`sure_testproxy::start`] directly.
//!
//! Environment:
//!
//! | var | value | default |
//! |---|---|---|
//! | `SURE_TESTPROXY_MODE` | `record` \| `replay` | `replay` |
//! | `SURE_TESTPROXY_SNAPSHOT_DIR` | directory of `<upstream>.ndjson` files | none — stubs only |
//! | `SURE_TESTPROXY_CONTROL_BIND` | `SocketAddr` for the control plane | `127.0.0.1:0` |
//! | `RUST_LOG` | tracing filter, written to **stderr** | `warn` |
//!
//! Every default is the one that cannot reach the internet, and `record` without a snapshot
//! directory persists nothing — see [`sure_testproxy::ClusterConfig`].

use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use partly_proxy_lib::Mode;
use sure_testproxy::{ClusterConfig, Started, Upstream, start};
use tokio::io::AsyncReadExt;

const MODE_ENV: &str = "SURE_TESTPROXY_MODE";
const SNAPSHOT_DIR_ENV: &str = "SURE_TESTPROXY_SNAPSHOT_DIR";
const CONTROL_BIND_ENV: &str = "SURE_TESTPROXY_CONTROL_BIND";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    // Every fallback comes from `ClusterConfig::default()` rather than being restated here, so
    // the reasoning for "why replay, why loopback, why port zero" lives in exactly one place.
    let defaults = ClusterConfig::default();
    let config = ClusterConfig {
        mode: mode_from_env(defaults.mode)?,
        snapshot_dir: snapshot_dir_from_env(),
        control_bind: match std::env::var(CONTROL_BIND_ENV) {
            Ok(raw) => raw.trim().parse().with_context(|| {
                format!("{CONTROL_BIND_ENV}={raw:?} is not a socket address, e.g. 127.0.0.1:0")
            })?,
            Err(_) => defaults.control_bind,
        },
    };

    let started = start(&config).await?;
    announce(&started, config.mode)?;

    let stop = wait_for_stop().await;
    tracing::info!(reason = stop.as_str(), "shutting down");
    // Flushes each NDJSON backend before returning, so a recording pass that ends here has a
    // complete file rather than one missing its last exchange.
    let outcome = started.cluster.shutdown().await;
    let code = match &outcome {
        Ok(()) => 0,
        Err(err) => {
            tracing::error!(%err, "cluster shutdown reported a problem");
            1
        }
    };

    // Exit rather than return. `tokio::io::stdin` does its reads on a blocking thread, and on
    // the ctrl-c path one of those reads is still parked there; dropping the runtime waits for
    // blocking tasks, so returning normally would hang until someone closed stdin. Everything
    // with anything to flush has already been awaited above, which is what makes skipping the
    // remaining destructors safe here and not a shortcut.
    std::process::exit(code);
}

/// Logs go to **stderr**, unconditionally: stdout carries the handshake line and nothing else,
/// or the harness's `JSON.parse` fails on a log record it never asked for.
///
/// `warn` by default so a passing run is silent and a failing one is not — the replay-miss line
/// in [`sure_testproxy::start`] is WARN precisely so it survives this filter.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Read [`MODE_ENV`], refusing anything that is not one of the two spellings.
///
/// `partly_proxy_lib::Mode` has its own `FromStr` accepting the same two, but its `Default` is
/// `Mode::Record` — so the safe fallback has to be stated on this side regardless, and stating
/// both in one match keeps "what is accepted" next to "what happens when nothing is set".
///
/// A hard error rather than the warn-and-default that `sure-server` uses for its limits
/// (`packages/server/src/config.rs`), because the two failures point in opposite directions.
/// Defaulting a limit costs a weaker guard on a running server; defaulting *this* would let
/// `SURE_TESTPROXY_MODE=repaly` in CI select `record`, and a record-mode run reaches the real
/// Akahu with real credentials and reports it as a passing test. The asymmetry is the point.
fn mode_from_env(default: Mode) -> anyhow::Result<Mode> {
    let Ok(raw) = std::env::var(MODE_ENV) else {
        return Ok(default);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        // An unset variable expanded by a shell arrives as the empty string, and the reading
        // that cannot reach the internet is the one to take.
        "" => Ok(default),
        "record" => Ok(Mode::Record),
        "replay" => Ok(Mode::Replay),
        // Wildcard over arbitrary text out of the environment, not over one of our enums — and
        // it rejects rather than defaults, which is the half of rule 2 that matters here.
        other => {
            anyhow::bail!("{MODE_ENV}={other:?} is not a mode: expected \"record\" or \"replay\"")
        }
    }
}

/// Read [`SNAPSHOT_DIR_ENV`], treating explicitly-empty as unset — the same reading
/// `sure-server` gives `WEB_DIR`, and the one that lets a harness clear an inherited value
/// without unsetting it.
fn snapshot_dir_from_env() -> Option<PathBuf> {
    std::env::var(SNAPSHOT_DIR_ENV)
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from)
}

/// Print the one line of JSON the spawning harness reads, then flush it.
///
/// ```text
/// {"control":"127.0.0.1:54321",
///  "env":{"FRANKFURTER_BASE_URL":"http://127.0.0.1:54322/v1", ...},
///  "mode":"replay",
///  "upstreams":{"frankfurter":"127.0.0.1:54322", ...}}
/// ```
///
/// Keys come out sorted, because `serde_json`'s map is a `BTreeMap` — which matters only to
/// something trying to match this line as text instead of parsing it.
///
/// `env` is the handshake proper: the harness passes it straight to `sure-api` as environment,
/// so no TypeScript ever has to know that Yahoo's charts sit under `/v8/finance/chart` or that
/// a path prefix exists at all. `upstreams` is the same set of listeners without the prefixes,
/// for a test that wants to talk to one directly. `control` is where the JSON-Lines commands
/// go, and it is reported rather than assumed because the default bind is port zero.
///
/// One line, because a harness that reads a line and parses it is the simplest thing that can
/// work across a process boundary, and because everything else this process says goes to stderr.
fn announce(started: &Started, mode: Mode) -> anyhow::Result<()> {
    let mut upstreams: BTreeMap<&str, String> = BTreeMap::new();
    for upstream in Upstream::ALL {
        let addr = started
            .cluster
            .addr(upstream.name())
            .with_context(|| format!("upstream {} bound no listener", upstream.name()))?;
        upstreams.insert(upstream.name(), addr.to_string());
    }

    let handshake = serde_json::json!({
        "mode": mode_name(mode),
        "control": started.control_addr.to_string(),
        "upstreams": upstreams,
        "env": started.endpoints,
    });
    let line = serde_json::to_string(&handshake).context("serialise the handshake")?;

    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").context("write the handshake to stdout")?;
    // `Stdout` is line-buffered, so the newline above should be enough on its own. "Should be
    // enough" is not worth betting a hung suite on: the harness blocks reading this line, and
    // this process then blocks until the harness gives up and kills it.
    out.flush().context("flush the handshake")?;
    Ok(())
}

/// The mode, spelled the way [`mode_from_env`] accepts it, so the handshake echoes back exactly
/// what a harness may set. Exhaustive on purpose: a new mode in `partly-proxy-lib` must be
/// named here rather than reported as something it is not.
fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Record => "record",
        Mode::Replay => "replay",
    }
}

/// Why the host stopped. Two possibilities, so it is an enum and not a `&str` (CLAUDE.md
/// rule 1) — the text spelling exists only for the log line.
enum Stop {
    CtrlC,
    StdinClosed,
}

impl Stop {
    fn as_str(&self) -> &'static str {
        match self {
            Stop::CtrlC => "ctrl-c",
            Stop::StdinClosed => "stdin closed",
        }
    }
}

/// Wait for ctrl-c or for stdin to close, whichever comes first.
///
/// The stdin watch is the one that earns its keep. A Playwright worker that is killed — a
/// timeout, a `^C` in the terminal running the suite, a crashed shard — never gets to send a
/// signal to its children, and an orphaned proxy still holding a snapshot file and a control
/// port outlives it and poisons the next run. Losing the pipe is the one event that is
/// guaranteed to reach us in that case.
async fn wait_for_stop() -> Stop {
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            if let Err(err) = signal {
                // Installing the handler failed, so no further ctrl-c will ever arrive. Stopping
                // is the honest answer: a test host that cannot be interrupted is worse than one
                // that exits saying why.
                tracing::error!(%err, "could not listen for ctrl-c");
            }
            Stop::CtrlC
        }
        () = stdin_closed() => Stop::StdinClosed,
    }
}

/// Resolve when stdin reaches EOF, discarding anything that arrives first.
///
/// Read into a small fixed buffer rather than `read_to_end`'s growing `Vec`: nothing
/// communicates with this process over stdin — commands come in over TCP — so the only
/// information the pipe carries is the moment it closes, and a harness that pipes something in
/// by accident should not be able to grow this process's memory while it does.
async fn stdin_closed() {
    let mut stdin = tokio::io::stdin();
    let mut scratch = [0_u8; 256];
    loop {
        match stdin.read(&mut scratch).await {
            Ok(0) => return,
            Ok(_) => {}
            Err(err) => {
                // A broken pipe is the whole point of watching this; anything else read can
                // report is equally a reason to stop trusting the parent to still be there.
                tracing::warn!(%err, "stdin read failed; treating it as closed");
                return;
            }
        }
    }
}
