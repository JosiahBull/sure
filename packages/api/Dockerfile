# syntax=docker/dockerfile:1

# Single-image production build for Sure. Produces one `sure-api` binary that serves
# BOTH the JSON API and the built Svelte SPA — the "single binary" deployment from the
# README. The web/ SPA is bundled into this image (served via WEB_DIR), so it needs no
# image of its own.
#
# Build context is the REPO ROOT, not this directory:
#   docker build -f packages/api/Dockerfile -t sure .
#
# Multi-arch: only the Rust release binary is compiled per target arch. The OpenAPI
# spec and the SPA are architecture-independent, so they build once on the native build
# platform ($BUILDPLATFORM) — under `docker buildx --platform linux/amd64,linux/arm64`
# that keeps the slow, emulated work down to just the arm64 Rust compile.

ARG RUST_VERSION=1
ARG NODE_VERSION=24

# Both builder stages and the runtime sit on Debian 13 (trixie), which is current stable —
# bookworm is oldstable. The three are kept on one release deliberately: the ca-certificates
# bundle the runtime ships and the toolchain that produced the binary then come from the same
# vintage. (The binary is statically linked against musl, so the runtime's glibc is not part
# of the contract — distroless/static carries no libc at all — but the certificate store is.)
# RUST_VERSION floats on the major so a new stable rustc is picked up without an edit here;
# NODE_VERSION is pinned to the 24 LTS line to match `engines.node` and CI's node-version.

# ---- Emit the OpenAPI spec (arch-independent → build once, on the native platform) --
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-trixie AS spec
WORKDIR /build
# Only the Cargo workspace members are needed here (web/client are not Cargo crates).
COPY Cargo.toml Cargo.lock ./
# `appbase` and `testproxy` are workspace members, so cargo needs their manifests to load the
# workspace at all — even though `-p sure-server` never builds testproxy.
COPY packages/appbase ./packages/appbase
COPY packages/telemetry ./packages/telemetry
COPY packages/testproxy ./packages/testproxy
COPY packages/core ./packages/core
COPY packages/scheduler ./packages/scheduler
COPY packages/dal ./packages/dal
COPY packages/providers ./packages/providers
COPY packages/app ./packages/app
COPY packages/api ./packages/api
COPY packages/mcp ./packages/mcp
# `.sqlx` carries the compile-time query metadata every `sqlx::query!` in `sure-dal` is checked
# against, and `.cargo/config.toml` pins SQLX_OFFLINE=true. Both are required for the build: with
# no metadata and no database the macros cannot resolve, and the compile fails. Nothing here ever
# reaches a database — that is the point of shipping the metadata instead.
COPY .cargo ./.cargo
COPY .sqlx ./.sqlx
COPY packages/server ./packages/server
# gen-openapi writes the spec the TypeScript client is generated from (it creates the
# parent directory itself). This is the same artifact `pnpm gen:openapi` produces.
RUN cargo run --locked -p sure-api --bin gen-openapi -- packages/client/openapi.json

# ---- Build the typed client + SPA (arch-independent → build once, native) -----------
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-trixie-slim AS web
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
WORKDIR /build
RUN corepack enable
# Manifests first so the (slow) pnpm install layer caches independently of source edits.
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
COPY packages/client/package.json ./packages/client/
COPY packages/web/package.json ./packages/web/
COPY packages/api-tests/package.json ./packages/api-tests/
RUN pnpm install --frozen-lockfile
# Sources + the generated spec from the `spec` stage.
COPY packages/client ./packages/client
COPY packages/web ./packages/web
COPY --from=spec /build/packages/client/openapi.json ./packages/client/openapi.json
# Generate the typed client, then build the SPA. build:fast = `vite build` without
# svelte-check (type-checking is a CI gate, not a packaging step).
RUN pnpm --filter @sure/client generate \
 && pnpm --filter @sure/web build:fast

# ---- Compile the release binary (arch-specific → built per target platform) ---------
# Statically linked against musl, so the runtime image below can be one with no libc, no
# shell and no package manager in it at all. This stage runs *as* the target platform (no
# --platform override), so under `buildx --platform linux/amd64,linux/arm64` each arch
# compiles natively-under-emulation and `musl-gcc` is always the one for the arch in hand —
# no cross toolchain to configure.
FROM rust:${RUST_VERSION}-trixie AS bin
WORKDIR /build
# musl-tools brings `musl-gcc`; cmake and clang are for `aws-lc-sys`, the C/asm crypto
# backend behind rustls, which every provider adapter's HTTPS goes through. SQLite is
# compiled into the binary too (libsqlite3-sys `bundled`), which is what leaves the runtime
# image needing no shared library whatsoever. trixie carries all four under these same names;
# on Debian 13.6 that resolves to musl 1.2.5, cmake 3.31 and clang 19 (a name, not a version,
# is what would break on a future base image — apt has no candidate to fall back to).
RUN apt-get update \
 && apt-get install -y --no-install-recommends musl-tools musl-dev cmake clang \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
# `appbase` and `testproxy` are workspace members, so cargo needs their manifests to load the
# workspace at all — even though `-p sure-server` never builds testproxy.
COPY packages/appbase ./packages/appbase
COPY packages/telemetry ./packages/telemetry
COPY packages/testproxy ./packages/testproxy
COPY packages/core ./packages/core
COPY packages/scheduler ./packages/scheduler
COPY packages/dal ./packages/dal
COPY packages/providers ./packages/providers
COPY packages/app ./packages/app
COPY packages/api ./packages/api
COPY packages/mcp ./packages/mcp
# `.sqlx` carries the compile-time query metadata every `sqlx::query!` in `sure-dal` is checked
# against, and `.cargo/config.toml` pins SQLX_OFFLINE=true. Both are required for the build: with
# no metadata and no database the macros cannot resolve, and the compile fails. Nothing here ever
# reaches a database — that is the point of shipping the metadata instead.
COPY .cargo ./.cargo
COPY .sqlx ./.sqlx
COPY packages/server ./packages/server
# The release profile (Cargo.toml) already strips symbols and enables LTO. The `sure-api`
# binary is produced by the `sure-server` crate (the composition root); `sure-api` itself
# is now a routes/handlers library with no `main`.
#
# The triple is derived from the machine rather than passed in, because this stage already
# *is* the target arch; the binary is then copied to a fixed path so the runtime stage does
# not have to name a triple it would otherwise have to keep in step.
RUN case "$(uname -m)" in \
      x86_64)  TARGET=x86_64-unknown-linux-musl  ;; \
      aarch64) TARGET=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;; \
    esac \
 && rustup target add "$TARGET" \
 && RUSTFLAGS="-C target-feature=+crt-static -C relocation-model=static" \
    CC_x86_64_unknown_linux_musl=musl-gcc \
    CC_aarch64_unknown_linux_musl=musl-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
    cargo build --release --locked -p sure-server --bin sure-api --target "$TARGET" \
 && cp "target/$TARGET/release/sure-api" /sure-api
# `relocation-model=static` above is why the amd64 binary is a plain ET_EXEC. `+crt-static` is
# already the musl default, but on x86_64-unknown-linux-musl rustc then emits a *static-PIE* —
# and on the bookworm base this stage used to sit on, a musl static-PIE still carried a
# PT_INTERP naming /lib/ld-musl-x86_64.so.1, a loader `distroless/static` does not contain.
# That no longer reproduces on trixie. Measured on this base (rustc 1.97.1, musl 1.2.5): a
# minimal binary linked exactly the way this stage links — musl-gcc, `+crt-static` — comes out
# DYN with no PT_INTERP whether the flag is set or not; with the flag it is ET_EXEC, also with
# no PT_INTERP. The flag stays anyway: it pins ET_EXEC rather than inheriting the absence of an
# interpreter from a toolchain that has already changed it once, and dropping it would change
# the shipped artifact (a PIE, with ASLR back) — its own decision, not a side effect of a
# base-image bump. What guards the property is the assertion below, not this flag.
# aarch64 was never affected: a plain static ET_EXEC with no PT_INTERP either way, which is
# why this only ever failed on one arch.
#
# Its own step, not chained onto the build: as part of that `&&` run, any compile failure
# would have been reported as "not statically linked" instead of as itself. A binary that
# asks for a loader fails at runtime with an unhelpful "no such file or directory" (the
# missing thing being the loader), so it is worth catching at build time.
#
# The assertion is PT_INTERP, not `ldd`. `ldd` prints "statically linked" for the static-PIE
# above — true as far as it goes, since nothing is in DT_NEEDED — while the binary still names
# an interpreter it cannot run without. Reading the program headers asks the question the
# kernel actually asks at execve.
RUN if readelf -l /sure-api | grep -q INTERP; then \
      echo "sure-api requests a program interpreter:" >&2; \
      readelf -l /sure-api | grep -A1 INTERP >&2; \
      echo "the runtime image has no loader for it" >&2; \
      exit 1; \
    fi
# An empty directory to become /data, since the runtime image has no shell to mkdir with.
RUN mkdir -p /empty-data

# ---- Runtime ------------------------------------------------------------------------
# `distroless/static` is about 2 MB and contains no shell, no package manager, no libc and
# no userland — just ca-certificates (outbound TLS for the providers), /etc/passwd for the
# nonroot user, and tzdata. Nothing in here is executable except the one binary below, so
# the usual post-exploitation toolkit — a shell, curl, apt, coreutils — simply is not present.
#
# `sure_appbase::run` installs its own SIGTERM/SIGINT handlers (so PID 1's
# "ignore unhandled signals" rule never applies), and the app spawns no child
# processes at all — the Landlock policy forbids `execve` outright, so there are
# no zombies to reap. See docs/SANDBOX.md and docs/HTTP.md.
FROM gcr.io/distroless/static-debian13:nonroot AS runtime
WORKDIR /app
# Owned by root and not writable by the user that runs it: the binary cannot rewrite itself.
COPY --from=bin --chown=root:root --chmod=0555 /sure-api /usr/local/bin/sure-api
COPY --from=web --chown=root:root /build/packages/web/dist ./web
# 65532 is distroless' `nonroot`. The volume has to be owned by it, and with no shell there
# is nothing to `chown` with — so the ownership is set on the way in from an empty directory.
COPY --from=bin --chown=65532:65532 /empty-data /data

# Single-binary runtime config. BIND_ADDR is 0.0.0.0 because the app's own default,
# 127.0.0.1, is unreachable from outside the container. The SQLite DB lives under /data
# (a volume) so it survives container replacement; sure-api creates the file on start.
#
# On top of the non-root user below, sure-api sandboxes *itself* with Landlock at startup:
# /data is the only writable path, nothing anywhere is executable, and outbound TCP is
# limited to 443 and 53. It needs no privileges and nothing from the runtime, so there is
# nothing to configure here — set SURE_SANDBOX=enforce to refuse to start when the host
# kernel is too old to apply all of it. See docs/SANDBOX.md.
ENV DATABASE_URL=sqlite:/data/sure.db \
    BIND_ADDR=0.0.0.0:8080 \
    WEB_DIR=/app/web \
    RUST_LOG=info
USER 65532:65532
VOLUME ["/data"]
EXPOSE 8080

# The binary probes itself — there is no curl here, and adding one would mean carrying a
# second program (and its CVEs) purely to ask a question the server can answer about itself.
# Exec form, because there is no shell to parse a string one. See packages/server/src/health.rs.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/sure-api", "--health-check"]

ENTRYPOINT ["/usr/local/bin/sure-api"]
