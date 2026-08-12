//! Errors from edge discovery, connections, registration, and serving.

use thiserror::Error;

/// Errors from edge discovery, connections, registration, and serving.
#[derive(Debug, Error)]
pub enum Error {
    /// Edge discovery (DNS SRV or fallback resolution) failed.
    #[error("edge discovery failed: {0}")]
    EdgeDiscovery(String),
    /// The QUIC connection to the edge failed.
    #[cfg(quic_any)]
    #[error("quic connection failed: {0}")]
    Quic(String),
    /// Registration with the edge was rejected.
    #[error("registration failed: {0}")]
    Registration(#[source] libcfd_rpc::tunnel::RegistrationFailure),
    /// The edge rejected the connection because the tunnel is already
    /// registered elsewhere (cloudflared's `EDUPCONN`).
    #[error("duplicate connection: {0}")]
    DuplicateConnection(String),
    /// The HTTP/2 edge connection failed.
    #[cfg(h2_any)]
    #[error("http2 edge connection failed: {0}")]
    H2(String),
    /// The edge returned an error for the RPC stream.
    #[error("control stream error: {0}")]
    Control(#[from] libcfd_rpc::RpcError),
    /// The TLS configuration for the edge connection could not be built.
    #[cfg(quic_any)]
    #[error("tls configuration failed: {0}")]
    Tls(String),
    /// An underlying I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(quic_any)]
impl From<boring::error::ErrorStack> for Error {
    fn from(err: boring::error::ErrorStack) -> Self {
        Self::Tls(err.to_string())
    }
}

#[cfg(quic_any)]
impl From<quiche::Error> for Error {
    fn from(err: quiche::Error) -> Self {
        Self::Quic(err.to_string())
    }
}

#[cfg(h2_any)]
impl From<h2::Error> for Error {
    fn from(err: h2::Error) -> Self {
        Self::H2(err.to_string())
    }
}
