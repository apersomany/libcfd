//! `libcfd` is a library that connects to the Cloudflare Tunnel edge and
//! serves origin traffic, without imposing a particular async runtime on its
//! users.
//!
//! The core flow is [`EdgeConnector::run`]: create a quick tunnel (or reuse
//! one from [`create_quick_tunnel`]), or load a [`NamedTunnel`], discover an
//! edge address, establish a QUIC or HTTP/2 connection, register the tunnel,
//! and deliver incoming traffic to consumer-provided origin handlers.
//!
//! [`run_quick_tunnel`] is the convenience API for a quick tunnel with an
//! HTTP-only origin over QUIC.
//!
//! Public API notes:
//! - no Tokio types are exposed; callers drive the returned futures on their
//!   own executor;
//! - every public future is `Send`;
//! - `tracing` is used for diagnostics and no global subscriber is installed.

mod api;
mod connector;
mod control;
mod edge;
mod error;
mod h2;
mod origin;
mod quic;
mod roots;
mod run;
mod serve;
mod shutdown;
mod tunnel;

pub use connector::{EdgeConnector, EdgeOptions, Transport, default_config_json};
pub use error::Error;
pub use origin::{
    Body, Duplex, HttpOrigin, HttpOriginDyn, Origin, Request, Response, TcpOrigin, TcpOriginDyn,
    WebSocketConnection, WebSocketOrigin, WebSocketOriginDyn,
};
pub use run::{RunOptions, run_quick_tunnel};
pub use tunnel::{NamedTunnel, QuickTunnel, QuickTunnelOptions, Tunnel, create_quick_tunnel};
#[cfg(test)]
mod loopback_test;
