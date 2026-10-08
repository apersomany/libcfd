use thiserror::Error;

/// Errors surfaced by the libcfd public API.
///
/// The crate-level error composes the per-level `tunnel::Error`,
/// `edge::Error`, and `origin::Error` types; match on the inner variant
/// to handle a specific failure.
#[derive(Debug, Error)]
pub enum Error {
    /// A tunnel identity or the quick tunnel HTTP API failed.
    #[cfg(any_tunnel)]
    #[error(transparent)]
    Tunnel(#[from] crate::tunnel::Error),
    /// Edge discovery, connection, registration, or serving failed.
    #[cfg(edge_conn)]
    #[error(transparent)]
    Edge(#[from] crate::edge::Error),
    /// An origin handler or its byte stream failed.
    #[error(transparent)]
    Origin(#[from] crate::origin::Error),
}

/// Maps a bare string to an origin-handler failure, so handlers can write
/// `Err("message".into())`.
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Origin(crate::origin::Error::Handler(message))
    }
}

/// I/O failures in the edge path surface as `Error::Edge(edge::Error::Io)`.
#[cfg(edge_conn)]
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Edge(error.into())
    }
}

/// RPC failures on the control stream surface as
/// `Error::Edge(edge::Error::Control)`.
#[cfg(edge_conn)]
impl From<libcfd_rpc::RpcError> for Error {
    fn from(error: libcfd_rpc::RpcError) -> Self {
        Self::Edge(error.into())
    }
}

#[cfg(feature = "quic-edge-quiche")]
impl From<boring::error::ErrorStack> for Error {
    fn from(error: boring::error::ErrorStack) -> Self {
        Self::Edge(error.into())
    }
}

#[cfg(h2_any)]
impl From<h2::Error> for Error {
    fn from(error: h2::Error) -> Self {
        Self::Edge(error.into())
    }
}

#[cfg(feature = "quic-edge-quiche")]
impl From<quiche::Error> for Error {
    fn from(error: quiche::Error) -> Self {
        Self::Edge(error.into())
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Whether retrying this error is pointless (the edge will keep
    /// rejecting the tunnel), mirroring cloudflared's permanent-vs-retryable
    /// registration split.
    #[cfg(edge_conn)]
    pub(crate) fn is_permanent(&self) -> bool {
        matches!(
            self,
            Error::Edge(crate::edge::Error::Registration(
                libcfd_rpc::tunnel::RegistrationFailure::Permanent(_)
            ))
        )
    }

    #[cfg(feature = "quick-tunnel")]
    pub(crate) fn quick_tunnel_api(message: impl Into<String>) -> Self {
        Self::Tunnel(crate::tunnel::Error::QuickTunnelApi(message.into()))
    }

    #[cfg(feature = "quick-tunnel")]
    pub(crate) fn quick_tunnel_response(message: impl Into<String>) -> Self {
        Self::Tunnel(crate::tunnel::Error::QuickTunnelResponse(message.into()))
    }

    #[cfg(feature = "quick-tunnel")]
    pub(crate) fn quick_tunnel_request(error: std::io::Error) -> Self {
        Self::Tunnel(crate::tunnel::Error::QuickTunnelRequest(error))
    }

    #[cfg(feature = "named-tunnel")]
    pub(crate) fn named_tunnel_credentials(message: impl Into<String>) -> Self {
        Self::Tunnel(crate::tunnel::Error::NamedTunnelCredentials(message.into()))
    }

    #[cfg(any_tunnel)]
    pub(crate) fn invalid_tunnel_identifier(message: impl Into<String>) -> Self {
        Self::Tunnel(crate::tunnel::Error::InvalidTunnelIdentifier(
            message.into(),
        ))
    }

    #[cfg(edge_conn)]
    pub(crate) fn edge_discovery(message: impl Into<String>) -> Self {
        Self::Edge(crate::edge::Error::EdgeDiscovery(message.into()))
    }

    #[cfg(quic_any)]
    pub(crate) fn quic(message: impl Into<String>) -> Self {
        Self::Edge(crate::edge::Error::Quic(message.into()))
    }

    #[cfg(edge_conn)]
    pub(crate) fn registration(failure: libcfd_rpc::tunnel::RegistrationFailure) -> Self {
        Self::Edge(crate::edge::Error::Registration(failure))
    }

    #[cfg(edge_conn)]
    pub(crate) fn duplicate_connection(cause: impl Into<String>) -> Self {
        Self::Edge(crate::edge::Error::DuplicateConnection(cause.into()))
    }

    #[cfg(h2_any)]
    pub(crate) fn h2(message: impl Into<String>) -> Self {
        Self::Edge(crate::edge::Error::H2(message.into()))
    }

    #[cfg(quic_any)]
    pub(crate) fn edge_io(error: std::io::Error) -> Self {
        Self::Edge(crate::edge::Error::Io(error))
    }

    #[cfg(feature = "axum-origin")]
    pub(crate) fn origin_handler(message: impl Into<String>) -> Self {
        Self::Origin(crate::origin::Error::Handler(message.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_public_error_path_is_typed_and_displayable() {
        let mut errors: Vec<Error> = Vec::new();
        #[cfg(feature = "quick-tunnel")]
        errors.extend([
            Error::Tunnel(crate::tunnel::Error::QuickTunnelApi("rate limited".into())),
            Error::Tunnel(crate::tunnel::Error::QuickTunnelResponse("bad json".into())),
            Error::Tunnel(crate::tunnel::Error::QuickTunnelRequest(
                std::io::Error::other("dns"),
            )),
        ]);
        #[cfg(feature = "named-tunnel")]
        errors.push(Error::Tunnel(crate::tunnel::Error::NamedTunnelCredentials(
            "missing file".into(),
        )));
        #[cfg(any_tunnel)]
        errors.push(Error::Tunnel(
            crate::tunnel::Error::InvalidTunnelIdentifier("not a uuid".into()),
        ));
        #[cfg(edge_conn)]
        errors.extend([
            Error::Edge(crate::edge::Error::EdgeDiscovery("no srv records".into())),
            Error::Edge(crate::edge::Error::Registration(
                libcfd_rpc::tunnel::RegistrationFailure::Permanent("blocked".into()),
            )),
            Error::Edge(crate::edge::Error::DuplicateConnection("EDUPCONN".into())),
            Error::Edge(crate::edge::Error::Control(libcfd_rpc::RpcError::Eof)),
            Error::Edge(crate::edge::Error::Io(std::io::Error::other("io"))),
        ]);
        #[cfg(quic_any)]
        errors.extend([
            Error::Edge(crate::edge::Error::Quic("handshake failed".into())),
            Error::Edge(crate::edge::Error::Tls("bad certificate".into())),
        ]);
        #[cfg(h2_any)]
        errors.push(Error::Edge(crate::edge::Error::H2(
            "connection reset".into(),
        )));
        errors.extend([
            Error::Origin(crate::origin::Error::Handler("handler failed".into())),
            Error::Origin(crate::origin::Error::Io(std::io::Error::other("io"))),
        ]);
        for error in errors {
            assert!(!error.to_string().is_empty());
        }
    }

    #[cfg(edge_conn)]
    #[test]
    fn registration_failure_classifies_permanent() {
        let retryable = libcfd_rpc::tunnel::RegistrationFailure::Retryable {
            cause: "busy".into(),
            retry_after: 1_000,
        };
        let permanent = libcfd_rpc::tunnel::RegistrationFailure::Permanent("blocked".into());
        assert!(!Error::Edge(crate::edge::Error::Registration(retryable)).is_permanent());
        assert!(Error::Edge(crate::edge::Error::Registration(permanent)).is_permanent());
    }
}
