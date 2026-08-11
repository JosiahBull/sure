//! Conditional-request support: a content-derived `ETag` on cacheable reads, and a `304
//! Not Modified` when the client already has that representation.
//!
//! This is what makes [`CachePolicy::Revalidate`](crate::cache::CachePolicy::Revalidate)
//! cheap. The browser is told to check with the server every time, but "check" costs one
//! empty 304 rather than a re-download of a transaction list or a net-worth series.
//! `ServeDir` only emits `Last-Modified`, so static files gain conditional requests here
//! too — and `Last-Modified` has one-second granularity, which an `ETag` doesn't.
//!
//! # Why the tags are weak
//!
//! Compression is layered *outside* this one, so the bytes hashed here are always the
//! identity encoding while the bytes on the wire may be brotli or gzip. A strong validator
//! must identify the exact octets sent, a weak one only needs to identify equivalent
//! representations — which is precisely the relationship between a response and its
//! compressed form, and is all `If-None-Match` on a `GET` requires.
//!
//! # Why `DefaultHasher`
//!
//! An `ETag` is an opaque token: the only requirement is that identical bodies produce
//! identical tags within and across processes, which `DefaultHasher` (fixed keys, unlike
//! `RandomState`) satisfies. It is not a cryptographic digest, and it does not need to be
//! — nothing here is authenticated by it, and the worst case for a 64-bit collision
//! between two bodies of the same length is one stale read on a private, revalidating
//! resource.

use std::hash::{Hash, Hasher};

use axum::body::{Body, HttpBody};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::cache::policy_for;

/// Headers RFC 9110 §15.4.5 says a `304` must carry over from the `200` it stands in for,
/// so a cache that stores the 304 keeps the metadata it needs.
const CARRY_OVER: &[HeaderName] = &[
    header::CACHE_CONTROL,
    header::CONTENT_LOCATION,
    header::DATE,
    header::ETAG,
    header::EXPIRES,
    header::VARY,
];

/// Attach an `ETag` to cacheable reads and answer matching `If-None-Match` with `304`.
pub async fn etag(State(max_bytes): State<usize>, request: Request, next: Next) -> Response {
    // Only a safe, cacheable read has a representation worth validating.
    let taggable = matches!(*request.method(), Method::GET | Method::HEAD)
        && policy_for(
            request.method(),
            request
                .extensions()
                .get::<MatchedPath>()
                .map(MatchedPath::as_str),
            request.uri().path(),
        )
        .cache
        .wants_etag();

    if !taggable {
        return next.run(request).await;
    }

    let if_none_match = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let response = next.run(request).await;
    if response.status() != StatusCode::OK {
        return response;
    }

    let (mut parts, body) = response.into_parts();

    // Buffer only what we already know is small enough — never discover the size by
    // accumulating it. An API response is an already-materialised `Json` value, so its
    // hint is exact; `ServeDir` streams the file but declares its length up front. If
    // neither says, the body passes through untagged.
    match declared_len(&parts, &body) {
        Some(len) if len <= max_bytes => {}
        _ => return Response::from_parts(parts, body),
    }

    let bytes = match axum::body::to_bytes(body, max_bytes).await {
        Ok(bytes) => bytes,
        Err(err) => {
            // The body was consumed by the failed read, so there is nothing to pass
            // through. `request_context` turns this into the standard scrubbed envelope.
            tracing::warn!(error = %err, "failed to buffer response body for ETag");
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            return response;
        }
    };

    let tag = weak_tag(&bytes);
    // A malformed tag would be a bug in `weak_tag`, not something a request can cause.
    if let Ok(value) = HeaderValue::from_str(&tag) {
        parts.headers.insert(header::ETAG, value);
    }

    if if_none_match.is_some_and(|candidates| matches_tag(&candidates, &tag)) {
        return not_modified(&parts);
    }

    Response::from_parts(parts, Body::from(bytes))
}

/// The response's length if it is known without reading the body.
fn declared_len(parts: &axum::http::response::Parts, body: &Body) -> Option<usize> {
    body.size_hint()
        .exact()
        .map(|len| len as usize)
        .or_else(|| {
            parts
                .headers
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
        })
}

/// `W/"<len>-<hash>"`. Mixing the length in makes a collision require two bodies that are
/// both the same length and hash-equal.
fn weak_tag(bytes: &[u8]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("W/\"{:x}-{:x}\"", bytes.len(), hasher.finish())
}

/// Compare an `If-None-Match` value against our tag.
///
/// Weak comparison per RFC 9110 §8.8.3.2: `W/` prefixes are ignored on both sides, since
/// weak validators are exactly what `If-None-Match` is defined to compare weakly.
fn matches_tag(if_none_match: &str, tag: &str) -> bool {
    let strip = |s: &str| s.trim().trim_start_matches("W/").trim().to_owned();
    let ours = strip(tag);
    if_none_match == "*"
        || if_none_match
            .split(',')
            .any(|candidate| strip(candidate) == ours)
}

/// A bodyless `304` carrying the metadata the client needs to refresh its cache entry.
fn not_modified(parts: &axum::http::response::Parts) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::NOT_MODIFIED;
    let headers = response.headers_mut();
    for name in CARRY_OVER {
        for value in parts.headers.get_all(name) {
            headers.append(name.clone(), value.clone());
        }
    }
    // Not in the RFC's list, but a CDN in front reads these the same way it reads
    // Cache-Control, and dropping them on a 304 would lose the "never store this" signal.
    for name in ["cdn-cache-control", "cloudflare-cdn-cache-control"] {
        if let Some(value) = parts.headers.get(name) {
            headers.insert(name, value.clone());
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_weak_stable_and_content_derived() {
        let tag = weak_tag(b"hello");
        assert!(tag.starts_with("W/\""), "{tag}");
        assert_eq!(tag, weak_tag(b"hello"), "same bytes must produce same tag");
        assert_ne!(tag, weak_tag(b"hellp"));
        // Length is part of the tag, so equal-hash bodies of different lengths differ.
        assert_ne!(weak_tag(b""), weak_tag(b"\0"));
    }

    #[test]
    fn if_none_match_compares_weakly() {
        let tag = weak_tag(b"body");
        assert!(matches_tag(&tag, &tag));
        assert!(matches_tag("*", &tag));
        // A cache may echo the tag back without the weakness prefix.
        assert!(matches_tag(tag.trim_start_matches("W/"), &tag));
        // Multiple candidates, ours in the middle.
        assert!(matches_tag(
            &format!("W/\"other\", {tag}, W/\"third\""),
            &tag
        ));
        assert!(!matches_tag("W/\"nope\"", &tag));
        assert!(!matches_tag("", &tag));
    }
}
