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
//! it reconnects with exponential backoff.
//!
//! # Feature gates
//!
//! - `quick-tunnel`: the quick tunnel HTTP API client and [`QuickTunnel`]
//!   type;
//! - `named-tunnel`: [`NamedTunnel`] and the credentials-file loader;
//! - `quic-edge`: the QUIC edge transport (quiche);
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
//! falls back to HTTP/2 if the edge ever stops serving v1.
//!
//! # Runtime notes
//! - no Tokio types are exposed; callers drive the returned futures on a
//!   Tokio runtime (execution uses Tokio internally);
//! - every public future is `Send`;
//! - all public entry points return the typed [`Error`] (thiserror); the RPC
//!   crate exposes its own typed [`libcfd_rpc::RpcError`] and
//!   `RegistrationFailure`;
//! - `tracing` is used for diagnostics and no global subscriber is installed.

#[cfg(feature = "quick-tunnel")]
mod api;
// The `edge_conn` cfg (any tunnel feature with any edge transport feature) is
// emitted by build.rs; the transport modules below only compile when a tunnel
// feature is present because `h2` and `serve` use `crate::control` and
// `crate::tunnel`, keeping every feature combination buildable.
#[cfg(edge_conn)]
mod connector;
#[cfg(edge_conn)]
mod control;
#[cfg(edge_conn)]
mod edge;
mod error;
#[cfg(edge_conn)]
mod event;
#[cfg(h2_any)]
mod h2;
#[cfg(all(feature = "quick-tunnel", any_edge))]
#[cfg(test)]
mod loopback;
mod origin;
#[cfg(quic_any)]
mod quic;
#[cfg(edge_conn)]
mod roots;
#[cfg(all(feature = "quick-tunnel", feature = "quic-edge"))]
mod run;
#[cfg(quic_any)]
mod serve;
#[cfg(any_tunnel)]
mod tunnel;

#[cfg(edge_conn)]
pub use connector::{EdgeConnector, EdgeOptions, Transport, default_config_json};
pub use error::Error;
#[cfg(feature = "axum-origin")]
pub use origin::axum::AxumOrigin;
pub use origin::{
    Body, Duplex, HttpOrigin, HttpOriginDyn, Origin, ReadHalf, Request, Response, TcpOrigin,
    TcpOriginDyn, WebSocketConnection, WebSocketOrigin, WebSocketOriginDyn, WriteHalf,
    websocket_accept,
};
#[cfg(all(feature = "quick-tunnel", feature = "quic-edge"))]
pub use run::{RunOptions, run_quick_tunnel};
#[cfg(feature = "named-tunnel")]
pub use tunnel::NamedTunnel;
#[cfg(any(feature = "quick-tunnel", feature = "named-tunnel"))]
pub use tunnel::Tunnel;
#[cfg(feature = "quick-tunnel")]
pub use tunnel::{QuickTunnel, QuickTunnelOptions, create_quick_tunnel};
