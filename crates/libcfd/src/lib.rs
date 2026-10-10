#![warn(missing_docs)]

//! `libcfd` is a library that connects to the Cloudflare Tunnel edge and
//! serves origin traffic through runtime-neutral public types and `Send`
//! futures. Built-in network execution requires a consumer-provided Tokio
//! runtime with I/O and time enabled; the library does not create one.
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
//! - `quic-edge`: the QUIC edge transport. Defaults to the quinn backend
//!   (pure-Rust rustls/ring); enable `quic-edge-quiche` to use quiche
//!   (BoringSSL) instead. Feature flags may coexist, but only one backend
//!   is selected: quiche takes precedence, including with `--all-features`;
//! - `quic-edge-quinn`: direct selection of the quinn backend;
//! - `h2-edge`: the HTTP/2 edge transport;
//! - `axum-origin`: the optional HTTP-only axum `Router` adapter;
//!
//! `quick-tunnel`, `named-tunnel`, `quic-edge`, and `h2-edge` are enabled
//! by default. Transports can be disabled to slim the dependency tree;
//! the [`Transport`] selection only offers enabled
//! transports. A transport feature without a tunnel feature still compiles
//! (the tunnel-agnostic types remain), but no [`EdgeConnector`] entry point
//! is available for that combination.
//!
//! HTTP/2 fallback requires [`Transport::Auto`] and both transports enabled;
//! QUIC-only runs do not fall back. The quinn backend needs a C compiler
//! (`ring`); the quiche backend additionally builds BoringSSL with cmake and
//! libclang.
//!
//! # Runtime notes
//! - no concrete executor types are exposed; network entry points use
//!   Tokio sockets, timers, and internal tasks and require an active Tokio
//!   runtime with I/O and time enabled;
//! - [`HttpOrigin::handle`] and [`StreamOrigin::connect`] are intentionally
//!   synchronous. Consumers respond immediately or schedule their own
//!   asynchronous work with the owned typed responder. The optional axum
//!   adapter schedules router work with Tokio;
//! - every public future is `Send`;
//! - tunnel creation and runs return the typed [`Error`] (thiserror); the RPC
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
