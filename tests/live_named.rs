//! Live-edge named tunnel tests.
//!
//! These are ignored by default and run only on demand via
//! `scripts/live-test.sh`, which requires `NAMED_TUNNEL_TOKEN` (read from
//! the environment or `tests/state/named-token.txt`). The token is
//! normalized into `tests/state/named_tunnel.json`, loaded through the
//! credentials-file path, and the edge's remotely-managed configuration
//! supplies the routed hostname used for the public request. The raw token
//! is never stored in generated state.
//!
//! The websocket and TCP tests are independent of the HTTP tests so an
//! unavailable route produces a clear prerequisite failure instead of a
//! confusing assertion. Live tests must run with `--test-threads=1`.

#![cfg(all(feature = "named-tunnel", feature = "quick-tunnel"))]

mod support;

#[cfg(edge_conn)]
use libcfd::Transport;
#[cfg(edge_conn)]
use support::{init_logging, run};

/// Named tunnel over the quinn QUIC backend with a remotely-managed
/// configuration: verifies the callback supplies routed hostnames, a public
/// request to the first hostname reaches the origin, and the run shuts down
/// cleanly.
#[cfg(quic_quinn)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_named_quic_quinn_remote_config_serves_http() {
    init_logging();
    run::named_http_live_test(Transport::Quic, "named-quinn")
        .await
        .expect("named tunnel over quinn should serve http");
}

/// Named tunnel over HTTP/2 with a remotely-managed configuration.
#[cfg(feature = "h2-edge")]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_named_h2_remote_config_serves_http() {
    init_logging();
    run::named_http_live_test(Transport::H2, "named-h2")
        .await
        .expect("named tunnel over http/2 should serve http");
}

/// Named tunnel over the quiche QUIC backend (when enabled).
#[cfg(quic_quiche)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_named_quic_quiche_remote_config_serves_http() {
    init_logging();
    run::named_http_live_test(Transport::Quic, "named-quiche")
        .await
        .expect("named tunnel over quiche should serve http");
}

/// Named tunnel websocket round trip over the quinn QUIC backend. Requires
/// a routed hostname whose ingress service accepts websocket upgrades.
#[cfg(quic_quinn)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_named_quic_quinn_websocket() {
    init_logging();
    run::named_ws_live_test(Transport::Quic, "named-quinn")
        .await
        .expect("named tunnel should serve a websocket echo");
}

/// Named tunnel raw TCP route round trip over the quinn QUIC backend. The
/// route hostname is discovered from the remote configuration's `tcp://`
/// ingress service; Cloudflare carries the route's bytes as a websocket
/// connection, which the tunnel's websocket origin handler serves.
#[cfg(quic_quinn)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_named_quic_quinn_tcp() {
    init_logging();
    run::named_tcp_live_test(Transport::Quic, "named-quinn")
        .await
        .expect("named tunnel should serve bytes through its tcp route");
}
