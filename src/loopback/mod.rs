//! End-to-end loopback tests: a mock edge (quiche server) speaking the
//! registration RPC and data-stream protocols, exercised through the real
//! client path over loopback UDP.

#[cfg(feature = "h2-edge")]
mod h2_common;
#[cfg(feature = "h2-edge")]
mod h2_tests;
#[cfg(feature = "h2-edge")]
mod h2_ws_tests;
#[cfg(feature = "quic-edge")]
mod mock_edge;
#[cfg(feature = "quic-edge")]
mod quic_tests;

use crate::tunnel::QuickTunnel;

// Genuine capnp-go replies, byte-identical to libcfd-rpc's verified goldens.
const BOOTSTRAP_RETURN: &str = "000000000b00000000000000010001000300000000000000000000000200010000000000010000000000000000000000000000000000020003000000000000000100000017000000040000000100010001000000000000000000000000000000";
const REGISTER_RETURN: &str = "0000000012000000000000000100010003000000000000000000000002000100010000000100000000000000000000000000000000000200040000000000010025000000070000000000000001000100010000000000000000000000010002000000000000000000050000008200000009000000220000000102030405060708090a0b0c0d0e0f106c687200000000000000000001000100";
const EMPTY_RETURN: &str = "0000000009000000000000000100010003000000000000000000000002000100020000000100000000000000000000000000000000000200000000000000000001000000070000000000000001000100";

fn hex(encoded: &str) -> Vec<u8> {
    (0..encoded.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&encoded[i..i + 2], 16).unwrap())
        .collect()
}

fn make_tunnel() -> QuickTunnel {
    QuickTunnel {
        tunnel_id: "550e8400-e29b-41d4-a716-446655440000".into(),
        name: String::new(),
        hostname: "test.trycloudflare.com".into(),
        account_tag: "test-account".into(),
        secret: (1..=16).collect(),
    }
}
