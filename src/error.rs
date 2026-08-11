use thiserror::Error;

/// Errors surfaced by the libcfd public API.
#[derive(Debug, Error)]
pub enum Error {
    /// The quick tunnel HTTP API rejected the request or returned an error.
    #[error("quick tunnel API error: {0}")]
    QuickTunnelApi(String),
    /// The quick tunnel HTTP API response could not be parsed.
    #[error("quick tunnel API response was malformed: {0}")]
    QuickTunnelResponse(String),
    /// The HTTP request to the quick tunnel API failed.
    #[error("quick tunnel API request failed: {0}")]
    QuickTunnelRequest(#[source] std::io::Error),
    /// Edge discovery (DNS SRV or fallback resolution) failed.
    #[error("edge discovery failed: {0}")]
    EdgeDiscovery(String),
    /// The QUIC connection to the edge failed.
    #[error("quic connection failed: {0}")]
    Quic(String),
    /// Registration with the edge was rejected.
    #[error("registration failed: {0}")]
    Registration(#[source] libcfd_rpc::tunnel::RegistrationFailure),
    /// The edge returned an error for the RPC stream.
    #[error("control stream error: {0}")]
    Control(#[from] libcfd_rpc::RpcError),
    /// The TLS configuration for the edge connection could not be built.
    #[error("tls configuration failed: {0}")]
    Tls(String),
    /// The origin handler returned an error.
    #[error("origin handler error: {0}")]
    Origin(String),
    /// An underlying I/O operation failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The tunnel was asked to shut down.
    #[error("tunnel shut down")]
    Shutdown,
}

impl From<String> for Error {
    fn from(msg: String) -> Self {
        Self::Origin(msg)
    }
}

impl From<boring::error::ErrorStack> for Error {
    fn from(err: boring::error::ErrorStack) -> Self {
        Self::Tls(err.to_string())
    }
}

impl From<quiche::Error> for Error {
    fn from(err: quiche::Error) -> Self {
        Self::Quic(err.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
