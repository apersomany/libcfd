use rustls::Certificate;

pub fn cloudflare_ca() -> Certificate {
    Certificate(include_bytes!("cloudflare_ca.der").to_vec())
}

#[allow(unused)]
mod quic_metadata_protocol_capnp {
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub use quic_metadata_protocol_capnp::*;

#[allow(unused)]
mod tunnelrpc_capnp {
    include!(concat!(env!("OUT_DIR"), "/tunnelrpc_capnp.rs"));
}

pub use tunnelrpc_capnp::*;
