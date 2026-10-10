//! QUIC connection to the Cloudflare edge.
//!
//! Two backends are available, selected by feature:
//!
//! - `quic-edge-quinn` (default, implied by `quic-edge`): quinn with the
//!   pure-Rust rustls/ring crypto provider;
//! - `quic-edge-quiche`: quiche with its BoringSSL backend.
//!
//! Feature flags may coexist, but only one backend is selected: enabling
//! `quic-edge-quiche` alongside `quic-edge` or `quic-edge-quinn` selects
//! quiche (see the crate's build.rs), including with `--all-features`.

pub(crate) mod serve;

#[cfg(feature = "quic-edge-quiche")]
mod quiche;
#[cfg(quic_quinn)]
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
