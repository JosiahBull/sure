//! What every fixture in this directory needs, and no more.
//!
//! Each file in `tests/` is its own binary, so anything shared has to live in a subdirectory
//! module compiled into all of them. The bar for landing something here is that *every* test
//! binary uses it: an item only some of them want is dead code in the rest, and a
//! `#![allow(dead_code)]` to silence that would also silence a helper that quietly lost its
//! last caller.

use std::net::SocketAddr;

/// Bind ephemerally and let the OS pick.
///
/// `ClusterHandle::run` reports `local_addr()`, so port zero is answered with the port actually
/// bound — asking the OS for a free port and then binding it in a second step is a race that
/// buys nothing, and one that bites hardest here: every fixture stands up a cluster, and several
/// stand up two at once.
pub fn ephemeral() -> SocketAddr {
    "127.0.0.1:0".parse().expect("loopback literal parses")
}
