use rustls::Certificate;

pub fn cloudflare_ca() -> Certificate {
    Certificate(include_bytes!("cloudflare_ca.der").to_vec())
}

#[allow(unused)]
mod quic_metadata_protocol_capnp;

pub use quic_metadata_protocol_capnp::*;

#[allow(unused)]
mod tunnelrpc_capnp;

pub use tunnelrpc_capnp::*;
