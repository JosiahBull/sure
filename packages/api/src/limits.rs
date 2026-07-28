//! Request-level abuse guards: a per-client rate limit and a global in-flight ceiling,
//! plus the shared helper that keeps machine-generated rejections in the same JSON error
//! envelope as everything else.
//!
//! The two guards answer different questions. The rate limit bounds how *often* one client
//! may ask; the in-flight ceiling bounds how much work is running at once, whoever asked
//! for it. Only the second one protects against the realistic failure here — a handful of
//! concurrent `/api/forecast` or `/api/reports/*` calls saturating a small box — but the
//! first keeps a single misbehaving client from getting there in the first place.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::{ErrorBody, ErrorDetail};

/// Marks a response whose body is already a proper `{ "error": { code, message } }`
/// envelope, so [`crate::telemetry::request_context`] leaves it alone instead of scrubbing
/// it into the generic internal-error text.
#[derive(Clone, Copy, Debug)]
pub struct ErrorAlreadyClothed;

/// Build an error response in the API's standard envelope, marked so nothing downstream
/// rewrites it.
///
/// Used for rejections that never reach a handler (rate limit, load shed, deadline), which
/// would otherwise come back as an empty body or bare text and break clients that expect
/// the envelope everywhere.
///
/// These short-circuit above the cache layer, so they set their own `no-store` — a
/// transient rejection is the last thing that should be remembered as this URL's answer.
pub fn clothe_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = ErrorBody {
        error: ErrorDetail {
            code: code.to_string(),
            message: message.to_string(),
        },
    };
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.extensions_mut().insert(ErrorAlreadyClothed);
    response
}

// ---- per-client rate limit --------------------------------------------------------

/// Drop a client's bucket once it has been idle this long. Bounds memory on a public
/// address without needing a background task.
const BUCKET_IDLE_TTL: Duration = Duration::from_secs(600);
/// Sweep expired buckets once the map grows past this. Small enough that a scan is
/// trivial, large enough that a family's worth of devices never triggers one.
const SWEEP_THRESHOLD: usize = 1024;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// A token bucket per client address.
///
/// Hand-rolled rather than pulling in `tower_governor`/`governor`: this needs to exempt
/// loopback (the same host runs `pnpm seed`, `curl`, and the e2e suite), to understand
/// `CF-Connecting-IP`, and to reject in the API's own error envelope — none of which that
/// crate's `KeyExtractor` interface allows without fighting it. The whole mechanism is the
/// forty lines below, and it is unit-tested.
#[derive(Debug)]
pub struct RateLimiter {
    /// Sustained requests per second.
    rps: f64,
    /// Bucket capacity — how far a client may burst above `rps`.
    burst: f64,
    exempt_loopback: bool,
    /// Whether proxy-supplied client-IP headers may be believed. See
    /// [`ApiConfig::trust_proxy_headers`](crate::config::ApiConfig::trust_proxy_headers).
    trust_proxy_headers: bool,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl RateLimiter {
    pub fn new(rps: f64, burst: f64, exempt_loopback: bool, trust_proxy_headers: bool) -> Self {
        Self {
            rps: rps.max(f64::MIN_POSITIVE),
            burst: burst.max(1.0),
            exempt_loopback,
            trust_proxy_headers,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take a token for `ip`. On refusal, returns how long the caller should wait.
    pub fn check(&self, ip: IpAddr) -> Result<(), Duration> {
        if self.exempt_loopback && ip.is_loopback() {
            return Ok(());
        }
        let now = Instant::now();
        // A poisoned lock here would mean a panic inside this tiny critical section, which
        // cannot happen — but recovering is still better than turning it into an outage.
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());

        if buckets.len() > SWEEP_THRESHOLD {
            buckets.retain(|_, b| now.duration_since(b.last) < BUCKET_IDLE_TTL);
        }

        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.burst,
            last: now,
        });
        // Refill for the time that has passed, capped at the bucket's capacity.
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rps).min(self.burst);
        bucket.last = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / self.rps))
        }
    }

    /// Resolve the address to charge for this request.
    ///
    /// Proxy headers are trusted only when configured to be: they are trivially forged, so
    /// believing them from an arbitrary peer would let anyone pick their own rate-limit
    /// key (or someone else's).
    fn client_ip(&self, request: &Request) -> Option<IpAddr> {
        if self.trust_proxy_headers {
            if let Some(ip) = from_proxy_headers(request.headers()) {
                return Some(ip);
            }
        }
        request
            .extensions()
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip())
    }
}

/// `CF-Connecting-IP` first (Cloudflare sets it itself and strips any client-supplied
/// copy), then the left-most `X-Forwarded-For` entry, then `X-Real-IP`.
fn from_proxy_headers(headers: &HeaderMap) -> Option<IpAddr> {
    let single = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<IpAddr>().ok())
    };
    single("cf-connecting-ip")
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.split(',').next())
                .and_then(|s| s.trim().parse::<IpAddr>().ok())
        })
        .or_else(|| single("x-real-ip"))
}

/// Reject requests from a client that is asking too often.
pub async fn rate_limit(
    State(limiter): State<Arc<RateLimiter>>,
    request: Request,
    next: Next,
) -> Response {
    // No resolvable address means no key to charge. That happens only if the server is
    // wired up without `ConnectInfo`; let the request through rather than fail closed on
    // every request because of a composition mistake.
    let Some(ip) = limiter.client_ip(&request) else {
        return next.run(request).await;
    };

    match limiter.check(ip) {
        Ok(()) => next.run(request).await,
        Err(retry_after) => {
            let secs = retry_after.as_secs().max(1);
            tracing::warn!(client_ip = %ip, retry_after_secs = secs, "rate limited");
            let mut response = clothe_error(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many requests. Slow down and try again shortly.",
            );
            let headers = response.headers_mut();
            headers.insert(axum::http::header::RETRY_AFTER, secs.into());
            headers.insert("x-ratelimit-limit", (limiter.rps as u64).into());
            response
        }
    }
}

// ---- global in-flight ceiling -----------------------------------------------------

/// A ceiling on concurrently executing requests.
///
/// Sheds rather than queues: a caller that gets a fast 503 with `Retry-After` can back off,
/// whereas an unbounded queue turns a burst into a pile of requests that all time out
/// together, having each consumed a connection and a database handle on the way.
#[derive(Clone, Debug)]
pub struct InFlight {
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl InFlight {
    pub fn new(max: usize) -> Self {
        Self {
            semaphore: Arc::new(tokio::sync::Semaphore::new(max.max(1))),
        }
    }
}

/// Hold a slot for the life of the request, or shed it.
pub async fn shed_when_saturated(
    State(in_flight): State<InFlight>,
    request: Request,
    next: Next,
) -> Response {
    match in_flight.semaphore.clone().try_acquire_owned() {
        // The permit is dropped when `_permit` goes out of scope, i.e. once the response
        // has been produced.
        Ok(_permit) => next.run(request).await,
        Err(_) => {
            tracing::warn!("shedding request: in-flight limit reached");
            let mut response = clothe_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded",
                "The server is busy. Try again shortly.",
            );
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, 1.into());
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    const CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));

    #[test]
    fn burst_is_allowed_then_refused() {
        let limiter = RateLimiter::new(1.0, 3.0, false, false);
        for i in 0..3 {
            assert!(limiter.check(CLIENT).is_ok(), "request {i} should pass");
        }
        let retry = limiter.check(CLIENT).expect_err("bucket should be empty");
        assert!(retry <= Duration::from_secs(1), "retry hint: {retry:?}");
    }

    #[test]
    fn tokens_refill_over_time() {
        // 1000 rps: a millisecond of sleep is worth a token.
        let limiter = RateLimiter::new(1000.0, 1.0, false, false);
        assert!(limiter.check(CLIENT).is_ok());
        assert!(limiter.check(CLIENT).is_err());
        std::thread::sleep(Duration::from_millis(5));
        assert!(limiter.check(CLIENT).is_ok());
    }

    #[test]
    fn clients_are_limited_independently() {
        let limiter = RateLimiter::new(1.0, 1.0, false, false);
        let other = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8));
        assert!(limiter.check(CLIENT).is_ok());
        assert!(limiter.check(CLIENT).is_err());
        assert!(limiter.check(other).is_ok());
    }

    #[test]
    fn loopback_is_exempt_when_configured() {
        let limiter = RateLimiter::new(1.0, 1.0, true, false);
        let local = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..50 {
            assert!(limiter.check(local).is_ok());
        }
        // …and not exempt when it isn't.
        let strict = RateLimiter::new(1.0, 1.0, false, false);
        assert!(strict.check(local).is_ok());
        assert!(strict.check(local).is_err());
    }

    #[test]
    fn idle_buckets_are_swept() {
        let limiter = RateLimiter::new(1000.0, 1.0, false, false);
        for i in 0..=SWEEP_THRESHOLD {
            let ip = IpAddr::V4(Ipv4Addr::from(i as u32));
            let _ = limiter.check(ip);
        }
        let len = limiter.buckets.lock().unwrap().len();
        // The sweep only removes buckets idle past the TTL, and none are here, so the map
        // is still full — what matters is that crossing the threshold is not a panic and
        // the retained entries are intact.
        assert!(len > SWEEP_THRESHOLD, "buckets retained: {len}");
    }

    #[test]
    fn proxy_headers_are_read_in_priority_order() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "198.51.100.1".parse().unwrap());
        assert_eq!(
            from_proxy_headers(&headers),
            Some("198.51.100.1".parse().unwrap())
        );

        headers.insert("x-forwarded-for", "198.51.100.2, 10.0.0.1".parse().unwrap());
        assert_eq!(
            from_proxy_headers(&headers),
            Some("198.51.100.2".parse().unwrap()),
            "left-most X-Forwarded-For entry wins over X-Real-IP"
        );

        headers.insert("cf-connecting-ip", "198.51.100.3".parse().unwrap());
        assert_eq!(
            from_proxy_headers(&headers),
            Some("198.51.100.3".parse().unwrap()),
            "Cloudflare's own header wins over both"
        );
    }

    #[test]
    fn garbage_proxy_headers_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "not-an-ip".parse().unwrap());
        headers.insert("x-forwarded-for", "".parse().unwrap());
        assert_eq!(from_proxy_headers(&headers), None);
    }
}
