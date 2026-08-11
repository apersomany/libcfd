//! `libcfd` is a library that connects to Cloudflare Tunnel edge and serves
//! origin traffic, without imposing a particular async runtime on its users.
//!
//! The core flow is [`run_quick_tunnel`]: create a quick tunnel (or reuse one
//! from [`create_quick_tunnel`]), discover an edge address, establish a QUIC
//! connection, register the tunnel, and deliver incoming HTTP requests to a
//! consumer-provided [`HttpOrigin`].
//!
//! Public API notes:
//! - no Tokio types are exposed; callers drive the returned futures on their
//!   own executor;
//! - every public future is `Send`;
//! - `tracing` is used for diagnostics and no global subscriber is installed.

mod api;
mod control;
mod edge;
mod error;
mod origin;
mod quic;
mod run;
mod serve;
mod tunnel;

pub use error::Error;
pub use origin::{Body, HttpOrigin, HttpOriginDyn, Request, Response};
pub use run::{RunOptions, default_config_json, run_quick_tunnel};
pub use tunnel::{QuickTunnel, QuickTunnelOptions, create_quick_tunnel};
