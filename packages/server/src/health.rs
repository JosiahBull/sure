//! The container's `HEALTHCHECK`, as a mode of this binary.
//!
//! A minimal runtime image has no shell and no `curl`, so the probe has to be something
//! already in the image. Rather than add a second binary — and with it a second thing to
//! keep patched — the server probes itself: `sure-api --health-check` asks its own
//! `/api/health` and exits 0 or 1.
//!
//! It goes through `reqwest`, which every provider adapter already uses, so this costs the
//! image nothing: the client is linked into the binary whether or not this module exists.
//! Deliberately *not* `reqwest::blocking`, which is a feature this workspace does not enable
//! and which would bring its own background runtime along — `tokio` is already here, and one
//! current-thread runtime for one request is smaller than that.
//!
//! # This must run before the sandbox
//!
//! The Landlock policy permits outbound TCP to 443 and 53 only (see [`crate::sandbox`]), so a
//! probe that ran after it could not connect to the server's own port. `main` therefore
//! branches here as its very first act, before `load_dotenv`, tracing, config or the
//! sandbox — none of which a health check needs anyway.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{bail, Context};

/// The argument that selects this mode.
pub const FLAG: &str = "--health-check";

/// Bounds the whole request. Docker's own `--timeout` (3s in the Dockerfile) would kill a
/// hung check anyway, but only after leaving a process around until it fires; this keeps the
/// failure inside the process that owns it.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Probe the running server. `Ok` is healthy; any error is not.
pub fn probe() -> anyhow::Result<()> {
    let url = format!(
        "http://{}/api/health",
        target(std::env::var("BIND_ADDR").ok().as_deref())?
    );

    // One request, then the process exits — a current-thread runtime is the whole of what
    // that needs, and it starts in microseconds.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building the probe runtime")?
        .block_on(async {
            let response = reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .context("building the probe client")?
                .get(&url)
                .send()
                .await
                .with_context(|| format!("requesting {url}"))?;
            if !response.status().is_success() {
                bail!("health endpoint answered {}", response.status());
            }
            Ok(())
        })
}

/// Where to send the probe, given `BIND_ADDR`.
///
/// A bind address is not a destination: the container binds `0.0.0.0:8080` so that the port
/// is reachable from outside, and connecting to `0.0.0.0` is not meaningful. The port is the
/// part that matters — the probe always talks to loopback, of whichever family was bound.
fn target(bind_addr: Option<&str>) -> anyhow::Result<SocketAddr> {
    let raw = bind_addr.unwrap_or("127.0.0.1:8080");
    let bind: SocketAddr = raw
        .parse()
        .with_context(|| format!("BIND_ADDR {raw:?} is not an address:port"))?;
    let ip = if bind.ip().is_unspecified() {
        match bind.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    } else {
        bind.ip()
    };
    Ok(SocketAddr::new(ip, bind.port()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;

    /// The case the container actually runs: bound to the v4 wildcard, probed on v4 loopback.
    #[test]
    fn a_wildcard_bind_is_probed_on_loopback() {
        let addr = target(Some("0.0.0.0:8080")).unwrap();
        assert_eq!(addr, SocketAddr::from(([127, 0, 0, 1], 8080)));
    }

    #[test]
    fn a_v6_wildcard_stays_on_v6() {
        let addr = target(Some("[::]:9000")).unwrap();
        assert_eq!(addr.ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(addr.port(), 9000);
    }

    /// An explicit address is used as given — a server bound to one interface is probed there.
    #[test]
    fn an_explicit_address_is_left_alone() {
        assert_eq!(
            target(Some("127.0.0.1:1234")).unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 1234))
        );
    }

    #[test]
    fn an_unset_bind_addr_uses_the_same_default_the_server_does() {
        assert_eq!(
            target(None).unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 8080))
        );
    }

    #[test]
    fn a_malformed_bind_addr_says_so_rather_than_probing_something_else() {
        let err = target(Some("http://localhost:8080"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("address:port"), "{err}");
    }

    /// Answer one request with `status`, then close. Returns the port it is listening on.
    ///
    /// The whole request is drained before answering, not just its first line. Closing a
    /// socket that still has unread bytes in its receive buffer sends an RST rather than a
    /// FIN, and an RST tells the peer's kernel to discard what it has buffered — including
    /// the response just written. (The same mechanism `@sure/api-tests`' `attemptOversized`
    /// exists to work around; here it made a healthy server look unhealthy.)
    fn stub(status: &'static str) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                // Headers end at the first blank line; read to there so nothing is left over.
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) if line == "\r\n" || line == "\n" => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                let _ = socket.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            }
        });
        port
    }

    /// Drives the real [`probe`], so what it sends has to be something a server will answer.
    #[test]
    fn a_200_is_healthy_and_anything_else_is_not() {
        // SAFETY: both cases run sequentially inside this one test, and `probe` reads
        // `BIND_ADDR` once, up front.
        unsafe { std::env::set_var("BIND_ADDR", format!("127.0.0.1:{}", stub("200 OK"))) };
        if let Err(e) = probe() {
            panic!("a healthy server was reported unhealthy: {e:#}");
        }

        unsafe {
            std::env::set_var(
                "BIND_ADDR",
                format!("127.0.0.1:{}", stub("503 Service Unavailable")),
            )
        };
        let err = probe().unwrap_err().to_string();
        assert!(err.contains("503"), "{err}");
    }

    /// Nothing listening is unhealthy, not a panic — the state during startup and after a
    /// crash, which is exactly when Docker is asking.
    #[test]
    fn a_closed_port_is_unhealthy() {
        // Bind and drop, so the port is almost certainly free and nothing is behind it.
        let port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        unsafe { std::env::set_var("BIND_ADDR", format!("127.0.0.1:{port}")) };
        assert!(probe().is_err());
    }
}
