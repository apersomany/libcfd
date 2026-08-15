//! The [`TcpOrigin`] trait consumers implement for raw TCP proxying.

use crate::origin::http::body::Request;
use crate::origin::responder::Responder;

/// Handles raw TCP connections from the edge.
///
/// `connect` establishes the consumer-side connection (consumers own origin
/// I/O) and writes the outcome into `respond`: the byte stream to pump with
/// the edge ([`stream`](Responder::stream)), or a failure
/// ([`fail`](Responder::fail)). The destination host is carried in
/// `request.uri` (`http://<host>[:port]`).
///
/// `connect` is synchronous; consumers that need to await origin I/O spawn
/// a task that calls `stream` or `fail` when the work completes.
pub trait TcpOrigin: Send + Sync {
    /// Establishes the consumer-side connection and writes the byte stream.
    fn connect(&self, request: Request, respond: Responder);
}
