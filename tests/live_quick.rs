//! Live-edge quick tunnel tests.
//!
//! These talk to the real Cloudflare edge and the trycloudflare.com API, so
//! they are ignored by default and only run on demand:
//!
//! ```text
//! scripts/live-test.sh
//! ```
//!
//! Credentials are cached in `tests/state/quick_tunnel.json` (gitignored)
//! and reused across runs; a cached tunnel that fails to serve is replaced
//! once. The secret is never printed or logged. Live tests must run with a
//! single test thread (`--test-threads=1`) because they reuse one tunnel
//! identity.

#![cfg(feature = "quick-tunnel")]

mod support;

#[cfg(edge_conn)]
use libcfd::Transport;
#[cfg(edge_conn)]
use support::{init_logging, run};

/// Quick tunnel over the quinn QUIC backend with an HTTP origin: verifies
/// the public HTTPS status, response body, request path, origin invocation,
/// startup polling, and bounded shutdown.
#[cfg(quic_quinn)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_quick_quic_quinn_serves_http() {
    init_logging();
    run::quick_http_live_test(Transport::Quic, "quick-quinn", "/hello")
        .await
        .expect("quick tunnel over quinn should serve http");
}

/// Quick tunnel over HTTP/2 with an HTTP origin.
#[cfg(feature = "h2-edge")]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_quick_h2_serves_http() {
    init_logging();
    run::quick_http_live_test(Transport::H2, "quick-h2", "/hello")
        .await
        .expect("quick tunnel over http/2 should serve http");
}

/// Quick tunnel over the quiche QUIC backend with an HTTP origin (when the
/// quiche backend is enabled).
#[cfg(quic_quiche)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_quick_quic_quiche_serves_http() {
    init_logging();
    run::quick_http_live_test(Transport::Quic, "quick-quiche", "/hello")
        .await
        .expect("quick tunnel over quiche should serve http");
}

/// Quick tunnel websocket round trip over the quinn QUIC backend. The
/// catch-all quick tunnel route supports websocket upgrades.
#[cfg(quic_quinn)]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_quick_quic_quinn_serves_websocket() {
    init_logging();
    run::quick_ws_live_test(Transport::Quic, "quick-quinn")
        .await
        .expect("quick tunnel should serve a websocket echo");
}
