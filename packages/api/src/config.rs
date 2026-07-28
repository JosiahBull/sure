//! Tunables for the HTTP boundary: cache-header emission, compression, CORS, and every
//! request-level abuse guard.
//!
//! These are plain data with sensible defaults. **No environment parsing happens here** —
//! reading the environment is a concern of *running* the server, so `sure-server`'s
//! `Config` owns it and hands the result to [`build_app`](crate::build_app). That keeps
//! `sure-api` a library that a test (or a future embedder) can configure directly.

use std::time::Duration;

/// The origins allowed to make cross-origin browser requests when nothing is configured.
///
/// The API has **no authentication**, so a permissive CORS policy would let any page the
/// user happens to visit read their entire financial history out of the browser. The
/// deployed hostname plus the Vite dev server are the only origins that legitimately need
/// cross-origin access; the SPA itself is same-origin in both dev (Vite proxies `/api`)
/// and production (`WEB_DIR`), so it never relies on CORS at all.
pub const DEFAULT_CORS_ORIGINS: &[&str] = &[
    "https://sure.bullfamilies.com",
    "http://localhost:5173",
    "http://127.0.0.1:5173",
];

/// Request-level limits. Each has a working default, so an unconfigured deployment is
/// still protected.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Global request-body ceiling, applied to every route that doesn't override it.
    /// Matches axum's own default.
    pub max_body_bytes: usize,
    /// Ceiling for `POST /api/config/import`. Larger than the global one because the
    /// matching export is a full database dump — a 2 MB cap makes the round trip fail.
    pub max_snapshot_body_bytes: usize,
    /// Ceiling for `POST /api/accounts/{id}/brokerage/import` (a Sharesies export zip).
    pub max_import_body_bytes: usize,
    /// Deadline for an ordinary request.
    pub request_timeout: Duration,
    /// Deadline for the handful of routes that legitimately take minutes: a provider sync
    /// (network round trips to a bank), a brokerage import, a historical backfill.
    pub long_request_timeout: Duration,
    /// How many requests may be in flight before new ones are shed with 503. Bounds CPU
    /// and memory when something expensive (reports, the Monte Carlo forecast) is hammered.
    pub max_in_flight: usize,
    /// Sustained per-client request rate.
    pub rate_limit_rps: f64,
    /// How many requests a client may burst above [`Self::rate_limit_rps`].
    pub rate_limit_burst: f64,
    /// Skip rate limiting for loopback peers. On by default: this is a LAN app you also
    /// drive from the same host with `curl`, `pnpm seed`, and the e2e suite.
    pub rate_limit_exempt_loopback: bool,
    /// Largest response the ETag middleware will buffer in order to hash it. Bigger
    /// responses stream through untagged rather than being held in memory.
    pub max_etag_body_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: 2 * 1024 * 1024,
            max_snapshot_body_bytes: 32 * 1024 * 1024,
            max_import_body_bytes: 50 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            long_request_timeout: Duration::from_secs(300),
            max_in_flight: 64,
            rate_limit_rps: 50.0,
            rate_limit_burst: 200.0,
            rate_limit_exempt_loopback: true,
            max_etag_body_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Everything [`build_app`](crate::build_app) needs beyond the state and the SPA directory.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub limits: Limits,
    /// Allowed cross-origin request origins. Empty disables CORS entirely (same-origin
    /// only), which is the correct posture for the single-binary deployment.
    pub cors_allowed_origins: Vec<String>,
    /// Emit `CDN-Cache-Control` / `Cloudflare-CDN-Cache-Control` alongside `Cache-Control`.
    /// Inert unless a CDN is in front, but they are what stops an over-broad "Cache
    /// Everything" rule from publishing private data, so they default on.
    pub cdn_cache_headers: bool,
    /// Trust `CF-Connecting-IP` / `X-Forwarded-For` / `X-Real-IP` when identifying the
    /// client for rate limiting. **Only safe when the peer is a proxy you control** —
    /// otherwise any client can forge its own rate-limit key. Off by default.
    pub trust_proxy_headers: bool,
    /// Compress responses that ask for it.
    pub compression: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            cors_allowed_origins: DEFAULT_CORS_ORIGINS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            cdn_cache_headers: true,
            trust_proxy_headers: false,
            compression: true,
        }
    }
}
