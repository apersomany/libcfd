use thiserror::Error;

/// Errors surfaced by the Cap'n Proto RPC layer.
#[derive(Debug, Error)]
pub enum RpcError {
    /// The stream ended while a full RPC message was expected.
    #[error("rpc stream ended unexpectedly")]
    Eof,
    /// Invalid or malformed Cap'n Proto framing or message content.
    #[error("rpc protocol error: {0}")]
    Protocol(String),
    /// The peer aborted the RPC connection.
    #[error("rpc aborted by peer (type {error_type}): {reason}")]
    Abort {
        /// The abort reason reported by the peer.
        reason: String,
        /// The abort type code reported by the peer.
        error_type: u16,
    },
    /// The peer returned an exception for a call we made.
    #[error("rpc call failed remotely: {0}")]
    RemoteCall(String),
    /// The transport failed while reading or writing.
    #[error("rpc transport error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<capnp::Error> for RpcError {
    fn from(err: capnp::Error) -> Self {
        Self::Protocol(err.to_string())
    }
}

impl From<capnp::NotInSchema> for RpcError {
    fn from(err: capnp::NotInSchema) -> Self {
        Self::Protocol(format!("value not in schema: {err:?}"))
    }
}

impl From<std::str::Utf8Error> for RpcError {
    fn from(err: std::str::Utf8Error) -> Self {
        Self::Protocol(format!("invalid utf-8 in message: {err}"))
    }
}

pub(crate) type Result<T> = std::result::Result<T, RpcError>;
