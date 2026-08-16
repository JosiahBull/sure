#!/usr/bin/env bash
#
# The inside half of `pnpm snapshots:verify` / `pnpm snapshots:update`. Runs as root inside
# mcr.microsoft.com/playwright:<the tag checks.yml pins>, driven by scripts/linux-snapshots.mjs
# — read that file first; it explains why this exists and does the preflight this script
# assumes has already passed.
#
#   linux-snapshots.container.sh <verify|update> [extra playwright args...]
#
# It is bash rather than a second .mjs because every line is a process invocation with a pipe or
# a conditional — the shell is the language for that, and `node --run`-ing an .mjs that only
# shells out would obscure the one thing a reader needs to check against the CI job it mirrors
# (the `web-tests` steps in .github/workflows/checks.yml). The host half stays .mjs, like the
# rest of scripts/, because it does argument handling, YAML/JSON reading and file comparison.
#
# Contract with the host: the repo is at /src read-only, three named volumes carry the caches
# ($CARGO_HOME, /work/target, /pnpm-store), and everything this run produces for the host is
# written under /out.
set -euo pipefail

mode="${1:?usage: linux-snapshots.container.sh <verify|update> [playwright args...]}"
shift

# Before the two `command -v` checks below, because rustup lives in the CARGO_HOME volume and is
# therefore *already installed* on every run but the first — while not being on PATH, since
# nothing in this container's profile knows about the volume. Checking first and exporting after
# would reinstall the toolchain every run.
export PATH="${CARGO_HOME:?CARGO_HOME must be set by the host script}/bin:$PATH"

echo "▶ copying the worktree into /work"
# Copied rather than built in place, in both directions:
#   * the host's node_modules and target are darwin/arm64 — reused here they poison the build
#     with objects and native modules for the wrong platform;
#   * /src is mounted read-only precisely so this container cannot poison the host's the same
#     way, or write into a tree the developer has open.
# `data` is excluded because on a developer's machine it is data/sure.db — the live financial
# database (CLAUDE.md). global-setup.ts recreates the directory for its own throwaway test-e2e.db.
mkdir -p /work
tar -C /src -cf - \
  --exclude=./.git \
  --exclude=./node_modules \
  --exclude='./packages/*/node_modules' \
  --exclude=./target \
  --exclude=./data \
  . | tar -C /work -xf -
cd /work

# Two separate conditions, not one. CARGO_HOME persists in a named volume but the container
# filesystem does not, so on the second run `cargo` is present while the linker it shells out to
# is gone — installing both together behind a single `command -v cargo` check gives a confusing
# "linker `cc` not found" at the end of a ten-minute emulated build.
if ! command -v cc >/dev/null 2>&1; then
  echo "▶ installing the C toolchain (the Playwright image ships Node and browsers, not a linker)"
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends build-essential pkg-config curl >/dev/null
fi
# `cargo --version`, not `command -v cargo`: the shims and the toolchain they dispatch to live in
# *different* volumes-worth of state — the shims in $CARGO_HOME/bin, the installed toolchain and
# the `default_toolchain` setting under $RUSTUP_HOME — and the two can get out of step. When they
# do, the shim exists, this guard passes, and the build dies twenty lines later with
#
#     error: rustup could not choose a version of cargo to run, because one wasn't specified
#     explicitly, and no default is configured
#
# which says nothing about the actual fix. Two ways to reach it: an interrupted first run that
# wrote the shims before the toolchain finished downloading, and a CARGO_HOME volume populated
# before RUSTUP_HOME was pinned into it (rustup then defaulted to ~/.rustup, which the container
# discards). Testing that cargo *runs* covers both, and costs one process on a path that is
# otherwise about to spend minutes building.
if ! cargo --version >/dev/null 2>&1; then
  if command -v rustup >/dev/null 2>&1; then
    # Repair in place rather than reinstalling: rustup-init refuses outright when it finds an
    # existing installation, so the reinstall path below is not available here anyway.
    echo "▶ repairing the Rust toolchain in the CARGO_HOME volume (no default was configured)"
    rustup default stable >/dev/null
  else
    echo "▶ installing Rust into the CARGO_HOME volume (first run only)"
    # --no-modify-path: the PATH export above is the only one that survives, and a profile edit
    # would be written into a container filesystem that is discarded when this exits anyway.
    # --default-toolchain is explicit so the setting this guard checks for is always written,
    # rather than relying on the installer's default staying "install one and make it default".
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --no-modify-path --default-toolchain stable >/dev/null
  fi
fi

# corepack ships with the image's Node and reads the `packageManager` field from package.json, so
# the pnpm version here is the one the lockfile was written by. `|| true` and then a hard check:
# corepack is deprecated upstream and a future image may drop it, and "pnpm: command not found"
# forty lines into the log is worth one line of diagnosis here.
corepack enable >/dev/null 2>&1 || true
if ! command -v pnpm >/dev/null 2>&1; then
  echo "✗ no pnpm in the image and 'corepack enable' did not provide one." >&2
  echo "  Install it in this script (npm i -g pnpm@\$(node -p \"require('/work/package.json').packageManager.split('@')[1]\"))." >&2
  exit 1
fi
# The store is a named volume, so the second run installs from disk rather than the network.
pnpm config set store-dir /pnpm-store --global >/dev/null 2>&1 || true

echo "▶ pnpm install"
pnpm install --frozen-lockfile
echo "▶ pnpm gen:client"
# Not silenced: on a cold volume this compiles gen-openapi under emulation, which is minutes of
# apparent silence otherwise.
pnpm gen:client

playwright_args=(test)
if [ "$mode" = "update" ]; then
  # `=all`, not a bare `--update-snapshots`. Bare means "changed", which is decided by the same
  # comparator the assertion uses — and this suite runs at `maxDiffPixelRatio: 0.03`, so a real
  # UI change under that tolerance leaves the stale baseline on disk and the run still passes.
  # The whole point of update mode is to get the *current* rendering committed.
  playwright_args+=(--update-snapshots=all)
fi

echo "▶ playwright ${playwright_args[*]} $*"
# global-setup builds the SPA and both Rust binaries, spawns the backend on 8099 and seeds it.
# 8099 is a fixed port, but it is bound inside this container's own network namespace — so unlike
# a host-side `pnpm test:web`, this cannot collide with anything already listening out there.
status=0
pnpm --filter @sure/web exec playwright "${playwright_args[@]}" "$@" || status=$?

# Everything below runs whether the suite passed or failed: a failure is exactly when the host
# wants the artefacts.
if [ "$mode" = "update" ]; then
  echo "▶ exporting *-linux.png to /out/snapshots"
  mkdir -p /out/snapshots
  # `cp --parents` keeps tests/<spec>.ts-snapshots/ in the path, so the host can copy each file
  # back over the one it came from without knowing the layout.
  (cd packages/web && find tests -name '*-linux.png' -exec cp --parents {} /out/snapshots \;)
fi

if [ -d packages/web/test-results ]; then
  echo "▶ exporting packages/web/test-results to /out (a failure leaves its *-actual.png there)"
  rm -rf /out/test-results
  cp -R packages/web/test-results /out/test-results
fi

# This container is root and /out is a bind mount of a host directory. Docker Desktop remaps the
# owner for you; a native Linux daemon does not, and root-owned files inside a directory the next
# run tries to clear are an EACCES with no obvious cause.
chown -R "${HOST_UID:-0}:${HOST_GID:-0}" /out 2>/dev/null || true

exit "$status"
