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
//! # Runtime notes
//! - no Tokio types are exposed; callers drive the returned futures on their
//!   own executor;
//! - every public future is `Send`;
//! - `tracing` is used for diagnostics and no global subscriber is installed.

mod api;
mod connector;
mod control;
mod edge;
mod error;
mod event;
mod h2;
mod origin;
mod quic;
mod roots;
mod run;
mod serve;
mod tunnel;

pub use connector::{EdgeConnector, EdgeOptions, Transport, default_config_json};
pub use error::Error;
pub use origin::{
    Body, Duplex, HttpOrigin, HttpOriginDyn, Origin, ReadHalf, Request, Response, TcpOrigin,
    TcpOriginDyn, WebSocketConnection, WebSocketOrigin, WebSocketOriginDyn, WriteHalf,
};
pub use run::{RunOptions, run_quick_tunnel};
pub use tunnel::{NamedTunnel, QuickTunnel, QuickTunnelOptions, Tunnel, create_quick_tunnel};
#[cfg(test)]
mod loopback_test;
