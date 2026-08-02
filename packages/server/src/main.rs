use sure_server::config::{load_dotenv, Config};
use sure_server::sandbox;

/// Deliberately not `#[tokio::main]`.
///
/// The Landlock sandbox has to go on while the process is still single-threaded:
/// `landlock_restrict_self(2)` restricts the calling thread and is inherited by the
/// threads it later spawns, so applying it from inside the runtime would cover one worker
/// and leave the rest unrestricted. Building the runtime by hand puts the sandbox
/// unambiguously first — and `sandbox::apply` refuses to run at all if that ordering is
/// ever broken.
///
/// It is also why `sure_appbase::run` takes a runtime instead of building one: a
/// lifecycle helper that insisted on constructing its own would have to run before the
/// sandbox, or the sandbox would have to run after it. Neither is acceptable.
fn main() -> anyhow::Result<()> {
    // First of all: `init_tracing` reads `RUST_LOG`, and the provider clients read their
    // tokens lazily on the first sync — both have to see whatever the file sets. That
    // costs the log line a subscriber, so the path is reported once there is one.
    let env_file = load_dotenv()?;
    sure_api::init_tracing();
    if let Some(path) = &env_file {
        tracing::info!(file = %path.display(), "loaded env file");
    }
    let config = Config::from_env()?;

    // Also before the sandbox: sizing the worker pool reads the cgroup CPU quota from
    // /proc and /sys, which the sandbox does not grant. Left to the runtime it would fail
    // silently and fall back to the *host's* CPU count, over-threading a container that
    // has been given a fraction of a machine.
    let worker_threads = std::thread::available_parallelism().map_or(1, |n| n.get());

    sandbox::apply(&config)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()?;

    // `run` owns the shutdown sequence from here: signals, cancellation, and waiting for
    // everything `serve` spawned. The exit status is the application's own result — how
    // tidily the process stopped is reported, not returned, so a slow drain doesn't make
    // a successful run look like a crash.
    let lifecycle = config.lifecycle;
    sure_appbase::run(runtime, lifecycle, |shutdown| {
        sure_server::serve(config, shutdown)
    })
    .result
}
