//! Cap'n Proto wire schemas and a minimal RPC client for Cloudflare Tunnel
//! registration.
//!
//! This crate is the only crate allowed to depend on the `capnp` crates. The
//! main `libcfd` crate interacts with tunnel registration exclusively through
//! the typed [`tunnel::TunnelClient`] facade.

pub mod error;
pub mod io;
pub mod quic;
pub mod rpc;
pub mod tunnel;

pub mod rpc_capnp {
    #![allow(clippy::all)]
    #![allow(unused_imports)]
    include!(concat!(env!("OUT_DIR"), "/rpc_capnp.rs"));
}

pub mod tunnelrpc_capnp {
    #![allow(clippy::all)]
    #![allow(unused_imports)]
    include!(concat!(env!("OUT_DIR"), "/tunnelrpc_capnp.rs"));
}

pub mod quic_metadata_protocol_capnp {
    #![allow(clippy::all)]
    #![allow(unused_imports)]
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub use error::RpcError;
pub use io::AsyncStream;
pub use quic::{
    ConnectRequest, ConnectResponse, ConnectionType, DATA_STREAM_PROTOCOL_SIGNATURE,
    HTTP_HEADER_KEY, HTTP_HOST_KEY, HTTP_METHOD_KEY, HTTP_STATUS_KEY, PROTOCOL_V1,
    RPC_STREAM_PROTOCOL_SIGNATURE,
};
pub use rpc::{Incoming, RpcClient, read_incoming};
pub use tunnel::{
    ClientInfo, ConnectionDetails, ConnectionError, ConnectionOptions, ConnectionResponse,
    RegistrationFailure, TunnelAuth, TunnelClient,
};
