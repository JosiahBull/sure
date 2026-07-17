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

# ---- Emit the OpenAPI spec (arch-independent → build once, on the native platform) --
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-bookworm AS spec
WORKDIR /build
# Only the Cargo workspace members are needed here (web/client are not Cargo crates).
COPY Cargo.toml Cargo.lock ./
COPY packages/core ./packages/core
COPY packages/dal ./packages/dal
COPY packages/providers ./packages/providers
COPY packages/api ./packages/api
# gen-openapi writes the spec the TypeScript client is generated from (it creates the
# parent directory itself). This is the same artifact `pnpm gen:openapi` produces.
RUN cargo run --locked -p sure-api --bin gen-openapi -- packages/client/openapi.json

# ---- Build the typed client + SPA (arch-independent → build once, native) -----------
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-bookworm-slim AS web
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
FROM rust:${RUST_VERSION}-bookworm AS bin
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY packages/core ./packages/core
COPY packages/dal ./packages/dal
COPY packages/providers ./packages/providers
COPY packages/api ./packages/api
# The release profile (Cargo.toml) already strips symbols and enables LTO.
RUN cargo build --release --locked -p sure-api --bin sure-api

# ---- Runtime ------------------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# ca-certificates: outbound TLS for providers. curl: HEALTHCHECK. tini: PID-1 signal
# handling + zombie reaping (the server has no graceful-shutdown handler of its own).
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /app --create-home sure
WORKDIR /app
COPY --from=bin /build/target/release/sure-api /usr/local/bin/sure-api
COPY --from=web /build/packages/web/dist ./web

# Single-binary runtime config. BIND_ADDR is 0.0.0.0 because the app's own default,
# 127.0.0.1, is unreachable from outside the container. The SQLite DB lives under /data
# (a volume) so it survives container replacement; sure-api creates the file on start.
ENV DATABASE_URL=sqlite:/data/sure.db \
    BIND_ADDR=0.0.0.0:8080 \
    WEB_DIR=/app/web \
    RUST_LOG=info
RUN mkdir -p /data && chown sure:sure /data
USER sure
VOLUME ["/data"]
EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:8080/api/health" || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/sure-api"]
