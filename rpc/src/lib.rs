pub use capnp;
pub use capnp_futures;
pub use capnp_rpc;

mod quic_metadata_protocol_capnp;
mod tunnelrpc_capnp;

pub mod generated {
    pub use crate::quic_metadata_protocol_capnp::*;
    pub use crate::tunnelrpc_capnp::*;
}
