//! QUIC connection to the Cloudflare edge.
//!
//! Two backends are available, selected by feature:
//!
//! - `quic-edge-quinn` (default, implied by `quic-edge`): quinn with the
//!   pure-Rust rustls/ring crypto provider;
//! - `quic-edge-quiche`: quiche with its BoringSSL backend.
//!
//! The backends are mutually exclusive; enabling `quic-edge-quiche` alongside
//! `quic-edge` selects quiche (see build.rs).

#[cfg(feature = "quic-edge-quiche")]
mod quiche;
#[cfg(feature = "quic-edge-quinn")]
#[cfg_attr(not(quic_quinn), allow(dead_code))]
mod quinn;

#[cfg(quic_quiche)]
pub(crate) use quiche::{QuicConnection, QuicStream};
#[cfg(quic_quinn)]
pub(crate) use quinn::{QuicConnection, QuicStream};

/// TLS server name used for the QUIC edge connection (cloudflared uses the
/// same value).
pub(crate) const EDGE_SNI: &str = "quic.cftunnel.com";
/// ALPN protocol advertised on the QUIC edge connection.
pub(crate) const EDGE_ALPN: &[u8] = b"argotunnel";
