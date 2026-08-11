//! Per-route cache policy and request deadline.
//!
//! One table, [`policy_for`], classifies every request; two thin middlewares consume it —
//! [`cache_control`] stamps the response headers and [`timeout`] bounds how long a handler
//! may run. Keeping both in one table means a new route can't accidentally get a sensible
//! deadline but a nonsense cache policy (or vice versa), and it fails *closed*: anything
//! not named here is `no-store` with the ordinary deadline.
//!
//! # Why `private` everywhere under `/api`
//!
//! The API has no authentication — a URL is enough to read the household's finances. A
//! shared cache must therefore never keep an API response. `Cache-Control: private`
//! already says so, but Cloudflare lets a dashboard "Cache Everything" rule override
//! ordinary cache directives, so we additionally emit the RFC 9213 targeted directives
//! `CDN-Cache-Control` and `Cloudflare-CDN-Cache-Control` (the latter wins over the former
//! on Cloudflare) with `no-store`. Those are only honoured by a CDN, so they cost two
//! headers and nothing else on a LAN deployment.
//!
//! # Why reports aren't given a `max-age`
//!
//! `/api/reports/*` is the expensive part of the API, and it is tempting to let the
//! browser hold results for a minute. It would also mean editing a transaction and then
//! seeing stale numbers, which is worse than the cost. They get revalidation instead: the
//! ETag layer turns an unchanged report into a 304, saving the payload but not the
//! recompute. Cutting the recompute needs a server-side cache invalidated by every
//! mutation, which is a bigger change than this one.

use std::time::Duration;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::limits::clothe_error;

/// One year, the maximum any cache should be asked to hold something.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Named, non-content-addressed static files: always ask, usually get a 304.
const STATIC_REVALIDATE: &str = "public, max-age=0, must-revalidate";
/// Static and effectively stable (icons, fonts) but without a content hash in the name.
const STATIC_WINDOW: &str = "public, max-age=86400, stale-while-revalidate=604800";
const NO_STORE: &str = "no-store";
const NO_CACHE: &str = "no-cache";
/// Private data the browser must revalidate on every use.
const PRIVATE_REVALIDATE: &str = "private, no-cache";

/// How a response may be cached, by the browser and by a CDN respectively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachePolicy {
    /// Never stored anywhere. Mutations, liveness, live upstream lookups, the full config
    /// dump, and every error response.
    NoStore,
    /// Private, revalidated on every use. The default for API reads: paired with the ETag
    /// layer this costs one conditional request and usually returns an empty 304.
    Revalidate,
    /// Private, reusable by the browser for a bounded window. The payload is expensive to
    /// produce and a slightly stale answer is harmless. Carries the full directive string.
    PrivateWindow(&'static str),
    /// Content-addressed asset — the name changes when the bytes do.
    Immutable,
    /// Static, stable name, must be revalidated (the SPA shell and the service worker).
    StaticRevalidate,
    /// Static, stable name, safe to reuse for a day (icons, fonts).
    StaticWindow,
}

impl CachePolicy {
    /// `(Cache-Control, CDN-Cache-Control)` for this policy.
    fn directives(self) -> (&'static str, &'static str) {
        match self {
            CachePolicy::NoStore => (NO_STORE, NO_STORE),
            // A CDN must not hold private data even for the revalidation window.
            CachePolicy::Revalidate => (PRIVATE_REVALIDATE, NO_STORE),
            CachePolicy::PrivateWindow(cc) => (cc, NO_STORE),
            CachePolicy::Immutable => (IMMUTABLE, IMMUTABLE),
            CachePolicy::StaticRevalidate => (STATIC_REVALIDATE, NO_CACHE),
            CachePolicy::StaticWindow => (STATIC_WINDOW, "public, max-age=86400"),
        }
    }

    /// Whether a response under this policy is worth giving an `ETag`. There is no point
    /// hashing something no cache is allowed to keep.
    pub fn wants_etag(self) -> bool {
        !matches!(self, CachePolicy::NoStore)
    }
}

/// Which deadline a route gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deadline {
    /// The ordinary request timeout.
    Normal,
    /// For work that legitimately takes minutes: bank round trips, bulk imports,
    /// historical backfills, whole-ledger rule runs.
    Long,
}

/// The full policy for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoutePolicy {
    pub cache: CachePolicy,
    pub deadline: Deadline,
}

/// One route template *and verb* whose handler can legitimately run for minutes.
///
/// The verb is not decoration. A template can carry a cheap method alongside an expensive one,
/// because axum mounts them on a single `MethodRouter`, and keying the deadline on the path
/// alone would hand the cheap one the 300s allowance too — so a wedged one-statement request
/// holds an in-flight slot for five minutes instead of being cut loose at the ordinary timeout.
/// That is the same mismatch the raised body limit avoids by being layered onto `post(..)`
/// rather than onto the route.
///
/// Import is the case that taught this, and it no longer needs the protection: undo now lives at
/// its own template (`/api/import/{account_id}/{source}`) rather than sharing the upload's, so
/// it cannot inherit anything. The verb key stays because the next route to pair a bulk `POST`
/// with a cheap `DELETE` will get it for free.
///
/// Each entry's method is the one in `crate::routes`' `.route(..)` call for that template; a
/// method not listed here gets [`Deadline::Normal`], which is the safe direction to fail.
struct LongRoute {
    method: Method,
    template: &'static str,
}

/// Route templates whose handlers can legitimately run for minutes, per verb.
const LONG_ROUTES: &[LongRoute] = &[
    // A live round trip to the bank per linked account, then a write per transaction.
    LongRoute {
        method: Method::POST,
        template: "/api/providers/{id}/sync",
    },
    // The one long *read* that waits on someone else: `discover_accounts` calls the upstream
    // provider inline to enumerate what is linkable, so it waits on their API, not on SQLite.
    LongRoute {
        method: Method::GET,
        template: "/api/provider-kinds/{kind}/accounts",
    },
    // …and the one long read that waits on us. A 30-year projection is 2000 paths × 360 months
    // across every account and category — bounded by `MAX_PATH_MONTHS`, but still seconds of
    // uninterrupted CPU on the blocking pool. It ran comfortably inside the normal deadline
    // only while the horizon was capped at five years. Its `PrivateWindow` cache policy is a
    // separate question and unchanged: this is how long one may take, not how long the answer
    // stays good for.
    LongRoute {
        method: Method::GET,
        template: "/api/forecast",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/accounts/{id}/brokerage/backfill",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/accounts/{id}/brokerage/revalue",
    },
    // Every file upload, whatever it holds. Seven years of an everyday account is a few
    // thousand rows, each its own insert; a myIR zip is several workbooks read in full before a
    // row is written. The `DELETE` that undoes an import is a separate template on purpose, so
    // it cannot inherit this — see `LongRoute`.
    LongRoute {
        method: Method::POST,
        template: "/api/import",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/config/import",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/rules/run",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/rules/{id}/run",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/rules/runs/{run_id}/undo",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/crons/run",
    },
    LongRoute {
        method: Method::POST,
        template: "/api/crons/{id}/run",
    },
];

/// Which deadline `(method, template)` earns.
///
/// `HEAD` is folded onto `GET` because axum's `get(handler)` also answers `HEAD` with that
/// same handler and merely drops the body — the expensive work still happens, so a `HEAD` on
/// a long read must get the long deadline or it is guaranteed to time out.
fn deadline_for(method: &Method, template: &str) -> Deadline {
    let head = *method == Method::HEAD;
    let long = LONG_ROUTES.iter().any(|r| {
        r.template == template && (r.method == *method || (head && r.method == Method::GET))
    });
    if long {
        Deadline::Long
    } else {
        Deadline::Normal
    }
}

/// API reads that must never be stored: liveness, a live upstream call, and the full
/// database dump.
const API_NO_STORE: &[&str] = &[
    "/api/health",
    "/api/config/export",
    "/api/provider-kinds/{kind}/accounts",
];

/// Classify a request.
///
/// `matched_path` is the low-cardinality route template axum resolved (e.g.
/// `/api/accounts/{id}`), available to `Router::layer` middleware via request extensions.
/// It is absent for the static-file fallback, where `uri_path` is used instead.
pub fn policy_for(method: &Method, matched_path: Option<&str>, uri_path: &str) -> RoutePolicy {
    // Keyed on the verb as well as the template: a cheap method sharing a path with an
    // expensive one keeps the ordinary deadline. No matched path means the static-file
    // fallback, which is never long.
    let deadline = matched_path.map_or(Deadline::Normal, |p| deadline_for(method, p));

    // Anything that changes state is never cacheable, whatever it returns.
    if !matches!(*method, Method::GET | Method::HEAD) {
        return RoutePolicy {
            cache: CachePolicy::NoStore,
            deadline,
        };
    }

    let path = matched_path.unwrap_or(uri_path);
    let cache = if path.starts_with("/api/") || path == "/api" {
        api_policy(path)
    } else {
        static_policy(uri_path)
    };
    RoutePolicy { cache, deadline }
}

fn api_policy(template: &str) -> CachePolicy {
    if API_NO_STORE.contains(&template) {
        return CachePolicy::NoStore;
    }
    match template {
        // A fresh Monte Carlo draw per call is seconds of CPU, and the projection is a
        // long-horizon estimate — a minute-old one is not meaningfully different.
        "/api/forecast" => {
            CachePolicy::PrivateWindow("private, max-age=60, stale-while-revalidate=300")
        }
        // Backed by an end-of-day price cache that a scheduled task refreshes; re-asking
        // within five minutes cannot produce a different answer.
        "/api/accounts/{id}/stock-price" => {
            CachePolicy::PrivateWindow("private, max-age=300, stale-while-revalidate=1500")
        }
        _ => CachePolicy::Revalidate,
    }
}

fn static_policy(path: &str) -> CachePolicy {
    // Vite fingerprints everything under /assets, and vite-plugin-pwa fingerprints the
    // workbox runtime — the name changes whenever the bytes do.
    if path.starts_with("/assets/")
        || (path.starts_with("/workbox-") && path.ends_with(".js"))
        || path.starts_with("/fonts/")
    {
        // Fonts are the exception: they are copied through verbatim, so the name is stable
        // even when the file changes. They get a day rather than a year.
        return if path.starts_with("/fonts/") {
            CachePolicy::StaticWindow
        } else {
            CachePolicy::Immutable
        };
    }
    if path == "/favicon.svg" || (path.starts_with("/icon-") && path.ends_with(".png")) {
        return CachePolicy::StaticWindow;
    }
    // The SPA shell, the service worker and its registration shim, the manifest, and every
    // client-side route that falls back to index.html. Caching a stale service worker or
    // shell is how a deploy fails to reach the device, so these always revalidate.
    CachePolicy::StaticRevalidate
}

/// Stamp `Cache-Control` (and, when enabled, the CDN-targeted equivalents) onto the
/// response, unless the handler already set one.
///
/// Runs on the way out, so it can downgrade error responses to `no-store` regardless of
/// what the route's policy says.
pub async fn cache_control(State(cdn): State<bool>, request: Request, next: Next) -> Response {
    let policy = policy_for(
        request.method(),
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
        request.uri().path(),
    );

    let mut response = next.run(request).await;

    if response.headers().contains_key(header::CACHE_CONTROL) {
        return response;
    }

    // An error is never a cacheable representation of the resource.
    let cache = if response.status().is_client_error() || response.status().is_server_error() {
        CachePolicy::NoStore
    } else {
        policy.cache
    };
    let (browser, cdn_directive) = cache.directives();

    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(browser));
    if cdn {
        headers.insert("cdn-cache-control", HeaderValue::from_static(cdn_directive));
        // Cloudflare gives its own vendor header precedence over `CDN-Cache-Control`, so
        // set both rather than relying on which one the edge happens to read.
        headers.insert(
            "cloudflare-cdn-cache-control",
            HeaderValue::from_static(cdn_directive),
        );
    }
    response
}

/// Deadlines, chosen per route from the same table as the cache policy.
#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    pub normal: Duration,
    pub long: Duration,
}

/// Abandon a request that outruns its deadline.
///
/// Dropping the handler future cancels whatever it was awaiting (the SQL query is rolled
/// back and its connection returns to the pool), which is the point: a stuck request must
/// not hold an in-flight slot forever.
pub async fn timeout(State(deadlines): State<Deadlines>, request: Request, next: Next) -> Response {
    let policy = policy_for(
        request.method(),
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
        request.uri().path(),
    );
    let limit = match policy.deadline {
        Deadline::Normal => deadlines.normal,
        Deadline::Long => deadlines.long,
    };

    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                timeout_secs = limit.as_secs(),
                "request exceeded its deadline"
            );
            clothe_error(
                StatusCode::REQUEST_TIMEOUT,
                "timeout",
                &format!("Request exceeded its {}s deadline.", limit.as_secs()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_of(method: Method, matched: Option<&str>, path: &str) -> CachePolicy {
        policy_for(&method, matched, path).cache
    }

    #[test]
    fn mutations_are_never_cacheable() {
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::PATCH] {
            assert_eq!(
                cache_of(method, Some("/api/accounts"), "/api/accounts"),
                CachePolicy::NoStore
            );
        }
    }

    #[test]
    fn api_reads_default_to_private_revalidation() {
        assert_eq!(
            cache_of(Method::GET, Some("/api/accounts/{id}"), "/api/accounts/7"),
            CachePolicy::Revalidate
        );
        assert_eq!(
            cache_of(
                Method::GET,
                Some("/api/reports/net-worth"),
                "/api/reports/net-worth"
            ),
            CachePolicy::Revalidate
        );
        // Unknown API routes fail closed onto the same private policy, never onto a
        // public/CDN-cacheable one.
        assert_eq!(
            cache_of(
                Method::GET,
                Some("/api/something-new"),
                "/api/something-new"
            ),
            CachePolicy::Revalidate
        );
    }

    #[test]
    fn sensitive_and_live_reads_are_no_store() {
        for path in API_NO_STORE {
            assert_eq!(
                cache_of(Method::GET, Some(path), path),
                CachePolicy::NoStore
            );
        }
    }

    #[test]
    fn expensive_reads_get_a_private_window() {
        assert!(matches!(
            cache_of(Method::GET, Some("/api/forecast"), "/api/forecast"),
            CachePolicy::PrivateWindow(_)
        ));
        assert!(matches!(
            cache_of(
                Method::GET,
                Some("/api/accounts/{id}/stock-price"),
                "/api/accounts/3/stock-price"
            ),
            CachePolicy::PrivateWindow(_)
        ));
    }

    #[test]
    fn no_api_policy_is_cdn_cacheable() {
        for policy in [
            api_policy("/api/accounts"),
            api_policy("/api/forecast"),
            api_policy("/api/health"),
            api_policy("/api/config/export"),
        ] {
            assert_eq!(policy.directives().1, NO_STORE);
        }
    }

    #[test]
    fn static_assets_are_classified_by_name() {
        assert_eq!(
            cache_of(Method::GET, None, "/assets/index-Bl2CtK_1.js"),
            CachePolicy::Immutable
        );
        assert_eq!(
            cache_of(Method::GET, None, "/workbox-abeb32eb.js"),
            CachePolicy::Immutable
        );
        // Not fingerprinted by the build, so it must stay revalidated.
        assert_eq!(
            cache_of(Method::GET, None, "/sw.js"),
            CachePolicy::StaticRevalidate
        );
        assert_eq!(
            cache_of(Method::GET, None, "/registerSW.js"),
            CachePolicy::StaticRevalidate
        );
        assert_eq!(
            cache_of(Method::GET, None, "/index.html"),
            CachePolicy::StaticRevalidate
        );
        assert_eq!(
            cache_of(Method::GET, None, "/manifest.webmanifest"),
            CachePolicy::StaticRevalidate
        );
        // A client-side route falling back to the shell.
        assert_eq!(
            cache_of(Method::GET, None, "/transactions"),
            CachePolicy::StaticRevalidate
        );
        assert_eq!(
            cache_of(Method::GET, None, "/favicon.svg"),
            CachePolicy::StaticWindow
        );
        assert_eq!(
            cache_of(Method::GET, None, "/icon-512.png"),
            CachePolicy::StaticWindow
        );
        // Copied through verbatim, so the name doesn't change with the bytes.
        assert_eq!(
            cache_of(Method::GET, None, "/fonts/Geist[wght].woff2"),
            CachePolicy::StaticWindow
        );
    }

    #[test]
    fn long_routes_get_the_long_deadline() {
        assert_eq!(
            policy_for(
                &Method::POST,
                Some("/api/providers/{id}/sync"),
                "/api/providers/1/sync"
            )
            .deadline,
            Deadline::Long
        );
        assert_eq!(
            policy_for(&Method::GET, Some("/api/accounts"), "/api/accounts").deadline,
            Deadline::Normal
        );
    }

    /// Every entry must still resolve for the verb it was written for — a typo in a template
    /// (or a route renamed without updating this table) silently downgrades a minutes-long
    /// handler to the ordinary deadline, which is a timeout nobody can explain.
    #[test]
    fn every_long_route_resolves_for_its_own_method() {
        for entry in LONG_ROUTES {
            assert_eq!(
                policy_for(&entry.method, Some(entry.template), entry.template).deadline,
                Deadline::Long,
                "{} {}",
                entry.method,
                entry.template
            );
        }
    }

    /// Every file upload gets the long deadline — which is now one entry rather than four that
    /// had to agree with each other. They didn't: `/api/accounts/{id}/student-loan/import` was
    /// never added, so a myIR zip had 30 seconds where the same-sized ASB zip beside it had 300,
    /// and `every_long_route_resolves_for_its_own_method` could not catch it because it only
    /// checks the entries that *are* in the table. One route, one entry, and the disagreement has
    /// nowhere left to live.
    #[test]
    fn the_file_upload_import_gets_the_long_deadline() {
        assert_eq!(
            policy_for(&Method::POST, Some("/api/import"), "/api/import").deadline,
            Deadline::Long
        );
    }

    /// The W-36 remainder, restated for the shape it has now: undoing an import is one
    /// statement, and must not get the upload's five-minute allowance. It is a separate
    /// template, so this holds structurally — but the assertion is what would catch someone
    /// folding the two back onto one route.
    #[test]
    fn import_undo_keeps_the_normal_deadline_while_the_upload_stays_long() {
        assert_eq!(
            policy_for(&Method::POST, Some("/api/import"), "/api/import").deadline,
            Deadline::Long
        );
        assert_eq!(
            policy_for(
                &Method::DELETE,
                Some("/api/import/{account_id}/{source}"),
                "/api/import/4/asb_csv"
            )
            .deadline,
            Deadline::Normal
        );
    }

    /// A verb the table doesn't name gets the ordinary deadline even on a long path — the
    /// safe direction. `GET` on a POST-only import template is what a stray browser fetch or
    /// a probe looks like, and it has no business holding a slot for minutes.
    #[test]
    fn other_methods_on_a_long_path_are_normal() {
        for method in [Method::GET, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(
                policy_for(&method, Some("/api/config/import"), "/api/config/import").deadline,
                Deadline::Normal,
                "{method}"
            );
        }
        // ...and, mirrored: the one long read is long for GET but not for a mutation.
        let discover = "/api/provider-kinds/{kind}/accounts";
        assert_eq!(
            policy_for(
                &Method::GET,
                Some(discover),
                "/api/provider-kinds/akahu/accounts"
            )
            .deadline,
            Deadline::Long
        );
        assert_eq!(
            policy_for(
                &Method::POST,
                Some(discover),
                "/api/provider-kinds/akahu/accounts"
            )
            .deadline,
            Deadline::Normal
        );
    }

    /// axum answers `HEAD` with the `GET` handler, so the work — and therefore the deadline —
    /// has to be the same.
    #[test]
    fn head_inherits_the_get_deadline() {
        let discover = "/api/provider-kinds/{kind}/accounts";
        assert_eq!(
            policy_for(
                &Method::HEAD,
                Some(discover),
                "/api/provider-kinds/akahu/accounts"
            )
            .deadline,
            Deadline::Long
        );
    }

    /// The static-file fallback has no matched path and is never long.
    #[test]
    fn the_static_fallback_is_never_long() {
        assert_eq!(
            policy_for(&Method::GET, None, "/api/config/import").deadline,
            Deadline::Normal
        );
    }
}
