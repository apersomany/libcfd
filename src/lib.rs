#![warn(missing_docs)]

//! `libcfd` is a library that connects to the Cloudflare Tunnel edge and
//! serves origin traffic, without imposing a particular async runtime on its
//! users.
//!
//! # Quick tunnels over QUIC
//!
//! [`create_quick_tunnel`] requests a tunnel from the trycloudflare.com
//! service, and [`run_quick_tunnel`] runs it end to end with an HTTP-only
//! origin: edge discovery, QUIC connection, registration, and request
//! serving.
//!
//! # Named tunnels and transports
//!
//! [`EdgeConnector`] is the full entry point: it accepts any [`Tunnel`]
//! (quick or [`NamedTunnel`] loaded from a credentials file), an [`Origin`]
//! with HTTP, websocket and TCP handlers, and a [`Transport`] selection
//! (QUIC, HTTP/2, or auto with QUIC-to-HTTP/2 fallback). On connection loss
//! it reconnects with exponential backoff.//!
//! # Feature gates
//!
//! - `quick-tunnel`: the quick tunnel HTTP API client and [`QuickTunnel`]
//!   type;
//! - `named-tunnel`: [`NamedTunnel`] and the credentials-file loader;
//! - `quic-edge`: the QUIC edge transport. Defaults to the quinn backend
//!   (pure-Rust rustls/ring); enable `quic-edge-quiche` to use quiche
//!   (BoringSSL) instead — the two backends are mutually exclusive and
//!   quiche wins when both are enabled;
//! - `h2-edge`: the HTTP/2 edge transport.
//!
//! All four are enabled by default. Transports can be disabled to slim the
//! dependency tree; the [`Transport`] selection only offers enabled
//! transports. A transport feature without a tunnel feature still compiles
//! (the tunnel-agnostic types remain), but no [`EdgeConnector`] entry point
//! is available for that combination.
//!
//! The QUIC transport implements RFC 9000 (version 1). cloudflared also
//! offers QUIC version 2; quiche 0.29 does not support it yet, so libcfd
//! falls back to HTTP/2 if the edge ever stops serving v1. The quinn backend
//! needs only a C compiler (`ring`); the quiche backend additionally builds
//! BoringSSL with cmake and libclang.
//!
//! # Runtime notes
//! - no Tokio types are exposed; callers drive the returned futures on a
//!   Tokio runtime (execution uses Tokio internally);
//! - every public future is `Send`;
//! - all public entry points return the typed [`Error`] (thiserror); the RPC
//!   crate exposes its own typed [`libcfd_rpc::RpcError`] and
//!   `RegistrationFailure`;
//! - `tracing` is used for diagnostics and no global subscriber is installed.

// The edge_conn cfg (any tunnel + any edge transport) is emitted by build.rs; transports compile only with a tunnel feature so every feature combination stays buildable.
#[cfg(edge_conn)]
pub mod edge;
mod error;
pub mod origin;
#[cfg(all(feature = "quick-tunnel", quic_any))]
mod run;
#[cfg(any_tunnel)]
pub mod tunnel;

#[cfg(edge_conn)]
pub use edge::{
    EdgeConnector, EdgeOptions, RemoteConfiguration, Transport, default_configuration_json,
};
pub use error::Error;
#[cfg(feature = "axum-origin")]
pub use origin::axum::AxumOrigin;
pub use origin::{
    Body, HttpOrigin, HttpResponder, Origin, ReadHalf, Request, Response, Stream, StreamOrigin,
    StreamResponder, TcpResponder, WebSocketConnection, WebSocketResponder, WriteHalf,
    websocket_accept,
};
#[cfg(all(feature = "quick-tunnel", quic_any))]
pub use run::{RunOptions, run_quick_tunnel};
#[cfg(feature = "named-tunnel")]
pub use tunnel::NamedTunnel;
#[cfg(any_tunnel)]
pub use tunnel::Tunnel;
#[cfg(feature = "quick-tunnel")]
pub use tunnel::{QuickTunnel, QuickTunnelOptions, create_quick_tunnel};
