//! Shared support for the libcfd integration test binaries.
//!
//! The live-edge tests (`tests/live_quick.rs`, `tests/live_named.rs`) reuse
//! these helpers: the HTTPS client, the tunnel state managers, the origin
//! handlers, and the run/poll/shutdown scaffolding. Nothing here is exposed
//! by the `libcfd` crate itself.

#![allow(dead_code)]

/// Minimal HTTPS client used to verify public tunnel hostnames.
#[cfg(feature = "quick-tunnel")]
pub mod http;

/// Tunnel state: cached quick-tunnel credentials, named-tunnel token
/// normalization, and the lock that serializes live tunnel runs.
pub mod state;

/// Origin handlers used by the live tests.
#[cfg(edge_conn)]
pub mod origins;

/// Tunnel run scaffolding: start, poll, and shut down a live tunnel.
#[cfg(edge_conn)]
pub mod run;

/// Minimal websocket client used to verify websocket routes.
#[cfg(all(feature = "quick-tunnel", edge_conn))]
pub mod ws;

/// Initializes a tracing subscriber once; safe to call from every test.
pub fn init_logging() {
    let level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(tracing::Level::INFO);
    let _ = tracing_subscriber::fmt().with_max_level(level).try_init();
}

/// How long to keep polling a public hostname for the origin response.
pub const POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
/// How long to wait for a freshly created quick tunnel's hostname to
/// resolve: DNS propagation behind the public hostname is not instant.
pub const DNS_WAIT: std::time::Duration = std::time::Duration::from_secs(90);
/// How long to wait for a named tunnel's remote configuration push.
pub const CONFIG_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a tunnel run may take to shut down after the shutdown signal.
pub const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How long to wait between polls.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
