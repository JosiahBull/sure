use sure_server::config::{Config, load_dotenv};
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
    // Before anything else — before the env file, tracing, config, and above all the
    // sandbox. The container's HEALTHCHECK runs this binary in probe mode (there is no
    // shell or `curl` in the runtime image to do it with), and the Landlock policy permits
    // outbound TCP to 443 and 53 only, so a probe applied after `sandbox::apply` could not
    // reach the server's own port. It needs none of that setup regardless.
    if std::env::args().nth(1).as_deref() == Some(sure_server::health::FLAG) {
        return sure_server::health::probe();
    }

    // First of all, and now more strictly than before: `init_tracing` reads `RUST_LOG`, and
    // everything a provider needs — its base URL in `Config::from_env`, Akahu's token pair in
    // `serve` — is read once, at startup, rather than on the first sync. Nothing gets a second
    // chance to notice the file. That costs the log line a subscriber, so the path is reported
    // once there is one.
    let env_file = load_dotenv()?;
    let telemetry = sure_api::init_tracing();
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

    // Immediately *after* the sandbox and *before* the runtime, and it has to be exactly here.
    //
    // Building an OTLP provider spawns an OS thread (the periodic metric reader, and the batch
    // span/log processors). `sandbox::apply` refuses to run once the process has more than one
    // thread — `landlock_restrict_self(2)` only restricts the caller, so a thread that already
    // exists would keep an unrestricted domain — so doing this any earlier turns every start
    // into "sandbox::apply must run before any thread is spawned". Doing it any later, from
    // inside the runtime, means the exporter's socket is opened by a thread that inherited the
    // domain but by then `serve` is already answering requests it cannot trace.
    //
    // Note the sandbox does not open the collector's port on its own: it permits 443 and 53,
    // and it deliberately derives nothing from configuration. A plaintext collector needs
    // `SURE_SANDBOX_CONNECT_PORTS`. See docs/OBSERVABILITY.md.
    let otel = sure_telemetry::otel::init(&config.telemetry)?;
    telemetry.install(otel.layers);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()?;

    // `run` owns the shutdown sequence from here: signals, cancellation, and waiting for
    // everything `serve` spawned. The exit status is the application's own result — how
    // tidily the process stopped is reported, not returned, so a slow drain doesn't make
    // a successful run look like a crash.
    let lifecycle = config.lifecycle;
    let outcome = sure_appbase::run(runtime, lifecycle, |shutdown| {
        sure_server::serve(config, shutdown)
    });

    // Last of all, and not through `Shutdown::spawn`: the exporters run on their own OS
    // threads, which the drain cannot see (and so can never make a shutdown look unclean).
    // The other half of that bargain is that nothing else will flush them. Blocking is right
    // here — the runtime is gone, the shutdown report has already been logged, and the only
    // thing left to do is get the last batch out.
    otel.guard.shutdown();

    outcome.result
}
