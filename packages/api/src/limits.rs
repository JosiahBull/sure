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
use axum::response::Response;

// The envelope builder, its "leave this alone" marker, and the one overload response live
// in `sure_core::error` — not because the domain crate wants to know about HTTP, but because
// `AppError` has to be able to emit the *identical* 503 when the connection pool is
// exhausted or SQLite reports the write lock held. A client that had to recognise two
// shapes for "busy, come back" would get one of them wrong. Re-exported here because this
// is where the middleware that produces them lives, and every existing caller
// (`crate::cache`, `crate::telemetry`) names them through this module.
pub use crate::error::{
    clothe_error, overloaded_response, ErrorAlreadyClothed, PreservedErrorCode,
};

// ---- per-client rate limit --------------------------------------------------------

/// Drop a client's bucket once it has been idle this long. Bounds memory on a public
/// address without needing a background task.
const BUCKET_IDLE_TTL: Duration = Duration::from_secs(600);
/// Arm a sweep once the map grows past this. Small enough that a scan is trivial, large
/// enough that a family's worth of devices never triggers one.
const SWEEP_THRESHOLD: usize = 1024;
/// Minimum wall-clock gap between two sweeps.
///
/// The sweep is O(n) under the lock, and *armed* by map size — so without this, every
/// request past [`SWEEP_THRESHOLD`] walked the whole map while holding a global mutex. An
/// IPv6 client has a /64 of source addresses to spend, and a forged `X-Forwarded-For` (when
/// `trust_proxy_headers` is on) has no limit at all, so keeping the map above the threshold
/// is free for an attacker: the guard becomes the serialising bottleneck it exists to
/// prevent. Amortising it to once every 30s makes the *worst* case one scan per 30s rather
/// than one per request, and the common case still zero.
const SWEEP_MIN_INTERVAL: Duration = Duration::from_secs(30);
/// Hard ceiling on tracked clients. At ~48 bytes a bucket this is ~800KB — the bound that
/// holds between sweeps, when nothing has aged out yet. Past it a client with no bucket is
/// refused rather than admitted, which is the safe direction: an established client is
/// unaffected, and only an attacker mints keys at this rate.
const MAX_BUCKETS: usize = 16_384;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// The limiter's mutable state. One lock covers both fields because the sweep decision reads
/// the map's size and writes the timestamp; splitting them would let two threads decide to
/// sweep at once, which is the exact cost being avoided.
#[derive(Debug)]
struct Buckets {
    map: HashMap<IpAddr, Bucket>,
    /// When a sweep last ran. Starts at construction rather than at the epoch so a process
    /// that is handed 1024 distinct clients in its first second does not scan on each one.
    last_sweep: Instant,
    /// Sweeps performed. Only read by the tests, which is the cheapest way to assert "this
    /// does not run per request" — the property, not a proxy for it.
    sweeps: u64,
}

impl Buckets {
    /// Drop buckets idle past [`BUCKET_IDLE_TTL`]. The only O(n) operation here, and the
    /// only place `last_sweep` moves.
    fn sweep(&mut self, now: Instant) {
        let before = self.map.len();
        self.map
            .retain(|_, b| now.duration_since(b.last) < BUCKET_IDLE_TTL);
        self.last_sweep = now;
        self.sweeps += 1;
        if self.map.len() >= MAX_BUCKETS {
            // Nothing was idle enough to evict and the map is full: someone is minting keys
            // faster than the TTL retires them. Worth one log line per sweep (not per
            // request) because from here new clients are refused.
            tracing::warn!(
                buckets = self.map.len(),
                swept = before - self.map.len(),
                "rate-limiter bucket map is at capacity; new client addresses will be refused"
            );
        }
    }
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
    buckets: Mutex<Buckets>,
}

impl RateLimiter {
    pub fn new(rps: f64, burst: f64, exempt_loopback: bool, trust_proxy_headers: bool) -> Self {
        Self {
            rps: rps.max(f64::MIN_POSITIVE),
            burst: burst.max(1.0),
            exempt_loopback,
            trust_proxy_headers,
            buckets: Mutex::new(Buckets {
                map: HashMap::new(),
                last_sweep: Instant::now(),
                sweeps: 0,
            }),
        }
    }

    /// Take a token for `ip`. On refusal, returns how long the caller should wait.
    ///
    /// Everything here is O(1) except the housekeeping, which only an *unknown* address can
    /// trigger — a known one is a single hash lookup and some arithmetic, whatever the map's
    /// size. A new address may pay for a sweep, but at most one per
    /// [`SWEEP_MIN_INTERVAL`], and is refused outright if the map is at [`MAX_BUCKETS`] with
    /// nothing to evict.
    pub fn check(&self, ip: IpAddr) -> Result<(), Duration> {
        if self.exempt_loopback && ip.is_loopback() {
            return Ok(());
        }
        let now = Instant::now();
        // A poisoned lock here would mean a panic inside this tiny critical section, which
        // cannot happen — but recovering is still better than turning it into an outage.
        let mut buckets = self.buckets.lock().unwrap_or_else(|e| e.into_inner());

        // Two lookups on the cold path (this one and `entry` below) rather than one, because
        // only the cold path may grow the map and so only it needs the size checks.
        if !buckets.map.contains_key(&ip) {
            if buckets.map.len() >= SWEEP_THRESHOLD
                && now.duration_since(buckets.last_sweep) >= SWEEP_MIN_INTERVAL
            {
                buckets.sweep(now);
            }
            if buckets.map.len() >= MAX_BUCKETS {
                // Full, and the sweep either did not run or freed nothing. Refusing costs a
                // new client one retry; admitting would let anyone with a spare /64 grow
                // this map without bound.
                return Err(Duration::from_secs(1));
            }
        }

        let bucket = buckets.map.entry(ip).or_insert(Bucket {
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
            overloaded_response()
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
        let len = limiter.buckets.lock().unwrap().map.len();
        // The sweep only removes buckets idle past the TTL, and none are here, so the map
        // is still full — what matters is that crossing the threshold is not a panic and
        // the retained entries are intact.
        assert!(len > SWEEP_THRESHOLD, "buckets retained: {len}");
    }

    /// Fill the map with `count` addresses and pretend they were all seen `age` ago, so the
    /// next sweep has something to evict. Returns nothing: the point is the state.
    fn fill(limiter: &RateLimiter, count: usize, age: Duration) {
        let mut state = limiter.buckets.lock().unwrap();
        // `Instant` is monotonic-clock-based, so this is only unrepresentable on a machine
        // that booted seconds ago — worth saying out loud rather than filling with fresh
        // buckets and failing an unrelated assertion.
        let stale = Instant::now()
            .checked_sub(age)
            .expect("the monotonic clock should be older than BUCKET_IDLE_TTL");
        for i in 0..count {
            state.map.insert(
                IpAddr::V4(Ipv4Addr::from(i as u32)),
                Bucket {
                    tokens: 1.0,
                    last: stale,
                },
            );
        }
    }

    /// Make a sweep due right now, as if `SWEEP_MIN_INTERVAL` had elapsed.
    fn age_last_sweep(limiter: &RateLimiter) {
        let mut state = limiter.buckets.lock().unwrap();
        state.last_sweep = Instant::now()
            .checked_sub(SWEEP_MIN_INTERVAL + Duration::from_secs(1))
            .expect("the monotonic clock should be older than SWEEP_MIN_INTERVAL");
    }

    fn sweeps(limiter: &RateLimiter) -> u64 {
        limiter.buckets.lock().unwrap().sweeps
    }

    /// W-31, the whole point: past the threshold the O(n) sweep must not run on every
    /// request. It used to, under a global mutex, so anyone able to mint distinct keys (an
    /// IPv6 client, or a forged `X-Forwarded-For`) turned the limiter into the serialising
    /// bottleneck it exists to prevent.
    #[test]
    fn the_sweep_is_amortised_not_run_per_request() {
        let limiter = RateLimiter::new(1000.0, 5.0, false, false);
        fill(&limiter, SWEEP_THRESHOLD + 1, BUCKET_IDLE_TTL * 2);
        age_last_sweep(&limiter);

        // The first unknown address pays for the sweep, and it clears the idle entries.
        let first = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));
        assert!(limiter.check(first).is_ok());
        assert_eq!(sweeps(&limiter), 1);
        assert_eq!(limiter.buckets.lock().unwrap().map.len(), 1);

        // Now put it back over the threshold and hammer it with fresh keys. Under the old
        // code every one of these would have walked the whole map.
        fill(&limiter, SWEEP_THRESHOLD + 1, BUCKET_IDLE_TTL * 2);
        for i in 0..500u32 {
            let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, (i % 200) as u8 + 2));
            let _ = limiter.check(ip);
        }
        assert_eq!(
            sweeps(&limiter),
            1,
            "no second sweep is due for another SWEEP_MIN_INTERVAL"
        );

        // …and when the interval has passed, exactly one more happens.
        age_last_sweep(&limiter);
        let _ = limiter.check(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 250)));
        assert_eq!(sweeps(&limiter), 2);
    }

    /// A *known* client never triggers housekeeping, however large the map is — the hot path
    /// stays one hash lookup.
    #[test]
    fn an_established_client_never_pays_for_a_sweep() {
        let limiter = RateLimiter::new(1000.0, 100.0, false, false);
        assert!(limiter.check(CLIENT).is_ok());
        fill(&limiter, SWEEP_THRESHOLD + 1, BUCKET_IDLE_TTL * 2);
        age_last_sweep(&limiter);
        for _ in 0..50 {
            assert!(limiter.check(CLIENT).is_ok());
        }
        assert_eq!(sweeps(&limiter), 0);
    }

    /// The bound that holds *between* sweeps: with nothing idle to evict, the map stops
    /// growing and unknown addresses are refused with a retry hint. Established clients keep
    /// being served, which is the direction that matters — only an attacker mints keys this
    /// fast.
    #[test]
    fn the_bucket_map_is_capped() {
        let limiter = RateLimiter::new(1000.0, 100.0, false, false);
        assert!(limiter.check(CLIENT).is_ok());
        // Fresh (not idle), so a sweep cannot free anything. One short of the cap, which
        // `CLIENT`'s own bucket brings up to exactly it.
        fill(&limiter, MAX_BUCKETS - 1, Duration::ZERO);
        age_last_sweep(&limiter);
        assert_eq!(limiter.buckets.lock().unwrap().map.len(), MAX_BUCKETS);

        let newcomer = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));
        let retry = limiter
            .check(newcomer)
            .expect_err("a full map must refuse an address it has no room to track");
        assert!(retry <= Duration::from_secs(1), "retry hint: {retry:?}");
        let len = limiter.buckets.lock().unwrap().map.len();
        assert!(len <= MAX_BUCKETS, "map grew past the cap: {len}");
        // The sweep ran once (and freed nothing); the refusal itself is not a second sweep.
        assert_eq!(sweeps(&limiter), 1);
        assert!(limiter.check(newcomer).is_err());
        assert_eq!(sweeps(&limiter), 1);
        // The client that was already known is unaffected.
        assert!(limiter.check(CLIENT).is_ok());
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

    /// The shed path's response is now built by `sure_core::error::overloaded_response`,
    /// which `AppError` also uses for an exhausted connection pool (W-18). Pin the shape
    /// here so the two can't drift apart: it is a client-visible contract.
    #[test]
    fn the_overload_response_is_a_503_with_retry_after() {
        let response = overloaded_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::RETRY_AFTER)
                .unwrap(),
            "1"
        );
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .unwrap(),
            "no-store"
        );
        // Marked, so `telemetry::request_context` leaves the `overloaded` code alone rather
        // than scrubbing this 5xx into a generic `internal`.
        assert!(response.extensions().get::<ErrorAlreadyClothed>().is_some());
    }
}
