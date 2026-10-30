//! What this process saw of the request that asked, for telling which path a device's requests
//! take to reach it. The SPA's hidden connection page (`packages/web/src/pages/Debug.svelte`)
//! is the caller.
//!
//! The API has no authentication, so this endpoint echoes back only what the requester itself
//! sent, or what a proxy in front of the process added to it, and only from a whitelist
//! ([`ECHOED`] and [`is_echoed`]). No cookie, no `Authorization`, no Cloudflare Access
//! assertion, and nothing about the process: its environment, its files and its configuration
//! never appear here.
//!
//! The HTTP version is the one *this hop* used. Behind a reverse proxy that is the proxy's
//! connection to the origin, not the browser's to the proxy; the browser's own leg is
//! `nextHopProtocol` in its Resource Timing, or [`FORWARDED_HTTP_VERSION`] when the proxy is
//! configured to pass it on.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, Version};
use axum::{Router, routing::get};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::extract::Json;
use crate::state::AppState;

// OTEL span names for this module's handlers.
const META_DEBUG_REQUEST: &str = "meta.debug_request";

/// A request header a reverse proxy can set to the protocol the *browser* used to reach it,
/// which this process cannot otherwise see. Nothing sets it by default; in nginx it is
/// `proxy_set_header X-Forwarded-Http-Version $server_protocol;`.
pub const FORWARDED_HTTP_VERSION: &str = "x-forwarded-http-version";

/// The request headers echoed back, in the order they are listed. Everything else is dropped,
/// apart from Cloudflare's (see [`is_echoed`]).
const ECHOED: &[&str] = &[
    "host",
    "forwarded",
    "x-forwarded-for",
    "x-real-ip",
    "x-forwarded-proto",
    "x-forwarded-scheme",
    "x-forwarded-host",
    "x-forwarded-port",
    FORWARDED_HTTP_VERSION,
    "via",
    "user-agent",
    "accept-encoding",
];

/// Whether a header is echoed: the [`ECHOED`] names and Cloudflare's `CF-*` request headers,
/// except `CF-Access-*`. Those carry a Cloudflare Access JWT, the signed-in user's email, or a
/// service token's client secret, so they are credentials rather than metadata about the path.
fn is_echoed(name: &str) -> bool {
    ECHOED.contains(&name) || (name.starts_with("cf-") && !name.starts_with("cf-access-"))
}

/// Which kind of network an address belongs to. This is the in-app evidence of the path a
/// request took: a tailnet address means it came over Tailscale, a private one means the LAN.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NetworkClass {
    /// `127.0.0.0/8` or `::1`: the same machine.
    Loopback,
    /// `100.64.0.0/10`, the shared address space Tailscale assigns from, or its IPv6 prefix
    /// `fd7a:115c:a1e0::/48`.
    Tailscale,
    /// RFC 1918 (`10/8`, `172.16/12`, `192.168/16`), link-local, or an IPv6 unique-local or
    /// link-local address.
    Lan,
    /// Anything else.
    Public,
}

impl NetworkClass {
    /// Classify an address. An IPv4-mapped IPv6 address is classified as the IPv4 address it
    /// carries, which is how a dual-stack listener reports an IPv4 peer.
    pub fn of(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::of_v4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::of_v4(v4),
                None => Self::of_v6(v6),
            },
        }
    }

    fn of_v4(ip: Ipv4Addr) -> Self {
        let [a, b, _, _] = ip.octets();
        if ip.is_loopback() {
            Self::Loopback
        } else if a == 100 && (b & 0b1100_0000) == 64 {
            Self::Tailscale
        } else if ip.is_private() || ip.is_link_local() {
            Self::Lan
        } else {
            Self::Public
        }
    }

    fn of_v6(ip: Ipv6Addr) -> Self {
        let [s0, s1, s2, ..] = ip.segments();
        if ip.is_loopback() {
            Self::Loopback
        } else if (s0, s1, s2) == (0xfd7a, 0x115c, 0xa1e0) {
            // Inside `fc00::/7`, so it has to be tested before the unique-local check.
            Self::Tailscale
        } else if ip.is_unique_local() || ip.is_unicast_link_local() {
            Self::Lan
        } else {
            Self::Public
        }
    }
}

/// Where an address in [`RequestEcho::addresses`] was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressSource {
    /// `CF-Connecting-IP`.
    CfConnectingIp,
    /// One hop of `X-Forwarded-For`.
    XForwardedFor,
    /// `X-Real-IP`.
    XRealIp,
    /// The TCP connection itself. Behind a reverse proxy, this is the proxy.
    Peer,
}

/// One address the request carried, and the kind of network it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ObservedAddress {
    pub source: AddressSource,
    pub ip: String,
    pub network: NetworkClass,
}

/// One echoed request header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EchoedHeader {
    /// Lower-case, as HTTP/2 sends it.
    pub name: String,
    pub value: String,
}

/// What this process saw of the request that asked for it.
#[derive(Debug, Serialize, ToSchema)]
pub struct RequestEcho {
    /// The protocol of the connection that reached this process, as hyper names it
    /// (`HTTP/1.1`, `HTTP/2.0`). Behind a reverse proxy, that is the proxy's connection.
    pub http_version: String,
    /// The requester, as far as this process can tell: the first of [`Self::addresses`].
    pub client: Option<ObservedAddress>,
    /// Every address the request carried, in the order the rate limiter believes them when
    /// `TRUST_PROXY_HEADERS` is on: `CF-Connecting-IP`, each `X-Forwarded-For` hop from the
    /// left, `X-Real-IP`, then the TCP peer. A value that is not an IP address is left out
    /// here and still appears among the headers.
    pub addresses: Vec<ObservedAddress>,
    /// The whitelisted request headers, in a fixed order.
    pub headers: Vec<EchoedHeader>,
    /// This process's clock when it answered, RFC 3339 in UTC to the millisecond.
    pub server_time: String,
    /// The same instant in milliseconds since the Unix epoch, for working out clock skew.
    pub server_time_ms: i64,
    /// This build's version.
    pub version: String,
}

impl RequestEcho {
    fn of(
        version: Version,
        headers: &HeaderMap,
        peer: Option<SocketAddr>,
        now: DateTime<Utc>,
    ) -> Self {
        let addresses = addresses(headers, peer);
        Self {
            http_version: format!("{version:?}"),
            client: addresses.first().cloned(),
            addresses,
            headers: echoed_headers(headers),
            server_time: now.to_rfc3339_opts(SecondsFormat::Millis, true),
            server_time_ms: now.timestamp_millis(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// An address as a proxy writes it: bare, or with a port (`192.0.2.1:443`, `[2001:db8::1]:443`).
fn parse_ip(raw: &str) -> Option<IpAddr> {
    let raw = raw.trim();
    raw.parse::<IpAddr>()
        .ok()
        .or_else(|| raw.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
}

fn observed(source: AddressSource, ip: IpAddr) -> ObservedAddress {
    ObservedAddress {
        source,
        ip: ip.to_string(),
        network: NetworkClass::of(ip),
    }
}

fn addresses(headers: &HeaderMap, peer: Option<SocketAddr>) -> Vec<ObservedAddress> {
    let values = |name: &'static str| {
        headers
            .get_all(name)
            .into_iter()
            .filter_map(|value| value.to_str().ok())
    };
    let mut found = Vec::new();
    for (name, source) in [
        ("cf-connecting-ip", AddressSource::CfConnectingIp),
        ("x-forwarded-for", AddressSource::XForwardedFor),
        ("x-real-ip", AddressSource::XRealIp),
    ] {
        for value in values(name) {
            found.extend(
                value
                    .split(',')
                    .filter_map(parse_ip)
                    .map(|ip| observed(source, ip)),
            );
        }
    }
    found.extend(peer.map(|addr| observed(AddressSource::Peer, addr.ip())));
    found
}

fn echoed_headers(headers: &HeaderMap) -> Vec<EchoedHeader> {
    let header = |name: &str, value: &axum::http::HeaderValue| EchoedHeader {
        name: name.to_string(),
        value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
    };
    let mut echoed: Vec<EchoedHeader> = ECHOED
        .iter()
        .flat_map(|name| headers.get_all(*name).iter().map(|v| header(name, v)))
        .collect();
    // Cloudflare's, after the fixed list and sorted, so the order does not depend on the
    // order the proxy happened to send them in.
    let mut cloudflare: Vec<EchoedHeader> = headers
        .iter()
        .filter(|(name, _)| !ECHOED.contains(&name.as_str()) && is_echoed(name.as_str()))
        .map(|(name, value)| header(name.as_str(), value))
        .collect();
    cloudflare.sort_by(|a, b| a.name.cmp(&b.name));
    echoed.extend(cloudflare);
    echoed
}

/// What this process saw of this request: the HTTP version, the addresses it carried and the
/// kind of network each belongs to, the whitelisted proxy headers, and the server's clock and
/// version. Never stored.
#[utoipa::path(
    get,
    path = "/api/debug/request",
    tag = "meta",
    responses((status = 200, description = "The request as the server saw it", body = RequestEcho))
)]
#[tracing::instrument(name = META_DEBUG_REQUEST, level = "debug", skip_all)]
pub async fn request(request: Request) -> Json<RequestEcho> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| *addr);
    Json(RequestEcho::of(
        request.version(),
        request.headers(),
        peer,
        Utc::now(),
    ))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/debug/request", get(request))
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::http::HeaderValue;

    fn class(ip: &str) -> NetworkClass {
        NetworkClass::of(ip.parse().expect("a valid address"))
    }

    #[test]
    fn the_tailnet_range_is_tailscale_and_its_neighbours_are_not() {
        assert_eq!(class("100.64.0.0"), NetworkClass::Tailscale);
        assert_eq!(class("100.64.12.34"), NetworkClass::Tailscale);
        assert_eq!(class("100.127.255.255"), NetworkClass::Tailscale);
        // Either side of the /10.
        assert_eq!(class("100.63.255.255"), NetworkClass::Public);
        assert_eq!(class("100.128.0.0"), NetworkClass::Public);
        // Tailscale's IPv6 prefix, which sits inside the unique-local range.
        assert_eq!(class("fd7a:115c:a1e0::1"), NetworkClass::Tailscale);
        assert_eq!(class("fd7a:115c:a1e1::1"), NetworkClass::Lan);
    }

    #[test]
    fn private_and_link_local_ranges_are_the_lan() {
        for ip in [
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.20",
            "169.254.10.10",
            "fd00::1",
            "fe80::1",
        ] {
            assert_eq!(class(ip), NetworkClass::Lan, "{ip}");
        }
        // Just outside 172.16.0.0/12.
        assert_eq!(class("172.15.255.255"), NetworkClass::Public);
        assert_eq!(class("172.32.0.1"), NetworkClass::Public);
    }

    #[test]
    fn loopback_is_its_own_class() {
        assert_eq!(class("127.0.0.1"), NetworkClass::Loopback);
        assert_eq!(class("127.8.9.10"), NetworkClass::Loopback);
        assert_eq!(class("::1"), NetworkClass::Loopback);
    }

    #[test]
    fn everything_else_is_the_public_internet() {
        for ip in ["192.0.2.1", "198.51.100.7", "203.0.113.200", "2001:db8::1"] {
            assert_eq!(class(ip), NetworkClass::Public, "{ip}");
        }
    }

    #[test]
    fn an_ipv4_mapped_address_is_classified_as_its_ipv4_address() {
        assert_eq!(class("::ffff:100.64.1.2"), NetworkClass::Tailscale);
        assert_eq!(class("::ffff:192.168.1.1"), NetworkClass::Lan);
        assert_eq!(class("::ffff:127.0.0.1"), NetworkClass::Loopback);
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn echo(pairs: &[(&'static str, &'static str)], peer: Option<&str>) -> RequestEcho {
        RequestEcho::of(
            Version::HTTP_11,
            &headers(pairs),
            peer.map(|p| p.parse().expect("a valid socket address")),
            Utc::now(),
        )
    }

    #[test]
    fn only_whitelisted_headers_are_echoed() {
        let echo = echo(
            &[
                ("cookie", "session=secret"),
                ("authorization", "Bearer secret"),
                ("cf-access-jwt-assertion", "secret"),
                ("cf-access-client-secret", "secret"),
                ("x-api-key", "secret"),
                ("user-agent", "test-agent"),
                ("x-forwarded-for", "100.64.1.2"),
                ("cf-ray", "0000000000000000-AKL"),
                ("cf-ipcountry", "NZ"),
                ("host", "sure.example"),
            ],
            Some("127.0.0.1:50000"),
        );
        let names: Vec<&str> = echo.headers.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "host",
                "x-forwarded-for",
                "user-agent",
                "cf-ipcountry",
                "cf-ray"
            ]
        );
        assert!(
            echo.headers.iter().all(|h| !h.value.contains("secret")),
            "{:?}",
            echo.headers
        );
    }

    #[test]
    fn addresses_are_listed_in_the_rate_limiters_order_with_the_peer_last() {
        let echo = echo(
            &[
                ("x-real-ip", "100.64.1.2"),
                ("x-forwarded-for", "198.51.100.7, 100.64.1.2"),
                ("cf-connecting-ip", "203.0.113.9"),
            ],
            Some("172.18.0.2:41234"),
        );
        let seen: Vec<(AddressSource, &str, NetworkClass)> = echo
            .addresses
            .iter()
            .map(|a| (a.source, a.ip.as_str(), a.network))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    AddressSource::CfConnectingIp,
                    "203.0.113.9",
                    NetworkClass::Public
                ),
                (
                    AddressSource::XForwardedFor,
                    "198.51.100.7",
                    NetworkClass::Public
                ),
                (
                    AddressSource::XForwardedFor,
                    "100.64.1.2",
                    NetworkClass::Tailscale
                ),
                (
                    AddressSource::XRealIp,
                    "100.64.1.2",
                    NetworkClass::Tailscale
                ),
                (AddressSource::Peer, "172.18.0.2", NetworkClass::Lan),
            ]
        );
        assert_eq!(echo.client.as_ref(), echo.addresses.first());
    }

    #[test]
    fn a_value_that_is_not_an_address_is_skipped_and_ports_are_dropped() {
        let echo = echo(
            &[
                (
                    "x-forwarded-for",
                    "unknown, 192.0.2.4:443, [2001:db8::5]:8443",
                ),
                ("x-real-ip", "not-an-ip"),
            ],
            None,
        );
        let ips: Vec<&str> = echo.addresses.iter().map(|a| a.ip.as_str()).collect();
        assert_eq!(ips, ["192.0.2.4", "2001:db8::5"]);
    }

    #[test]
    fn with_no_proxy_headers_the_client_is_the_peer() {
        let echo = echo(&[], Some("127.0.0.1:50000"));
        let client = echo.client.expect("the peer is always known");
        assert_eq!(client.source, AddressSource::Peer);
        assert_eq!(client.network, NetworkClass::Loopback);
        assert_eq!(echo.http_version, "HTTP/1.1");
    }
}
