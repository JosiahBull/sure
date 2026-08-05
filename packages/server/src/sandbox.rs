//! Self-sandboxing with [Landlock](https://landlock.io): the process drops every ambient
//! right it does not need, before it starts doing work.
//!
//! Landlock is an unprivileged Linux LSM (5.13+). A process builds a ruleset describing
//! the accesses it wants to keep, applies it to itself, and from then on the kernel
//! refuses everything else — for that process and every thread and child it goes on to
//! create. There is no way back: a domain can only ever be narrowed. That makes it the
//! right shape for a server that does all of its privileged setup up front (read the
//! config, create the data directory) and then spends the rest of its life parsing
//! untrusted input — bank CSVs, myIR spreadsheets, JSON bodies, HTTP from the network.
//!
//! # What survives
//!
//! | | kept | so that |
//! |---|---|---|
//! | write | the database directory, and nothing else | SQLite can write `sure.db` and its `-wal`/`-shm` sidecars |
//! | read | the SPA directory, `/etc`, the system library and CA directories, `/dev/urandom` | static files, DNS/TLS/linker config, SQLite's PRNG |
//! | execute | *nothing* | there is no `execve(2)` this server ever makes |
//! | TCP bind | the one configured port | |
//! | TCP connect | 443 and 53 | the provider APIs are all HTTPS; 53 covers DNS falling back to TCP |
//! | signals | processes inside the sandbox | |
//! | abstract UNIX sockets | those created inside the sandbox | |
//!
//! Everything absent from that table is denied: no writes outside the data directory, no
//! reads of `$HOME` or another tenant's volume, no `execve`, no device `ioctl`s, no
//! `mknod`, no hard links or renames across directories, no listening on a second port,
//! no outbound connection to an arbitrary service, no signalling the rest of the host.
//!
//! # Why this runs in `main`, before the runtime
//!
//! `landlock_restrict_self(2)` restricts **the calling thread**, and threads that already
//! exist keep their unrestricted domain. Applying the sandbox from inside an async
//! context would therefore protect exactly one of tokio's worker threads and leave the
//! rest wide open. [`apply`] must be called while the process is still single-threaded,
//! so that the runtime's workers inherit the domain when they are spawned;
//! [`Plan::apply`] verifies that and refuses to pretend otherwise if it isn't true.
//!
//! # Kernels that can't do all of it
//!
//! Landlock has grown one ABI version at a time — network rules only arrived in 6.7,
//! scoping in 6.12 — so what actually sticks depends on the running kernel. In the
//! default [`SandboxMode::BestEffort`] the ruleset degrades to whatever the kernel
//! implements and startup continues; [`SandboxMode::Enforce`] refuses to start unless the
//! whole policy is in force. Either way the outcome is logged, with the effective ABI,
//! rather than being silently assumed.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::config::Config;

/// Directories a dynamic loader may pull shared objects from after startup.
///
/// glibc resolves names through NSS modules (`libnss_dns.so` and friends) that it
/// `dlopen`s lazily, on the *first* lookup — which for this server is the first provider
/// sync, long after the sandbox is up. Read access here is what keeps DNS working.
/// Non-existent entries are skipped, so the same list covers merged-`/usr` and split
/// layouts, and 32- and 64-bit multiarch.
const LIBRARY_DIRS: &[&str] = &["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/local/lib"];

/// System configuration, read-only.
///
/// Granted as one directory rather than the dozen files actually consulted
/// (`resolv.conf`, `hosts`, `nsswitch.conf`, `gai.conf`, `services`, `ld.so.cache`,
/// `ssl/certs/…`) for two reasons. A rule is bound to the *inode* it was added for, and
/// under Docker `/etc/resolv.conf` and `/etc/hosts` are bind-mounted files that get
/// replaced wholesale when the container's network changes — a per-file allowlist would
/// pin the old inodes and break DNS at the worst possible moment. And an allowlist that
/// has to track what glibc, rustls and the loader read next is a maintenance trap. This
/// stays a read-only grant over non-secret configuration; the secrets this process holds
/// arrive as environment variables, which Landlock does not mediate either way.
const CONFIG_DIRS: &[&str] = &["/etc"];

/// Where the individual CA certificates behind the system trust store actually live.
///
/// `/etc/ssl/certs` comes free with [`CONFIG_DIRS`], but on Debian its `*.pem` entries are
/// *symlinks* into these directories — and a Landlock rule resolves to the target's inode,
/// so granting the directory the links sit in grants nothing at all. The Akahu client is
/// the one that cares: it reaches the system store through `rustls-platform-verifier`,
/// where the other providers use reqwest's bundled webpki roots. Without this it still
/// works — `ca-certificates.crt` is a real concatenated file — but logs a `Permission
/// denied` warning for every certificate in the store on the way past.
const CA_CERT_DIRS: &[&str] = &[
    "/usr/share/ca-certificates",
    "/usr/local/share/ca-certificates",
    "/usr/share/pki",
];

/// Character devices opened read-only after startup. SQLite's unix VFS seeds its PRNG
/// from `/dev/urandom`; `getrandom(2)` covers everything else and is a syscall, not a
/// path.
const READ_ONLY_FILES: &[&str] = &["/dev/urandom"];

/// Env vars that relocate the trust store. openssl-probe (under `rustls-native-certs`)
/// honours both, so if an operator has pointed them somewhere custom that is where the
/// certificates will be read from.
const CA_CERT_VARS: &[&str] = &["SSL_CERT_FILE", "SSL_CERT_DIR"];

/// The only port the *built-in* provider endpoints reach: every adapter's
/// `DEFAULT_BASE_URL` in `sure-providers` is `https://`, so 443 and DNS are the whole of a
/// default process's egress.
///
/// A configured endpoint need not be. `FRANKFURTER_BASE_URL` and its two siblings can name a
/// loopback record/replay proxy on an ephemeral port ([`Config::provider_endpoints`]), and
/// that port is deliberately **not** derived from the config and folded in here: every entry
/// in `connect_ports` should be one an operator asked for, or the policy this module logs
/// stops being a policy anyone can predict from what they set. The cost is that a denied
/// `connect(2)` surfaces as an ordinary connection error from whichever adapter was pointed
/// at the proxy, naming nothing about Landlock — so a harness aiming one at `sure-testproxy`
/// has to list the port in `SURE_SANDBOX_CONNECT_PORTS`, or run with `SURE_SANDBOX=off`.
const HTTPS_PORT: u16 = 443;

/// DNS. Landlock only mediates TCP, and a resolver's normal path is UDP — which no
/// ruleset can restrict — but glibc retries over TCP when an answer comes back
/// truncated, and that retry is a `connect(2)` this rule has to allow.
const DNS_TCP_PORT: u16 = 53;

/// How hard to insist on the sandbox. `SURE_SANDBOX`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxMode {
    /// Don't restrict the process at all.
    Off,
    /// Apply every restriction the running kernel supports, and start either way. The
    /// default: an older kernel should mean less sandbox, not a server that won't boot.
    #[default]
    BestEffort,
    /// Refuse to start unless the whole policy is in force — which needs Landlock ABI 6
    /// (Linux 6.12), where the filesystem rules, the network rules and the scoping all
    /// exist. For a deployment that knows its kernel and wants a hole to be loud.
    Enforce,
}

impl SandboxMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxMode::Off => "off",
            SandboxMode::BestEffort => "best-effort",
            SandboxMode::Enforce => "enforce",
        }
    }
}

impl FromStr for SandboxMode {
    type Err = String;

    /// The HTTP/env edge, where the value is still text — the one place a wildcard arm
    /// over these spellings is the point rather than a missed variant.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" => Ok(SandboxMode::Off),
            "best-effort" | "best_effort" | "on" | "1" | "true" | "yes" => {
                Ok(SandboxMode::BestEffort)
            }
            "enforce" | "strict" => Ok(SandboxMode::Enforce),
            other => Err(format!("unknown sandbox mode {other:?}")),
        }
    }
}

/// The parts of the sandbox that come from the environment rather than from what the
/// server does. Every field is an *addition* to the built-in policy — there is no way to
/// widen it beyond what is listed here, and no way to grant execute.
#[derive(Clone, Debug, Default)]
pub struct SandboxConfig {
    pub mode: SandboxMode,
    /// `SURE_SANDBOX_READ_PATHS`, `:`-separated.
    pub read_paths: Vec<PathBuf>,
    /// `SURE_SANDBOX_WRITE_PATHS`, `:`-separated.
    pub write_paths: Vec<PathBuf>,
    /// `SURE_SANDBOX_CONNECT_PORTS`, `,`-separated. For pointing a provider at a local
    /// mock, or an egress proxy on a non-standard port.
    pub connect_ports: Vec<u16>,
}

/// The resolved policy: what [`apply`] is about to hand the kernel.
///
/// Built separately from being enforced so the decisions are testable on any platform,
/// and so the log line describes exactly what was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub read_only: Vec<PathBuf>,
    pub read_write: Vec<PathBuf>,
    pub bind_port: u16,
    pub connect_ports: Vec<u16>,
}

/// Lock the process down according to `config`, and log what stuck.
///
/// Must be called while the process is single-threaded — see the module docs. Does
/// nothing when `SURE_SANDBOX=off`, or on a non-Linux host (macOS development), where it
/// logs that it can't sandbox unless the mode is `enforce`.
pub fn apply(config: &Config) -> anyhow::Result<()> {
    let mode = config.sandbox.mode;
    if mode == SandboxMode::Off {
        tracing::warn!("sandbox disabled by SURE_SANDBOX=off");
        return Ok(());
    }

    // Has to exist before it can be granted: a Landlock rule is added by opening the
    // path. `sure_dal::connect` would create it moments later anyway.
    let database_dir = sure_dal::ensure_database_dir(&config.database_url)?;

    // SQLite spills large sorts and temporary b-trees to a file, and picks the directory
    // for it from `SQLITE_TMPDIR`, then `TMPDIR`, then `/var/tmp`, `/usr/tmp`, `/tmp`.
    // Left alone that would land in a shared temp directory the policy has no reason to
    // make writable — so point it at the data directory, which already is. Mutating the
    // environment is sound here and only here: `apply` runs from `main` before the tokio
    // runtime exists, so nothing else can be reading it concurrently.
    if let Some(dir) = &database_dir {
        if std::env::var_os("SQLITE_TMPDIR").is_none() {
            std::env::set_var("SQLITE_TMPDIR", dir);
        }
    }

    let plan = Plan::build(
        database_dir.as_deref(),
        config.web_dir.as_deref(),
        config.bind_addr,
        &config.sandbox,
    )?;
    plan.apply(mode)
}

impl Plan {
    /// Resolve the policy for one configuration. Creates nothing and enforces nothing —
    /// every path it names must already exist — which is what makes it testable off Linux.
    fn build(
        database_dir: Option<&Path>,
        web_dir: Option<&str>,
        bind_addr: SocketAddr,
        extra: &SandboxConfig,
    ) -> anyhow::Result<Self> {
        let mut read_write = Vec::new();
        if let Some(dir) = database_dir {
            push_optional(&mut read_write, dir, "database directory");
        }
        // Where SQLite will spill a large sort or a temporary b-tree. `apply` normally
        // points this at the database directory, collapsing the two into one rule; an
        // operator who sets it explicitly gets the directory they asked for.
        if let Some(dir) = std::env::var_os("SQLITE_TMPDIR") {
            push_optional(&mut read_write, Path::new(&dir), "SQLITE_TMPDIR");
        }
        for path in &extra.write_paths {
            push_required(&mut read_write, path)?;
        }

        let mut read_only = Vec::new();
        if let Some(dir) = web_dir {
            push_optional(&mut read_only, Path::new(dir), "WEB_DIR");
        }
        for dir in CONFIG_DIRS
            .iter()
            .chain(LIBRARY_DIRS)
            .chain(CA_CERT_DIRS)
            .chain(READ_ONLY_FILES)
        {
            push_optional(&mut read_only, Path::new(dir), "system path");
        }
        for var in CA_CERT_VARS {
            if let Some(path) = std::env::var_os(var) {
                push_optional(&mut read_only, Path::new(&path), var);
            }
        }
        for path in &extra.read_paths {
            push_required(&mut read_only, path)?;
        }
        // A path granted read-write needs no separate read-only rule; dropping it keeps
        // the log honest about which list each path ended up in.
        read_only.retain(|path| !read_write.contains(path));

        let mut connect_ports = vec![HTTPS_PORT, DNS_TCP_PORT];
        for port in &extra.connect_ports {
            if !connect_ports.contains(port) {
                connect_ports.push(*port);
            }
        }

        Ok(Self {
            read_only,
            read_write,
            bind_port: bind_addr.port(),
            connect_ports,
        })
    }
}

/// Canonicalise and record a path that the policy would like but can work without —
/// `/lib64` on a merged-`/usr` system, `/dev/urandom` in a minimal container. Resolving
/// symlinks here is also what collapses Debian's `/lib` → `/usr/lib` into one rule.
fn push_optional(paths: &mut Vec<PathBuf>, path: &Path, what: &str) {
    match std::fs::canonicalize(path) {
        Ok(resolved) => {
            if !paths.contains(&resolved) {
                paths.push(resolved);
            }
        }
        Err(err) => {
            tracing::debug!(path = %path.display(), kind = what, error = %err, "not granted; it does not exist");
        }
    }
}

/// Same, for a path an operator named explicitly. Being explicit and wrong should be
/// loud, so a missing one stops startup rather than quietly narrowing the sandbox into
/// something the operator did not ask for.
fn push_required(paths: &mut Vec<PathBuf>, path: &Path) -> anyhow::Result<()> {
    let resolved = std::fs::canonicalize(path)
        .map_err(|err| anyhow::anyhow!("sandbox path {}: {err}", path.display()))?;
    if !paths.contains(&resolved) {
        paths.push(resolved);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn display(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(target_os = "linux")]
impl Plan {
    fn apply(&self, mode: SandboxMode) -> anyhow::Result<()> {
        use landlock::{LandlockStatus, RulesetStatus, ABI};

        // The ABI at which every restriction this policy actually relies on exists:
        // filesystem rights (v1), TCP bind/connect (v4) and scoping (v6). `enforce`
        // deliberately asks for more than this — the newest ABI the crate knows — so a
        // newer kernel gets its refinements too (v9's `ResolveUnix`, say). Missing *those*
        // is a note; missing anything below this line is a hole worth warning about.
        const MATERIAL: ABI = ABI::V6;

        self.check_single_threaded(mode)?;
        let status = self.enforce()?;

        let (kernel, complete) = match status.landlock {
            LandlockStatus::NotEnabled => (
                "landlock is built into the kernel but not enabled (prepend \"landlock,\" to \
                 CONFIG_LSM, or to the \"lsm=\" boot parameter)"
                    .to_string(),
                false,
            ),
            LandlockStatus::NotImplemented => (
                "landlock is not built into this kernel (needs 5.13+)".to_string(),
                false,
            ),
            LandlockStatus::Available {
                effective_abi,
                kernel_abi,
            } => (
                format!("landlock abi {effective_abi:?} (kernel reports {kernel_abi:?})"),
                effective_abi >= MATERIAL,
            ),
        };

        if mode == SandboxMode::Enforce {
            // `no_new_privs` is what stops a setuid binary from being used to escape the
            // domain. The crate sets it as part of `restrict_self`; if it did not take,
            // the ruleset is not the guarantee it looks like.
            anyhow::ensure!(
                status.no_new_privs,
                "SURE_SANDBOX=enforce: prctl(PR_SET_NO_NEW_PRIVS) did not take"
            );
            anyhow::ensure!(complete, "SURE_SANDBOX=enforce: {kernel}");
        }

        // Bound to locals rather than inlined: inside a `tracing` macro, `%display(..)`
        // resolves to `tracing::field::display`, not to the helper below it.
        let read_only = display(&self.read_only);
        let read_write = display(&self.read_write);
        let connect_ports = format!("{:?}", self.connect_ports);

        match status.ruleset {
            // `FullyEnforced` only happens on a kernel as new as the newest ABI the crate
            // knows about, so `PartiallyEnforced` is the ordinary case, not a problem —
            // what decides the severity is whether the *material* rules made it in.
            RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced if complete => {
                tracing::info!(
                    read_only,
                    read_write,
                    bind_port = self.bind_port,
                    connect_ports,
                    kernel,
                    "sandbox enforced"
                );
            }
            RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => tracing::warn!(
                read_only,
                read_write,
                bind_port = self.bind_port,
                connect_ports,
                kernel,
                "sandbox only partly enforced: this kernel is too old for the network rules \
                 (abi 4) or the signal/socket scoping (abi 6)"
            ),
            RulesetStatus::NotEnforced => {
                tracing::warn!(kernel, "sandbox NOT enforced; running unrestricted");
            }
        }
        Ok(())
    }

    /// Hand the policy to the kernel and report what it made of it.
    ///
    /// Split out from [`Plan::apply`] so that the tests can enforce a policy and inspect
    /// the result without the single-thread check (which a test harness can never pass)
    /// or the logging.
    fn enforce(&self) -> anyhow::Result<landlock::RestrictionStatus> {
        use landlock::{
            Access, AccessFs, AccessNet, BitFlags, NetPort, PathBeneath, PathFd, Ruleset,
            RulesetAttr, RulesetCreatedAttr, Scope, ABI,
        };

        // The newest ABI this crate knows about. Compatibility is best-effort by default,
        // so asking for all of it on an older kernel enforces the subset that exists
        // rather than failing — and the status says which happened.
        const NEWEST: ABI = ABI::V9;

        // Deliberately not `AccessFs::from_read`, which also includes `Execute`: no path
        // in this policy grants it, so every `execve(2)` the process could be tricked
        // into making is refused. `dlopen` is unaffected — it reads and maps, and
        // Landlock's execute right only covers `execve`.
        let read: BitFlags<AccessFs> = AccessFs::ReadFile | AccessFs::ReadDir;
        // Exactly what SQLite does to a database directory: create the file and its
        // `-wal`/`-shm`/`-journal` sidecars, read and write them, truncate on a WAL
        // checkpoint, and unlink the sidecars on a clean close. Notably absent, because
        // nothing here needs them: MakeDir, MakeSym, MakeSock, MakeFifo, MakeChar,
        // MakeBlock, RemoveDir, Refer (link/rename across directories) and IoctlDev.
        let write: BitFlags<AccessFs> = read
            | AccessFs::WriteFile
            | AccessFs::MakeReg
            | AccessFs::RemoveFile
            | AccessFs::Truncate;

        let mut ruleset = Ruleset::default()
            // Handle *every* filesystem right, so anything not granted below is denied.
            .handle_access(AccessFs::from_all(NEWEST))?
            .handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp)?
            .scope(Scope::AbstractUnixSocket | Scope::Signal)?
            .create()?;

        for path in &self.read_only {
            ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(path)?, read))?;
        }
        for path in &self.read_write {
            ruleset = ruleset.add_rule(PathBeneath::new(PathFd::new(path)?, write))?;
        }
        ruleset = ruleset.add_rule(NetPort::new(self.bind_port, AccessNet::BindTcp))?;
        for port in &self.connect_ports {
            ruleset = ruleset.add_rule(NetPort::new(*port, AccessNet::ConnectTcp))?;
        }

        Ok(ruleset.restrict_self()?)
    }

    /// Refuse to hand back a false sense of security.
    ///
    /// `landlock_restrict_self(2)` restricts the calling thread; siblings that already
    /// exist keep their old domain. (ABI v8 adds an all-threads flag, but relying on it
    /// would silently do nothing on every kernel below 7.0.) So the ordering in `main` —
    /// sandbox first, tokio runtime second — is load-bearing, and this is the check that
    /// makes breaking it a startup failure instead of a sandbox that covers one worker.
    fn check_single_threaded(&self, mode: SandboxMode) -> anyhow::Result<()> {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            tracing::warn!(
                "could not read /proc/self/task; cannot confirm the process is \
                single-threaded, so the sandbox may only cover the calling thread"
            );
            return Ok(());
        };
        let threads = tasks.count();
        if threads > 1 {
            let message = format!(
                "sandbox::apply must run before any thread is spawned, but this process \
                 already has {threads}; the sandbox would cover only the calling thread"
            );
            match mode {
                // Unreachable: `apply` returns before building a plan.
                SandboxMode::Off => {}
                SandboxMode::BestEffort | SandboxMode::Enforce => anyhow::bail!(message),
            }
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
impl Plan {
    /// Landlock is a Linux LSM; there is no equivalent to fall back to here. Development
    /// happens on macOS, so this is a logged no-op rather than a build failure.
    fn apply(&self, mode: SandboxMode) -> anyhow::Result<()> {
        match mode {
            // Unreachable: `apply` returns before building a plan.
            SandboxMode::Off => {}
            SandboxMode::BestEffort => {
                tracing::warn!("sandbox unavailable: landlock is linux-only");
            }
            SandboxMode::Enforce => {
                anyhow::bail!("SURE_SANDBOX=enforce: landlock is linux-only");
            }
        }
        Ok(())
    }
}

/// The policy actually being enforced, exercised end to end against a live kernel.
///
/// Applying a Landlock domain is irreversible and would poison every test that ran after
/// it in the same process, so the one test that does it re-executes this very test binary
/// with [`CHILD`] set and asserts on the child's exit status. The child restricts its own
/// thread and then tries, one by one, the things the policy is supposed to stop.
#[cfg(all(test, target_os = "linux"))]
mod enforcement_tests {
    use super::*;
    use std::io::ErrorKind;
    use std::net::{TcpListener, TcpStream};

    /// Set on the re-executed child; its value is the temp directory to grant.
    const CHILD: &str = "SURE_SANDBOX_ENFORCEMENT_CHILD";
    const TEST: &str = "sandbox::enforcement_tests::the_policy_denies_what_it_should";
    /// Printed by the child once every denial has actually been observed. A child that
    /// exits cleanly without it proved nothing, so the parent insists on seeing it.
    const ENFORCED: &str = "SANDBOX-ENFORCED";
    /// Printed instead when the running kernel has no Landlock to test against.
    const SKIPPED: &str = "SANDBOX-UNAVAILABLE";

    #[test]
    fn the_policy_denies_what_it_should() {
        match std::env::var(CHILD) {
            Ok(dir) => child(PathBuf::from(dir)),
            Err(_) => parent(),
        }
    }

    fn parent() {
        let root = std::env::temp_dir().join(format!("sure-sandbox-{}", std::process::id()));
        let granted = root.join("granted");
        let denied = root.join("denied");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&denied).unwrap();
        std::fs::write(denied.join("secret"), b"x").unwrap();

        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, &root)
            .output()
            .unwrap();
        std::fs::remove_dir_all(&root).ok();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let report = format!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
        assert!(output.status.success(), "sandboxed child failed\n{report}");

        if stdout.contains(SKIPPED) {
            // A kernel without Landlock (an old CI image). Nothing was proved, and the
            // test says so rather than passing quietly.
            println!("SKIPPED: this kernel has no Landlock to enforce against");
            return;
        }
        assert!(
            stdout.contains(ENFORCED),
            "the child exited cleanly but never confirmed the sandbox was enforced\n{report}"
        );
    }

    fn child(root: PathBuf) {
        let granted = root.join("granted");
        let denied = root.join("denied");

        // Opened before the sandbox goes on: `bind(2)` is what Landlock checks, so a
        // listener created now keeps working and gives the connect test a live port that
        // is deliberately *not* in the policy.
        let unreachable = TcpListener::bind("127.0.0.1:0").unwrap();
        let unreachable_addr = unreachable.local_addr().unwrap();
        let allowed_port = free_port();
        let denied_port = free_port();

        let plan = Plan {
            read_only: vec![PathBuf::from("/etc")],
            read_write: vec![granted.clone()],
            bind_port: allowed_port,
            connect_ports: vec![HTTPS_PORT, DNS_TCP_PORT],
        };
        let status = plan.enforce().expect("restrict_self");
        if status.ruleset == landlock::RulesetStatus::NotEnforced {
            // No Landlock on this kernel. Asserting denials here would test nothing.
            println!("{SKIPPED}: {:?}", status.landlock);
            return;
        }
        assert!(status.no_new_privs, "no_new_privs did not take");

        // --- filesystem -----------------------------------------------------------
        std::fs::write(granted.join("db"), b"ok").expect("write inside the granted directory");
        std::fs::read(granted.join("db")).expect("read back inside the granted directory");
        std::fs::remove_file(granted.join("db")).expect("unlink a sidecar");
        std::fs::read("/etc/hostname").expect("read a granted read-only path");

        denies(
            "write outside the policy",
            std::fs::write(denied.join("x"), b""),
        );
        denies(
            "read outside the policy",
            std::fs::read(denied.join("secret")),
        );
        denies("write to a read-only path", std::fs::write("/etc/x", b""));
        // Nothing anywhere grants MakeDir or MakeSym, not even the writable directory.
        denies("mkdir", std::fs::create_dir(granted.join("sub")));
        denies(
            "symlink",
            std::os::unix::fs::symlink("/etc/passwd", granted.join("link")),
        );

        // --- execute --------------------------------------------------------------
        // No path grants Execute, so there is no binary in the image this can reach.
        denies(
            "execve",
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(":")
                .status(),
        );

        // --- network --------------------------------------------------------------
        TcpListener::bind(("127.0.0.1", allowed_port)).expect("bind the configured port");
        denies(
            "bind an unlisted port",
            TcpListener::bind(("127.0.0.1", denied_port)),
        );
        denies(
            "connect to an unlisted port",
            TcpStream::connect(unreachable_addr),
        );

        println!("{ENFORCED}: {:?}", status.ruleset);
    }

    /// A free port, found by binding and letting go. Racy in principle; the window is a
    /// few microseconds and nothing else in the test is listening.
    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Assert the kernel refused an operation, and that it refused it for the right
    /// reason — an `EACCES`/`EPERM` from Landlock, not an unrelated `ENOENT`.
    fn denies<T: std::fmt::Debug>(what: &str, result: std::io::Result<T>) {
        match result {
            Ok(value) => panic!("{what} was allowed ({value:?})"),
            Err(err) => assert_eq!(
                err.kind(),
                ErrorKind::PermissionDenied,
                "{what} failed, but not because the sandbox refused it: {err}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(database_dir: Option<&Path>, extra: SandboxConfig) -> Plan {
        Plan::build(
            database_dir,
            None,
            "127.0.0.1:8080".parse().unwrap(),
            &extra,
        )
        .unwrap()
    }

    #[test]
    fn mode_round_trips_through_its_text_form() {
        for mode in [
            SandboxMode::Off,
            SandboxMode::BestEffort,
            SandboxMode::Enforce,
        ] {
            assert_eq!(mode.as_str().parse::<SandboxMode>(), Ok(mode));
        }
    }

    #[test]
    fn an_unknown_mode_is_rejected_rather_than_defaulted() {
        // Silently falling back to `off` on a typo would be the worst possible failure.
        assert!("enforced".parse::<SandboxMode>().is_err());
    }

    #[test]
    fn an_in_memory_database_needs_nothing_writable() {
        // `SQLITE_TMPDIR` is not set under `cargo test`; if it ever were, this would be
        // asserting the wrong thing rather than failing, hence the guard.
        assert!(std::env::var_os("SQLITE_TMPDIR").is_none());
        assert_eq!(
            plan(None, SandboxConfig::default()).read_write,
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn the_database_directory_is_the_one_writable_path() {
        let dir = std::env::temp_dir();
        let plan = plan(Some(&dir), SandboxConfig::default());
        assert_eq!(plan.read_write, vec![std::fs::canonicalize(&dir).unwrap()]);
    }

    #[test]
    fn https_and_dns_are_always_connectable() {
        let plan = plan(None, SandboxConfig::default());
        assert!(plan.connect_ports.contains(&HTTPS_PORT));
        assert!(plan.connect_ports.contains(&DNS_TCP_PORT));
        assert_eq!(plan.bind_port, 8080);
    }

    #[test]
    fn extra_connect_ports_are_added_once() {
        let plan = plan(
            None,
            SandboxConfig {
                connect_ports: vec![8081, 443, 8081],
                ..SandboxConfig::default()
            },
        );
        assert_eq!(plan.connect_ports, vec![443, 53, 8081]);
    }

    #[test]
    fn a_named_path_that_does_not_exist_stops_startup() {
        let err = Plan::build(
            None,
            None,
            "127.0.0.1:8080".parse().unwrap(),
            &SandboxConfig {
                read_paths: vec![PathBuf::from("/definitely/not/here")],
                ..SandboxConfig::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("/definitely/not/here"));
    }

    #[test]
    fn the_system_paths_that_are_missing_are_skipped_not_fatal() {
        // `/lib64` and `/usr/lib64` don't exist on a merged-`/usr` Debian or on macOS.
        let plan = plan(None, SandboxConfig::default());
        assert!(plan.read_only.iter().all(|p| p.exists()));
    }

    #[test]
    fn a_writable_path_is_not_also_listed_read_only() {
        let tmp = std::env::temp_dir();
        let plan = Plan::build(
            Some(&tmp),
            tmp.to_str(),
            "127.0.0.1:8080".parse().unwrap(),
            &SandboxConfig::default(),
        )
        .unwrap();
        let canonical = std::fs::canonicalize(&tmp).unwrap();
        assert!(plan.read_write.contains(&canonical));
        assert!(!plan.read_only.contains(&canonical));
    }
}
