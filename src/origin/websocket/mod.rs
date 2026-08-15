//! The [`WebSocketOrigin`] trait consumers implement for websocket upgrades.

use crate::origin::http::body::Request;
use crate::origin::responder::Responder;

/// Handles websocket upgrades from the edge.
///
/// `connect` runs the origin-side handshake (the consumer owns all origin
/// I/O) and writes the outcome into `respond`: the response headers the
/// edge should see plus the origin byte stream
/// ([`accept`](Responder::accept)), or a failure ([`fail`](Responder::fail)).
/// The transport sends the response and then pumps bytes in both directions
/// between the edge stream and the origin.
///
/// `connect` is synchronous; consumers that need to await origin I/O spawn
/// a task that calls `accept` or `fail` when the work completes.
pub trait WebSocketOrigin: Send + Sync {
    /// Runs the origin-side websocket handshake and writes the outcome.
    fn connect(&self, request: Request, respond: Responder);
}
