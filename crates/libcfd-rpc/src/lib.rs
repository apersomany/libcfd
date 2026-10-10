#![warn(missing_docs)]

//! Cap'n Proto wire schemas and a minimal RPC client for Cloudflare Tunnel
//! registration.
//!
//! This crate is the only crate allowed to depend on the `capnp` crates. The
//! main `libcfd` crate interacts with tunnel registration exclusively through
//! the typed [`tunnel::TunnelClient`] facade.
//!
//! Wire schemas live in this crate's `schemas/` directory; generated bindings
//! are included from the build output. RPC I/O uses [`io::AsyncStream`]
//! (`futures_io` traits); callers supply and drive the stream without a
//! concrete executor type in this API. The typed [`tunnel::TunnelClient`]
//! futures are `Send`. The generic [`RpcClient::call`] boundary requires
//! `Send` callbacks and decoded output, keeping public call futures `Send`.

/// Server side of the edge's `CloudflaredServer` calls.
pub mod cloudflared;
/// RPC errors.
pub mod error;
/// Stream framing and message I/O.
pub mod io;
/// Plain-Rust types for the QUIC per-stream metadata protocol.
pub mod quic;
/// The minimal Cap'n Proto RPC client.
pub mod rpc;
/// The typed tunnel registration facade.
pub mod tunnel;

pub mod rpc_capnp {
    #![allow(clippy::all)]
    #![allow(unused_imports)]
    #![allow(missing_docs)]
    include!(concat!(env!("OUT_DIR"), "/rpc_capnp.rs"));
}

pub mod tunnelrpc_capnp {
    #![allow(clippy::all)]
    #![allow(unused_imports)]
    #![allow(missing_docs)]
    include!(concat!(env!("OUT_DIR"), "/tunnelrpc_capnp.rs"));
}

pub mod quic_metadata_protocol_capnp {
    #![allow(clippy::all)]
    #![allow(unused_imports)]
    #![allow(missing_docs)]
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub use cloudflared::{
    CloudflaredHandler, RegisterUdpSessionResponse, UpdateConfigurationResponse, serve_cloudflared,
};
pub use error::RpcError;
pub use io::AsyncStream;
pub use quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE,
    HTTP_HEADER_KEY, HTTP_HOST_KEY, HTTP_METHOD_KEY, HTTP_STATUS_KEY, PROTOCOL_V1,
    RPC_STREAM_PROTOCOL_SIGNATURE,
};
pub use rpc::{Incoming, RpcClient, read_incoming, send_exception};
pub use tunnel::{
    ClientInformation, ConnectionDetails, ConnectionError, ConnectionOptions, ConnectionResponse,
    RegistrationFailure, TunnelAuth, TunnelClient,
};
