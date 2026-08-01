# The Landlock sandbox

`sure-api` sandboxes itself. On Linux, before it opens the database or binds a socket, it
hands the kernel a list of the accesses it intends to keep and gives up everything else —
permanently, for itself and every thread and child it later creates.

The mechanism is [Landlock](https://landlock.io), an unprivileged Linux LSM available
since 5.13. No root, no capabilities, no container runtime cooperation, nothing to
configure on the host: the process restricts itself, and the restriction can only ever be
narrowed further.

Implementation: [`packages/server/src/sandbox.rs`](../packages/server/src/sandbox.rs).

## Why

The server spends its life parsing things other people wrote — bank CSVs, myIR
spreadsheets (a zip full of XML), JSON bodies, provider API responses, HTTP from whatever
reaches the port. All of its genuinely privileged work happens in the first few
milliseconds: read the config, create the data directory, load the CA store. Everything
after that runs on a much smaller set of rights than the process was started with.

Landlock closes the gap. A memory-safety bug in a parser, a logic bug in a handler, or a
malicious dependency in the tree still can't write outside the data directory, can't read
`$HOME`, can't `execve` a shell, and can't open a connection to anywhere but the two ports
the app legitimately talks to.

## The policy

| | granted | why |
| --- | --- | --- |
| **write** | the database directory | SQLite writes `sure.db` plus its `-wal`, `-shm` and `-journal` sidecars, so the *directory* is the unit — not the file |
| **read** | `WEB_DIR` | `ServeDir` serves the built SPA out of it |
| **read** | `/etc` | DNS (`resolv.conf`, `hosts`, `nsswitch.conf`, `gai.conf`), the CA store under `/etc/ssl/certs`, and `ld.so.cache` |
| **read** | `/lib`, `/lib64`, `/usr/lib`, `/usr/lib64`, `/usr/local/lib` | glibc `dlopen`s its NSS resolver modules lazily, on the *first* name lookup — which happens long after the sandbox is up |
| **read** | `/usr/share/ca-certificates`, `/usr/local/share/ca-certificates`, `/usr/share/pki`, plus `SSL_CERT_FILE`/`SSL_CERT_DIR` if set | see below |
| **read** | `/dev/urandom` | SQLite's unix VFS seeds its PRNG from it |
| **TCP bind** | the port in `BIND_ADDR` | |
| **TCP connect** | 443, 53 | every provider base URL is `https://`; 53 covers a resolver falling back to TCP on a truncated answer |
| **signals** | processes inside the sandbox | |
| **abstract UNIX sockets** | those created inside the sandbox | |

Everything else is denied. In particular:

- **No execute, anywhere.** Not a single path grants `LANDLOCK_ACCESS_FS_EXECUTE`, so
  every `execve(2)` fails. There is no shell to spawn. (`dlopen` is unaffected — Landlock's
  execute right covers `execve`, not mapping a library.)
- **No writes outside the data directory.** Not the SPA, not `/etc`, not `/tmp`, not the
  binary, not another volume mounted into the same container.
- **No reads outside the table above.** No `$HOME`, no `/root`, no sibling bind mount.
- **No second listening port**, and no outbound connection to anything but 443 and 53 — a
  reverse shell has nowhere to dial.
- **Nothing else in the write grant either.** The database directory gets exactly
  `ReadFile | ReadDir | WriteFile | MakeReg | RemoveFile | Truncate`. No `MakeDir`,
  `MakeSym`, `MakeSock`, `MakeFifo`, `MakeChar`, `MakeBlock`, `RemoveDir`, no `Refer`
  (hard-link or rename across directories), no `IoctlDev`.

The CA directories are there because a Landlock rule binds to the **inode** a path
resolves to, and on Debian every `*.pem` in `/etc/ssl/certs` is a *symlink* into
`/usr/share/ca-certificates`. Granting the directory the links live in grants nothing for
the files themselves. It matters for exactly one provider: the Akahu client goes through
`rustls-platform-verifier`, which reads the system trust store, where the others use
reqwest's bundled webpki roots. Without those directories TLS still works — the
concatenated `ca-certificates.crt` is a real file — but the verifier logs a `Permission
denied` warning for every certificate in the store on its way past.

Two deliberate compromises are worth naming:

`/etc` is granted as a directory rather than as the dozen files actually read. A Landlock
rule binds to the **inode** it was added for, and under Docker `/etc/resolv.conf` and
`/etc/hosts` are bind-mounted files that get *replaced* when the container's network
changes — a per-file allowlist would pin the old inodes and break DNS at the worst moment.
It stays a read-only grant over non-secret configuration.

The library directories are granted for the same reason DNS needs `/etc`: glibc's NSS
modules are loaded on demand. They are read-only, and with no execute right anywhere the
grant cannot be turned into running something.

## What it does not cover

- **UDP.** Landlock mediates TCP bind and connect only. Ordinary DNS is UDP and is not
  restricted; nor is any other datagram traffic.
- **Which host** a TCP connection goes to. The rules are per-port, not per-address —
  outbound 443 to anywhere is allowed.
- **Environment variables.** `AKAHU_USER_TOKEN` and friends live in the process's own
  memory, which no filesystem policy touches.
- **Already-open file descriptors.** stdout, stderr, and the listening socket keep
  working; Landlock gates the syscalls that *resolve a path*, not I/O on an existing fd.

Landlock is one layer. It composes with, and does not replace, running as a non-root user
(the image uses uid 10001), `cap_drop: ALL`, a read-only root filesystem, and seccomp.

## Ordering: why `main` builds the tokio runtime by hand

`landlock_restrict_self(2)` restricts **the calling thread**. Threads that already exist
keep their unrestricted domain; threads created afterwards inherit the restricted one.
Applying the sandbox from inside `#[tokio::main]` would therefore protect exactly one
worker and leave the rest of the pool wide open — a sandbox that looks enforced in the log
and isn't.

So [`main`](../packages/server/src/main.rs) is a plain `fn`: it loads config, applies the
sandbox while the process is still single-threaded, and only then builds the runtime.
`sandbox::apply` counts `/proc/self/task` and **refuses to start** if that ordering has
been broken, rather than enforcing a policy that covers one thread out of eight.

Two things have to be read before the sandbox goes on, for the same reason:

- `available_parallelism()`, which reads the cgroup CPU quota from `/proc` and `/sys`. Left
  to the runtime it would fail silently and size the worker pool from the *host's* CPU
  count, over-threading a container given a fraction of a machine.
- `SQLITE_TMPDIR`, which `apply` points at the database directory when it isn't already
  set. SQLite spills large sorts and temporary b-trees to a file, and would otherwise pick
  `/tmp` — a directory the policy has no reason to make writable.

## Configuration

| Env var | Default | Meaning |
| --- | --- | --- |
| `SURE_SANDBOX` | `best-effort` | `off` — don't sandbox. `best-effort` — apply whatever the running kernel supports and start either way. `enforce` — refuse to start unless the whole policy is in force. |
| `SURE_SANDBOX_READ_PATHS` | — | Extra read-only paths, `:`-separated. |
| `SURE_SANDBOX_WRITE_PATHS` | — | Extra read-write paths, `:`-separated. |
| `SURE_SANDBOX_CONNECT_PORTS` | — | Extra TCP connect ports, `,`-separated — for pointing a provider at a local mock, or an egress proxy on an odd port. |

The three `SURE_SANDBOX_*` lists can only *add* to the policy, and none of them can grant
execute. A path named in one of them must exist: being explicit and wrong should be loud,
so a missing one stops startup rather than quietly narrowing the sandbox.

`SURE_SANDBOX=enforce` is the right setting for a deployment that knows its kernel — it
requires Landlock ABI 6 or newer (Linux 6.12), the point at which the filesystem rules,
the network rules and the scoping all exist. It is not the default because Landlock has
grown one ABI version at a time, and an older kernel should mean *less* sandbox rather
than a server that won't boot.

## Verifying it

Startup logs one line saying what stuck:

```
INFO sure_server::sandbox: sandbox enforced
  read_only=/etc:/usr/lib:/app/web read_write=/data
  bind_port=8080 connect_ports=[443, 53]
  kernel="landlock abi V8 (kernel reports None)"
```

- **`sandbox enforced`** (INFO) — everything in the policy table is in force. Note that
  the underlying `RulesetStatus` is usually `PartiallyEnforced` and that is *not* a
  problem: the code asks for the newest ABI it knows about (v9, Linux 7.1) so that a newer
  kernel gets its refinements, and anything above ABI 6 that the kernel doesn't implement
  is a refinement, not a hole.
- **`sandbox only partly enforced`** (WARN) — the kernel is older than 6.12, so the
  network rules and/or the signal and socket scoping were dropped. The filesystem policy
  is still on. The message names the effective ABI.
- **`sandbox NOT enforced`** (WARN) — no Landlock at all. If it says `NotEnabled`, the
  kernel has it compiled in but not switched on: add `landlock,` to the front of
  `CONFIG_LSM`, or to the `lsm=` boot parameter.

The last two are startup errors under `SURE_SANDBOX=enforce`.

### Testing it

`cargo test -p sure-server` on Linux runs
`sandbox::enforcement_tests::the_policy_denies_what_it_should`, which applies a real
policy against the real kernel and then checks, one by one, that writing outside the
granted directory, reading outside it, `mkdir`, `symlink`, `execve`, binding an unlisted
port and connecting to an unlisted port all fail with `EACCES`/`EPERM` — and that the
granted operations still succeed.

Applying a Landlock domain is irreversible, so that test re-executes the test binary as a
child process and asserts on its exit status; nothing else in the suite is affected. On a
kernel without Landlock the child reports that and the test skips rather than passing
quietly. The remaining unit tests cover which paths and ports end up in the plan, and the
plan is handed to the kernel verbatim.
