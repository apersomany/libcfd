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
    /// A named tunnel credentials file could not be loaded or parsed.
    #[error("named tunnel credentials error: {0}")]
    NamedTunnelCredentials(String),
    /// Edge discovery (DNS SRV or fallback resolution) failed.
    #[error("edge discovery failed: {0}")]
    EdgeDiscovery(String),
    /// The QUIC connection to the edge failed.
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
    #[error("http2 edge connection failed: {0}")]
    H2(String),
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
    /// A tunnel id could not be parsed as a UUID.
    #[error("invalid tunnel id: {0}")]
    InvalidTunnelId(String),
}

impl From<String> for Error {
    fn from(msg: String) -> Self {
        Self::Origin(msg)
    }
}

#[cfg(feature = "quic-edge")]
impl From<boring::error::ErrorStack> for Error {
    fn from(err: boring::error::ErrorStack) -> Self {
        Self::Tls(err.to_string())
    }
}

#[cfg(feature = "quic-edge")]
impl From<quiche::Error> for Error {
    fn from(err: quiche::Error) -> Self {
        Self::Quic(err.to_string())
    }
}

#[cfg(feature = "h2-edge")]
impl From<h2::Error> for Error {
    fn from(err: h2::Error) -> Self {
        Self::H2(err.to_string())
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Whether retrying this error is pointless (the edge will keep
    /// rejecting the tunnel), mirroring cloudflared's permanent-vs-retryable
    /// registration split.
    #[cfg(all(
        any(feature = "quick-tunnel", feature = "named-tunnel"),
        any(feature = "quic-edge", feature = "h2-edge")
    ))]
    pub(crate) fn is_permanent(&self) -> bool {
        matches!(
            self,
            Error::Registration(libcfd_rpc::tunnel::RegistrationFailure::Permanent(_))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_error_path_is_typed_and_displayable() {
        let errors = [
            Error::QuickTunnelApi("rate limited".into()),
            Error::QuickTunnelResponse("bad json".into()),
            Error::QuickTunnelRequest(std::io::Error::other("dns")),
            Error::NamedTunnelCredentials("missing file".into()),
            Error::EdgeDiscovery("no srv records".into()),
            Error::Quic("handshake failed".into()),
            Error::Registration(libcfd_rpc::tunnel::RegistrationFailure::Permanent(
                "blocked".into(),
            )),
            Error::DuplicateConnection("EDUPCONN".into()),
            Error::H2("connection reset".into()),
            Error::Control(libcfd_rpc::RpcError::Eof),
            Error::Tls("bad certificate".into()),
            Error::Origin("handler failed".into()),
            Error::Io(std::io::Error::other("io")),
            Error::InvalidTunnelId("not a uuid".into()),
        ];
        for error in errors {
            let display = error.to_string();
            assert!(!display.is_empty());
        }
    }

    #[cfg(all(
        any(feature = "quick-tunnel", feature = "named-tunnel"),
        any(feature = "quic-edge", feature = "h2-edge")
    ))]
    #[test]
    fn registration_failure_classifies_permanent() {
        let retryable = libcfd_rpc::tunnel::RegistrationFailure::Retryable {
            cause: "busy".into(),
            retry_after: 1_000,
        };
        let permanent = libcfd_rpc::tunnel::RegistrationFailure::Permanent("blocked".into());
        assert!(!Error::Registration(retryable).is_permanent());
        assert!(Error::Registration(permanent).is_permanent());
    }

    #[test]
    fn io_error_converts_from_std() {
        let source = std::io::Error::new(std::io::ErrorKind::TimedOut, "timeout");
        let error: Error = source.into();
        assert!(matches!(error, Error::Io(_)));
    }

    #[test]
    fn rpc_error_converts_into_control_variant() {
        let error: Error = libcfd_rpc::RpcError::Eof.into();
        assert!(matches!(error, Error::Control(_)));
    }
}
