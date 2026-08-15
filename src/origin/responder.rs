//! The [`Responder`] objects the transports hand to origin handlers.

#[cfg(edge_conn)]
use tokio::sync::mpsc;

#[cfg(edge_conn)]
use crate::origin::duplex::{Duplex, WebSocketConnection};
#[cfg(edge_conn)]
use crate::origin::http::body::Response;

/// The outcome an origin handler produces for one request.
#[cfg(edge_conn)]
pub(crate) enum OriginEvent {
    /// An HTTP response to send to the edge.
    Response(Response),
    /// An accepted websocket upgrade.
    WebSocket(WebSocketConnection),
    /// A raw byte stream to pump with the edge (TCP proxy).
    Stream(Duplex),
    /// The handler failed; the message is sent back to the edge.
    Fail(String),
}

/// Writes the outcome of one request back to the edge.
///
/// Transports hand a `Responder` to every origin handler call. Handlers
/// respond by calling [`send`](Responder::send), [`accept`](Responder::accept),
/// [`stream`](Responder::stream) or [`fail`](Responder::fail); the transport
/// delivers the outcome to the edge. Handlers that need to await origin I/O
/// may move the `Responder` into a spawned task and respond when the work
/// completes.
pub struct Responder {
    #[cfg(edge_conn)]
    tx: mpsc::UnboundedSender<OriginEvent>,
}

impl Responder {
    /// Creates the per-request responder and receiver pair for a transport.
    #[cfg(edge_conn)]
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<OriginEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Sends an HTTP response to the edge, for [`HttpOrigin`](crate::HttpOrigin).
    #[cfg(edge_conn)]
    pub fn send(&self, response: Response) {
        let _ = self.tx.send(OriginEvent::Response(response));
    }

    /// Accepts a websocket upgrade, for [`WebSocketOrigin`](crate::WebSocketOrigin).
    #[cfg(edge_conn)]
    pub fn accept(&self, connection: WebSocketConnection) {
        let _ = self.tx.send(OriginEvent::WebSocket(connection));
    }

    /// Delivers a raw byte stream to pump with the edge, for
    /// [`TcpOrigin`](crate::TcpOrigin).
    #[cfg(edge_conn)]
    pub fn stream(&self, duplex: Duplex) {
        let _ = self.tx.send(OriginEvent::Stream(duplex));
    }

    /// Fails the request; the message is sent back to the edge as an error
    /// response.
    #[cfg(edge_conn)]
    pub fn fail(&self, message: impl Into<String>) {
        let _ = self.tx.send(OriginEvent::Fail(message.into()));
    }
}

/// Waits for the origin handler's outcome, folding handler failures and a
/// handler that produced nothing into an error message.
#[cfg(edge_conn)]
pub(crate) async fn wait_event(
    events: &mut mpsc::UnboundedReceiver<OriginEvent>,
) -> Result<OriginEvent, String> {
    match events.recv().await {
        Some(OriginEvent::Fail(message)) => Err(message),
        Some(event) => Ok(event),
        None => Err("origin handler produced no response".to_string()),
    }
}
