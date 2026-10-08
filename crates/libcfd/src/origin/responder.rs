//! The responder objects the transports hand to origin handlers.

#[cfg(edge_conn)]
use tokio::sync::oneshot;

#[cfg(edge_conn)]
use crate::origin::http::body::Response;
#[cfg(edge_conn)]
use crate::origin::stream::{Stream, WebSocketConnection};

/// Answers one HTTP request with a response or a failure.
///
/// Transports hand an `HttpResponder` to every
/// [`HttpOrigin`](crate::HttpOrigin) call. `send` delivers the
/// [`Response`] to the edge; `fail` writes the message back as an error
/// response. Handlers that need to await origin I/O may move the responder
/// into a spawned task and respond when the work completes. The responder
/// is consumed by responding; dropping it without responding surfaces as a
/// "handler produced no response" error to the edge.
pub struct HttpResponder {
    #[cfg(edge_conn)]
    tx: oneshot::Sender<Result<Response, String>>,
}

impl HttpResponder {
    /// Creates the per-request responder and receiver pair for a transport.
    #[cfg(edge_conn)]
    pub(crate) fn channel() -> (Self, oneshot::Receiver<Result<Response, String>>) {
        let (tx, rx) = oneshot::channel();
        (Self { tx }, rx)
    }

    /// Sends the HTTP response to the edge.
    #[cfg(edge_conn)]
    pub fn send(self, response: Response) {
        let _ = self.tx.send(Ok(response));
    }

    /// Fails the request; the message is sent back to the edge as an error
    /// response.
    #[cfg(edge_conn)]
    pub fn fail(self, message: impl Into<String>) {
        let _ = self.tx.send(Err(message.into()));
    }
}

/// Answers one websocket stream with the handshake and origin stream.
///
/// Transports hand a `WebSocketResponder` to every
/// [`StreamOrigin<WebSocketResponder>`](crate::StreamOrigin) call.
/// `upgrade` delivers the [`WebSocketConnection`] (the 101 response headers
/// plus the origin byte stream) to the edge; `fail` writes the message back
/// as an error response.
pub struct WebSocketResponder {
    #[cfg(edge_conn)]
    tx: oneshot::Sender<Result<WebSocketConnection, String>>,
}

impl WebSocketResponder {
    /// Creates the per-request responder and receiver pair for a transport.
    #[cfg(edge_conn)]
    pub(crate) fn channel() -> (Self, oneshot::Receiver<Result<WebSocketConnection, String>>) {
        let (tx, rx) = oneshot::channel();
        (Self { tx }, rx)
    }

    /// Accepts the websocket upgrade: the handshake response headers plus
    /// the origin byte stream to pump with the edge.
    #[cfg(edge_conn)]
    pub fn upgrade(self, connection: WebSocketConnection) {
        let _ = self.tx.send(Ok(connection));
    }

    /// Fails the request; the message is sent back to the edge as an error
    /// response.
    #[cfg(edge_conn)]
    pub fn fail(self, message: impl Into<String>) {
        let _ = self.tx.send(Err(message.into()));
    }
}

/// Answers one raw TCP stream with the origin stream.
///
/// Transports hand a `TcpResponder` to every
/// [`StreamOrigin<TcpResponder>`](crate::StreamOrigin) call. `stream`
/// delivers the byte stream to pump with the edge; the transport owns the
/// proxy acknowledgement (a bare ack over QUIC, a synthesized 101 over
/// HTTP/2). `fail` writes the message back as an error response.
pub struct TcpResponder {
    #[cfg(edge_conn)]
    tx: oneshot::Sender<Result<Stream, String>>,
}

impl TcpResponder {
    /// Creates the per-request responder and receiver pair for a transport.
    #[cfg(edge_conn)]
    pub(crate) fn channel() -> (Self, oneshot::Receiver<Result<Stream, String>>) {
        let (tx, rx) = oneshot::channel();
        (Self { tx }, rx)
    }

    /// Delivers the origin byte stream to pump with the edge.
    #[cfg(edge_conn)]
    pub fn stream(self, stream: Stream) {
        let _ = self.tx.send(Ok(stream));
    }

    /// Fails the request; the message is sent back to the edge as an error
    /// response.
    #[cfg(edge_conn)]
    pub fn fail(self, message: impl Into<String>) {
        let _ = self.tx.send(Err(message.into()));
    }
}

/// The responder family [`StreamOrigin`](crate::StreamOrigin) accepts:
/// websocket and TCP streams.
pub trait StreamResponder: Send {}

impl StreamResponder for WebSocketResponder {}
impl StreamResponder for TcpResponder {}

/// Waits for the origin handler's outcome, folding handler failures and a
/// dropped responder into an error message.
#[cfg(edge_conn)]
pub(crate) async fn wait_outcome<T>(
    receiver: oneshot::Receiver<Result<T, String>>,
) -> Result<T, String> {
    match receiver.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(message)) => Err(message),
        Err(_) => Err("origin handler produced no response".to_string()),
    }
}

#[cfg(all(test, edge_conn))]
mod tests {
    use super::*;
    use crate::origin::http::body::Response;

    #[tokio::test]
    async fn dropped_http_responder_surfaces_no_response_error() {
        let (responder, receiver) = HttpResponder::channel();
        drop(responder);
        assert_eq!(
            wait_outcome(receiver).await.unwrap_err(),
            "origin handler produced no response"
        );
    }

    #[tokio::test]
    async fn failed_http_responder_surfaces_handler_message() {
        let (responder, receiver) = HttpResponder::channel();
        responder.fail("origin down");
        assert_eq!(wait_outcome(receiver).await.unwrap_err(), "origin down");
    }

    #[tokio::test]
    async fn dropped_websocket_responder_surfaces_no_response_error() {
        let (responder, receiver) = WebSocketResponder::channel();
        drop(responder);
        let outcome = wait_outcome(receiver).await;
        match outcome {
            Err(message) => assert_eq!(message, "origin handler produced no response"),
            Ok(_) => panic!("expected a dropped-responder error"),
        }
    }

    #[tokio::test]
    async fn dropped_tcp_responder_surfaces_no_response_error() {
        let (responder, receiver) = TcpResponder::channel();
        drop(responder);
        let outcome = wait_outcome(receiver).await;
        match outcome {
            Err(message) => assert_eq!(message, "origin handler produced no response"),
            Ok(_) => panic!("expected a dropped-responder error"),
        }
    }

    #[tokio::test]
    async fn response_surfaces_through_http_responder() {
        let (responder, receiver) = HttpResponder::channel();
        responder.send(Response::new(
            http::StatusCode::OK,
            http::HeaderMap::new(),
            crate::origin::http::body::Body::empty(),
        ));
        let response = wait_outcome(receiver).await.unwrap();
        assert_eq!(response.status, http::StatusCode::OK);
    }
}
